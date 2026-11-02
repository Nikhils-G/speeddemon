//! Track A (Ingest): for every (UTC hour, endpoint) group in requests.log, the
//! request count, nearest-rank p50/p95/p99 latency and the 5xx error rate.
//!
//! How it is fast:
//! 1. The log is mmap'd, not read. The harness pre-warms it into the page cache,
//!    and pages cached by the host are not charged to our cgroup, so the 239 MB
//!    input costs nothing on the memory axis.
//! 2. One thread per hardware thread parses slices of the file, taken from a shared
//!    counter two at a time (two pairs per thread, so a thread slowed by a busy CPU
//!    does not hold up the rest), one line of each slice in turn, into packed u32
//!    records (endpoint, error flag, latency; the 1.2% of latencies of 7+ digits
//!    go to a side list). Field ends come from one AVX2 space mask, digits are
//!    converted where they lie by multiply-adds, and endpoints are found in one table
//!    shared by the threads, from their length and first and last 8 bytes, and
//!    compared in place, with no allocation or capacity check per line.
//! 3. Records carry no hour: shards write in time order, so the file is made of
//!    runs of lines in one hour (about 1,000), and each slice's parse (its part)
//!    keeps a list of its runs, keyed by the timestamp's first 13 bytes
//!    "YYYY-MM-DDTHH". Sorting those bytes sorts the hours, and they are the start
//!    of the output's hour label, so no calendar arithmetic is needed and any
//!    amount of disorder is handled.
//! 4. Endpoints are sorted in byte order. The threads take the hours from a shared
//!    queue, in blocks of one hour per thread, the largest hour of each block first:
//!    a thread gathers one hour's runs from every part, groups them by endpoint
//!    with a counting sort small enough to stay in L1/L2, gives back the records
//!    that no hour reads any more, and finds each group's exact percentiles: by
//!    counting ranks with AVX2 in groups of up to 64 values (95% of the groups), by
//!    selection in larger ones.
//! 5. Each hour is formatted straight into the output file, mapped shared, last
//!    line first, once the hours before it have their lengths and the records given
//!    back make room for it; a thread takes further hours meanwhile. So the output
//!    takes the place of the records, rather than adding to the memory peak. Once
//!    all of it is written, the threads drop its page-table entries in parallel, so
//!    the exit does not.

// The C runtime calls `main` below directly and exits with what it returns, so std's
// start-up is skipped: the main thread's stack-guard lookup (it reads /proc/self/maps),
// the signal handlers that report a stack overflow, with an alternate signal stack
// that every spawned thread then maps too, SIGPIPE set to ignore, and the check of
// fds 0 to 2. Nothing here needs them: it does not recurse deeply, write to a pipe or
// use stdin or stdout. The arguments still reach std::env::args (glibc passes them to
// std through .init_array), and a panic still prints its message, then aborts
// (panic = "abort").
#![no_main]

#[cfg(not(target_feature = "avx2"))]
compile_error!("track-a needs AVX2: build with RUSTFLAGS=\"-C target-cpu=native\"");

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::*};
use std::{arch::x86_64::*, cell::Cell, fs::File, io::Error, os::fd::AsRawFd, ptr, sync::OnceLock};

/// A record is `endpoint id << 21 | error << 20 | latency`: 11 bits of endpoint id,
/// bit 20 set for a 5xx status, and 20 bits of latency. A latency of 7 or more digits
/// (1.2% of lines), which may not fit, is stored as ESCAPE, its value appended to the
/// part's escapes; up to 6 digits always fit, below ESCAPE.
const ENDPOINT_SHIFT: u32 = 21;
const ERROR_SHIFT: u32 = 20;
const ESCAPE: u32 = (1 << ERROR_SHIFT) - 1;
/// Endpoint ids must fit the record's 11 bits: the spec has 1,842 endpoints.
const MAX_ENDPOINTS: usize = 1 << (32 - ENDPOINT_SHIFT);
/// Zero bytes mapped after the end of the log, so that fixed-size SIMD loads at
/// any line of it stay inside the mapping.
const PAD: usize = 256;
/// The fewest bytes the parse moves on per line, whatever the bytes are (see
/// `parse`): a slice of n bytes holds at most n / MIN_LINE + 1 lines.
const MIN_LINE: usize = 38;
/// The longest endpoint the spec allows, in bytes; the parse stops on a longer one.
const MAX_NAME: usize = 125;
/// An output line starts with the hour label "YYYY-MM-DDTHH:00:00Z" and a space.
const LABEL: usize = 21;

// SAFETY: the only symbol named `main` (`#![no_main]` leaves out rustc's own, and the
// crate has no dependencies), with the signature the C runtime calls it by, C's
// `int main(int argc, char **argv)`.
#[unsafe(no_mangle)]
extern "C" fn main(_: i32, _: *const *const u8) -> i32 {
    // The phases, in order:
    // 1. map the log;
    // 2. start one pool of threads, this one included, which runs both passes;
    // 3. pass 1, in each thread: parse slices of the log into packed records;
    // 4. the merge, on the last thread to finish pass 1: the hours and the output
    //    mapping, handed over together, then the endpoints' name order;
    // 5. pass 2, in each thread: per hour, group the records by endpoint, compute
    //    the statistics, and write each hour once the earlier ones have lengths and
    //    the records given back make room for it;
    // 6. teardown: each thread drops a share of the output's page-table entries,
    //    then this thread cuts the file to its length and exits.
    //
    // The worker stays one closure: it shares a dozen tables and counters across both
    // passes. Split into functions that take the shared state as parameters, the parse
    // loop compiled differently (other registers, one more stack load per turn) and
    // runs were 0.64% and 0.21% slower on the replica (201 pairs, two builds a side): a
    // parameter is held for the whole function, while the closure reloads a capture
    // where it uses it. Splitting out only the hour's write or the log's mapping
    // measured +0.39% and +0.54% (PERF.md, step 58). (A context struct of the same
    // references would keep the closure's loads, at about 36 more lines; not built.)
    //
    // Transparent huge pages off: with THP set to "always", touching one byte of a
    // sparse, lazily zeroed table can fault in a whole 2 MB page, so the memory
    // peak would depend on how fragmented the host is that run.
    // SAFETY: a plain syscall that sets one flag of this process, called with the
    // unsigned long arguments it reads; a failure only leaves THP on.
    unsafe { prctl(PR_SET_THP_DISABLE, 1u64, 0u64, 0u64, 0u64) };
    // Optional arguments: the data and output directories (the Dockerfile's PGO
    // training run passes /tmp/train /tmp/out).
    let dir = |n, default: &str| std::env::args().nth(n).unwrap_or(default.into());
    let input = File::open(dir(1, "/data") + "/requests.log").expect("open the log");
    // Read and write: the output is mapped shared and written through memory.
    let mut options = File::options();
    options.read(true).write(true).create(true).truncate(true);
    let output = options.open(dir(2, "/out") + "/summary.txt");
    let output = output.expect("create the summary");
    let len @ 1.. = input.metadata().expect("stat the log").len() as usize else {
        return 0; // the spec's empty input gives an empty summary
    };
    // 1. Map the log.
    // The log followed by PAD zero bytes: the whole range is reserved as zero
    // pages, then the file is mapped over its start (the rest of the file's last
    // page reads as zero). The dataset is mounted read-only, so the bytes cannot
    // change under us.
    let (anonymous, fixed) = (MAP_PRIVATE | MAP_ANONYMOUS, MAP_PRIVATE | MAP_FIXED);
    let base = map(ptr::null_mut(), len + PAD, PROT_READ, anonymous, -1);
    // SAFETY: MAP_FIXED replaces only the start of the reservation just made, which
    // nothing uses yet; the assert checks that the call did.
    let at = unsafe { mmap(base, len, PROT_READ, fixed, input.as_raw_fd(), 0) };
    assert!(at == base, "mmap the log: {}", Error::last_os_error());
    // SAFETY: `len + PAD` bytes are mapped, read-only and unchanging (see above), and
    // stay mapped until the parse is over; `log` is not used after that (see munmap).
    let log: &[u8] = unsafe { std::slice::from_raw_parts(base, len) };

    // 2. The pool, and what pass 1 needs.
    // Pass 1, parallel: the threads parse slices of the log, cut at line ends, into
    // packed records, with one endpoint table shared by all threads. Cut p is just
    // after the last LF before byte at(p), below (0 if there is none; the last
    // cut is the end of the log, which ends with an LF). Cuts never decrease, so the
    // slices hold every line once; two cuts in one line give an empty slice.
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    // Two pairs of slices per thread, taken in file order from a shared counter.
    // With one slice each, the scheduler sometimes put two threads on one CPU and
    // they kept it for the whole parse (4 of 30 runs on the test server, dev build):
    // the last thread finished its parse 4.9 to 8.0 ms after the first, and the run
    // took 18.1-20.8 ms against 14.6-15.6 ms (PERF.md, step 38). Now a slowed thread
    // just parses fewer slices. (Four pairs a thread: 0.3 ms slower on the test
    // server, 61 interleaved runs, from the per-slice work, mostly each slice's
    // release, one a slice then; pinning each thread to a CPU instead: worse, a pinned
    // thread cannot leave a CPU that something else is using.) A pair is parsed side
    // by side (see `parse`); the count is even, so a pair's second slice always exists.
    let slices = threads * 4;
    // Slice q's weight (slice q is in pair q / 2): the cuts share out the log in
    // proportion (`at`). A second-round slice weighs 200, so a unit is half a percent of
    // it. The first round's pairs (one a thread, claimed in start order) shrink by a unit
    // a claim (the slope): each of the 11 spawns takes about 50 us, so each claim's
    // thread starts about 50 us after the one before. The first pair is 10 units (5%)
    // smaller again: its thread adds most of the endpoints to the table while the spawns
    // are still under way. (Fitted to the parse ends of every thread in 14 runs on the
    // replica, where the last thread trailed the mean by 190 us. Measured, the 5% cut is
    // the gain and the slope adds nothing measurable; without the slope, in closed form,
    // the worker compiled differently and ran +0.33%: PERF.md, step 72 and experiments.)
    let w = |q: usize| 200 + threads.saturating_sub(q / 2) - 10 * (q < 2) as usize;
    let at = |p: usize| len * (0..p).map(w).sum::<usize>() / (0..slices).map(w).sum::<usize>();
    // Cut p: at(p) less the bytes after the last LF before it (rsplit always yields a
    // piece: the unwrap cannot fail). Those bytes and that LF may be in pages already
    // released, by this thread or another: they fault back in from the page cache
    // unchanged (see release).
    let cut = |p| at(p) - log[..at(p)].rsplit(|&b| b == b'\n').next().unwrap().len();
    // One pool of threads for both passes: each parses slices, then waits for the
    // merge (done by the last thread to finish pass 1, once every part is in) and
    // goes on with pass 2, so no thread is joined or started between the passes.
    let parts: Vec<OnceLock<Part>> = (0..slices).map(|_| OnceLock::new()).collect();
    // The merge's hand-off (`Merged`), then (id, name) of every endpoint in byte order
    // of the names (`names`), which the merging thread sorts after it.
    let (merged, order) = (OnceLock::<Merged>::new(), OnceLock::new());
    // How many runs of each part are still to be placed: the run that brings its
    // part's count to zero gives the part's records back.
    let readers: Vec<AtomicUsize> = parts.iter().map(|_| AtomicUsize::new(0)).collect();
    // Shared counters: the next slice to parse, the next hour of the queue, the threads
    // done with pass 1, the next share of the output's page-table entries to drop, and
    // the bytes of records given back so far and of output written (see `flush`).
    let [slice, next, parsed, share, given, wrote] = [(); 6].map(|_| AtomicUsize::new(0));
    // One pool of threads, this one included: as many threads as CPUs. (With this
    // thread apart, merging, 13 threads shared 12 CPUs, likely leaving two workers on
    // one CPU for the whole parse (PERF.md, step 38): replica, 300 runs each, 21 to 43
    // runs over +8% against 6 to 10 now, and a median 0.9% slower: PERF.md, step 63.)
    // The last thread to finish pass 1 merges. `written` is the teardown's barrier
    // (phase 6). The log's address goes as a number: a raw pointer cannot go to another
    // thread.
    let (written, base_addr) = (std::sync::Barrier::new(threads), base as usize);
    let total = std::thread::scope(|s| {
        let work = || {
            // 3. Pass 1.
            // This thread's records, each part taking the start of what is left.
            // Room for every line of the log, allocated zeroed: only the pages
            // written are ever backed by memory. One mapping per thread, not per
            // slice: a mapping per slice made eight slices a thread 2 ms slower.
            // (A slice of n bytes has at most n / MIN_LINE + 1 lines.)
            let mut free = &mut vec![0; len / MIN_LINE + slices].leak()[..];
            let claim = || slice.fetch_add(2, Relaxed);
            for p in std::iter::repeat_with(claim).take_while(|&p| p < slices) {
                let pair = [&log[cut(p)..cut(p + 1)], &log[cut(p + 1)..cut(p + 2)]];
                // SAFETY: the slices are adjacent (cut p to p + 1, then p + 1 to p + 2)
                // in the log: one private, read-only file mapping, which PAD zero bytes
                // follow.
                let [a, b] = unsafe { parse(pair, &mut free) };
                // Each index comes from the counter once, so these are the only sets
                // of parts p and p + 1 and cannot fail. (Unchecked on purpose:
                // `expect` or an assert here changed the instructions of the parse
                // loop above.)
                let _ = (parts[p].set(a), parts[p + 1].set(b));
            }
            // 4. The merge, on the last thread to finish pass 1: every part is in.
            if parsed.fetch_add(1, AcqRel) == threads - 1 {
                merged.set(merge(&parts, &readers, &output)).ok(); // cannot fail
                // The parse threads gave back the log's pages, but not the page
                // tables that mapped them (0.5 MB, charged to us): unmapping frees
                // those too, while the other threads start pass 2 and before their
                // values buffers fault in. A failure would only keep them.
                // SAFETY: every part is in, so nothing reads the log any more: every
                // endpoint's bytes and key were copied, and `log` is not used again.
                unsafe { munmap(base_addr as *mut u8, len + PAD) };
                // The name order, sorted while the other threads start pass 2: an hour
                // first waits for it when it builds its groups, after placing its values.
                // SAFETY: this is the merging thread, after the merge: every part is in,
                // so the parse is over, as `names` requires.
                order.set(unsafe { names() }).ok(); // cannot fail: set once, here
                // SAFETY: the keys (in the table, in .bss: anonymous memory) are read
                // only during the parse and by the sort just above (`names`), so
                // nothing reads them again; names are kept.
                unsafe { release(&TABLE.keys[..]) };
            }
            // 5. Pass 2, once the merge's hand-off is set (the groups wait for the name order).
            let (hours, file, lens) = merged.wait();
            // The number of records in hour h: each run's end minus its first record.
            let size = |h: usize| hours[h].iter().map(|run| run.end - run.begin.at).sum();
            let largest = (0..hours.len()).map(size).max().unwrap_or(0);
            // Where an hour's output starts, once the lengths of all earlier hours are known.
            let offset = |hour| lens[..hour].iter().try_fold(0, |at, n| Some(at + n.get()?));
            // Room kept for the values buffers below, whose pages are backed as their
            // hours reach them: three quarters of the bytes they can hold.
            let held = threads * largest * size_of::<u32>() * 3 / 4;
            // Writes the pending hours whose start is known. While hours are still being
            // computed (`gated`), an hour also waits until the records given back cover
            // the output written so far, its own bytes and `held`. Written as soon as its
            // start was known, the output piled up on the records and the values buffers
            // 2-3 ms into pass 2, at the memory peak; gated, the highest of 5 runs is
            // 0.7 to 0.8 MB lower at equal speed. (All the buffers' room: up to 0.4 MB
            // lower again, but 0.9% slower, as more hours are left to write at the end.)
            // A heuristic: two threads may pass it at once; it changes when an hour is
            // written, never its bytes.
            let flush = |pending: &mut Vec<(usize, usize, Vec<Stats>)>, gated: bool| {
                pending.retain(|&(hour, len, ref groups)| {
                    let Some(at) = offset(hour) else { return true };
                    if gated && wrote.load(Relaxed) + len + held > given.load(Relaxed) {
                        return true;
                    }
                    wrote.fetch_add(len, Relaxed);
                    // SAFETY: bytes at..at + len of the mapping are this hour's alone, and
                    // at + len <= total <= bound: a line is at most 213 bytes (see bound).
                    let out = unsafe { std::slice::from_raw_parts_mut((file + at) as _, len) };
                    // The line head, the 21-byte label "YYYY-MM-DDTHH:00:00Z " (the hour key's
                    // 13 bytes are the last 13 of its 16 big-endian bytes), and the name order
                    // (already set: this hour's groups were built in it).
                    let head = [&hours[hour][0].begin.key.to_be_bytes()[3..], b":00:00Z "].concat();
                    let endpoints = order.wait();
                    // Last line first, each one ending where the next one starts.
                    let mut at = len;
                    let named = endpoints.iter().zip(groups).filter(|(_, stat)| stat[0] > 0);
                    for (&(_, name), &stat) in named.rev() {
                        at = write(&head, name, stat, out, at);
                    }
                    assert_eq!(at, 0, "hour length");
                    false
                })
            };

            // Pass 2, parallel over hours: each thread takes the next hour, gathers its
            // records from every part, groups them by endpoint id, computes each group's
            // statistics and the hour's length, then writes each of its hours once the
            // earlier hours' lengths are known, taking further hours meanwhile.
            // Hours are taken in blocks of `threads` hours, one per thread, the
            // largest hour of each block first (every thread builds the same
            // queue). Taken in time order, a large hour started last in its block
            // held up the writing of the small hours after it, which waited with
            // their statistics in memory (up to 30 hours at once); started first,
            // it ends with its block.
            let mut queue: Vec<_> = hours.iter().enumerate().collect();
            queue.sort_by_key(|&(hour, _)| (hour / threads, std::cmp::Reverse(size(hour))));
            let (mut start, mut counts) = ([0u32; MAX_ENDPOINTS + 1], [0u64; MAX_ENDPOINTS]);
            // Room for the largest hour, allocated zeroed: its own mapping, whose
            // pages are backed only as far as this thread's hours reach.
            let (mut values, mut pending) = (vec![0; largest], Vec::new());
            while let Some(&(hour, &runs)) = queue.get(next.fetch_add(1, Relaxed)) {
                // Count per endpoint id, errors in the high half of the same
                // word (one increment per record), then set start[i + 1] to the
                // start of id i's group and place each latency at it, moving it
                // on: afterwards start[i]..start[i + 1] is id i's group.
                // (Each run is copied at the head of the loop: read in place, its
                // fields compiled to other instructions in both loops.)
                for &Run { begin, part, end } in runs {
                    for &record in &parts[part].wait().records[begin.at..end] {
                        let id = (record >> ENDPOINT_SHIFT) as usize;
                        counts[id] += 1 | ((record >> ERROR_SHIFT & 1) as u64) << 32;
                    }
                }
                // The low half of a count word is the record count (`as u32`).
                let mut sum = 0;
                for (at, &count) in start[1..].iter_mut().zip(&counts) {
                    (*at, sum) = (sum, sum + count as u32);
                }
                for &Run { begin, part, end } in runs {
                    let parsed = parts[part].wait();
                    let mut escapes = parsed.escapes[begin.escape..].iter();
                    for &record in &parsed.records[begin.at..end] {
                        let latency = match record & ESCAPE {
                            ESCAPE => *escapes.next().unwrap(),
                            latency => latency,
                        };
                        let at = &mut start[(record >> ENDPOINT_SHIFT) as usize + 1];
                        // SAFETY: each id's cursor stays below the next id's
                        // start, which is at most this hour's record count (the
                        // counts above are this hour's), so at most `largest`, the
                        // length of `values`.
                        unsafe { *values.get_unchecked_mut(*at as usize) = latency };
                        *at += 1;
                    }
                    // The records are no longer needed once placed: the last run of a
                    // part to be placed gives them all back. Parts are small (a 48th of
                    // the log), so one call per part is soon enough, and every call is
                    // a round of TLB shootdowns. (A count per run, not per hour and
                    // part: seed 1 has 1,101 runs against 134 such pairs, so about 80
                    // more decrements a thread, at the same speed on the replica. A
                    // part's escapes are a few KB: no whole page to give back.)
                    if readers[part].fetch_sub(1, AcqRel) == 1 {
                        given.fetch_add(size_of_val(parsed.records), Relaxed);
                        // SAFETY: anonymous memory (see `free`) that every run of the part
                        // has placed (the AcqRel count orders their loads before this
                        // call), so nothing reads it again.
                        unsafe { release(parsed.records) };
                    }
                }
                // The groups in byte order of their names: one entry per
                // endpoint, in the order of `endpoints`, so no name is stored.
                let endpoints = order.wait();
                let (mut groups, mut len) = (Vec::with_capacity(endpoints.len()), 0);
                for &(id, name) in endpoints {
                    let group = &mut values[start[id] as usize..start[id + 1] as usize];
                    // Taken, so `counts` is all zeros again for the next hour: every id
                    // a record can hold is in `endpoints`, which lists every id handed out.
                    groups.push(stats(group, (std::mem::take(&mut counts[id]) >> 32) as u32));
                    len += line_len(name, groups.last().unwrap());
                }
                lens[hour].set(len).unwrap(); // each hour is taken from the queue once
                pending.push((hour, len, groups));
                flush(&mut pending, true);
            }
            drop(values); // not needed while the last hours wait to be written
            // Sleep until the hours before this thread's last one have their
            // lengths, then write the rest, ungated. (A yield loop here spun on CPUs
            // whose other hardware thread was still computing an hour.)
            let last = pending.iter().map(|p| p.0).max().unwrap_or(0);
            lens[..last].iter().for_each(|n| _ = n.wait());
            flush(&mut pending, false);
            // 6. Teardown.
            // Once every thread is here the output is written: each then drops the
            // page-table entries of a share of its mapping (the pages stay in the
            // file), which the exit would do for all of it on one core (0.6 ms on
            // the test server). Dropped while other threads still wrote, they slowed
            // those writes by as much (PERF.md, experiments). Shares are cut at pages.
            written.wait();
            let (total, k) = (offset(lens.len()).unwrap(), share.fetch_add(1, Relaxed));
            let at = |k: usize| (file + total * k / threads).next_multiple_of(PAGE);
            // SAFETY: whole pages of the shared output mapping (page-aligned), all
            // written and not touched again. MADV_DONTNEED drops only their
            // page-table entries: the pages stay in the file.
            unsafe { madvise(at(k) as *mut u8, at(k + 1) - at(k), MADV_DONTNEED) };
            total
        };
        (1..threads).for_each(|_| _ = s.spawn(work));
        work() // the last worker is this thread
    });
    output.set_len(total as u64).expect("size the summary");
    // The output is complete in the page cache. Exit now: dropping the tables
    // above, one free at a time, took a few milliseconds on the scoring box.
    std::process::exit(0)
}

/// The merge between the passes: every part's runs sorted by hour and cut into
/// hours, each part's run count, and the output mapped at a size no output can
/// exceed. The endpoints' name order follows, from `names`.
fn merge(parts: &[OnceLock<Part>], readers: &[AtomicUsize], output: &File) -> Merged<'static> {
    // Every part's runs, in order of hour. Every part is in (this is the last thread
    // to finish pass 1), so no `wait` here blocks. Each start of a part begins one
    // run, which ends at the next start's first record, the last one at the part's
    // end. Room for all of them at once.
    let mut runs = Vec::with_capacity(parts.iter().map(|p| p.wait().starts.len()).sum());
    for (part, p) in parts.iter().enumerate() {
        let (starts, records) = (&p.wait().starts, p.wait().records.len());
        let ends = starts.iter().skip(1).map(|s| s.at).chain([records]);
        let bounds = starts.iter().copied().zip(ends);
        runs.extend(bounds.map(|(begin, end)| Run { begin, part, end }));
        // Each run is one reader of its part: the last one placed gives the part back.
        // (Relaxed: the hand-off orders this store before every decrement in pass 2.)
        readers[part].store(starts.len(), Relaxed);
    }
    // By hour only: the order of an hour's runs changes no output byte (each run reads
    // its own escapes; a group's values are counted or selected, in any order).
    runs.sort_unstable_by_key(|run| run.begin.key);
    // Cut into hours. Leaked: pass 2 reads the runs until the exit.
    let same_hour = |a: &Run, b: &Run| a.begin.key == b.begin.key;
    let hours: Vec<&[Run]> = runs.leak().chunk_by(same_hour).collect();

    // The output is mapped shared at a size no output can exceed, and cut to its
    // length at the end. On tmpfs, where /out is, one write call per hour would
    // serialise on the file's lock; page faults on the mapping do not. A line is
    // at most 213 bytes: the label and its space (LABEL), the name (at most 125
    // bytes, checked by the parse), six numbers of at most 10 digits after a space
    // each, and LF. Every endpoint id handed out (`count`) has a group in each hour.
    let bound = hours.len() * TABLE.count.load(Relaxed) * (LABEL + MAX_NAME + 6 * 11 + 1);
    output.set_len(bound as u64).expect("size the summary");
    // Kept as an address: a raw pointer could not be shared by the threads,
    // which each write only their own hours' bytes.
    let (read_write, fd) = (PROT_READ | PROT_WRITE, output.as_raw_fd());
    let file = map(ptr::null_mut(), bound, read_write, MAP_SHARED, fd) as usize;
    let lens = hours.iter().map(|_| OnceLock::new()).collect();
    (hours, file, lens)
}

/// The endpoints in byte order of their names, sorted by the merging thread after the
/// hand-off and the log's unmap, while the others start pass 2. In the merge, the sort
/// and its collect (0.16 ms) held up every other thread; moved here, the run was 0.9%
/// faster on the replica (201 pairs, two builds a side; PERF.md, step 69).
///
/// # Safety
///
/// Only once the parse is over, as the merging thread calls it (every part is in,
/// which orders every parse thread's writes before the call): then each id up to
/// `count` has its name and key written, and nothing writes the table any more.
unsafe fn names() -> Vec<(usize, &'static [u8])> {
    // (id, name) of every id handed out (ids start at 1), in byte order of the names:
    // first by the first 32 bytes of the keys as two big-endian numbers, so few
    // comparisons need the names, then by the full bytes on ties. A key has a
    // space after its endpoint, below every endpoint byte, then zeros, so a
    // proper prefix still sorts first. A spare id that was never published (see
    // `add`) is listed too, under a copy of a name that a published id holds: no
    // record holds the spare, so its group is empty in every hour and prints no line.
    // SAFETY: the parse is over (the caller's contract, see # Safety): each id up to
    // `count` has its name written, and nothing writes the table any more.
    let entry = |id: usize| (id, unsafe { &**TABLE.names[id].get() });
    // SAFETY: as above, for the keys. A Key's first 32 bytes, read as two
    // [u8; 16], are plain bytes: any value is valid, and their alignment is 1.
    let key = |id: usize| unsafe { *TABLE.keys[id].get().cast::<[[u8; 16]; 2]>() };
    let mut endpoints: Vec<_> = (1..=TABLE.count.load(Relaxed)).map(entry).collect();
    endpoints.sort_unstable_by_key(|&(id, name)| (key(id).map(u128::from_be_bytes), name));
    endpoints
}

/// The six numbers of a group's line: count (0: no line), p50, p95, p99, errors and
/// the error rate in parts per million. All fit 32 bits: latencies are unsigned
/// 32-bit numbers (the spec). An hour keeps these until it is written (24 bytes per
/// endpoint).
type Stats = [u32; 6];

/// The length of a group's line, "YYYY-MM-DDTHH:00:00Z endpoint" then the numbers,
/// each after a space, and LF: the 20-byte label and its space, the name, a space and
/// the digits of each number, and the LF. 0 for an empty group, which has no line.
fn line_len(name: &[u8], stat: &Stats) -> usize {
    let digits = |v: u32| v.checked_ilog10().map_or(1, |d| d as usize + 1);
    let len = LABEL + name.len() + stat.iter().map(|&v| 1 + digits(v)).sum::<usize>() + 1;
    if stat[0] > 0 { len } else { 0 } // a count of 0: no line
}

/// Writes the line that ends just before `out[end]`, from its end, straight into
/// place, and returns where it starts: no number's length is needed first, and nothing is
/// copied. (Put together in a stack buffer and copied, the copy's wide loads would wait
/// for the buffer's byte stores, a store-forwarding stall a line; the replica measured
/// the same speed either way, and this one is 11 lines shorter: PERF.md, step 50.)
fn write(head: &[u8], name: &[u8], stat: Stats, out: &mut [u8], end: usize) -> usize {
    let mut at = end - 1;
    out[at] = b'\n';
    for mut value in stat.into_iter().rev() {
        // Two digits at a time from the end while more than two are left, then the
        // last one or two as a pair: a lone digit's leading 0 is overwritten by the
        // space before the number.
        while value >= 100 {
            at -= 2;
            out[at..at + 2].copy_from_slice(&PAIRS[2 * (value % 100) as usize..][..2]);
            value /= 100;
        }
        out[at - 2..at].copy_from_slice(&PAIRS[2 * value as usize..][..2]);
        at -= 2 + (value >= 10) as usize;
        out[at] = b' ';
    }
    // The head (the 20-byte label and its space: LABEL bytes), then the name.
    at -= LABEL + name.len();
    out[at..at + LABEL].copy_from_slice(head);
    out[at + LABEL..at + LABEL + name.len()].copy_from_slice(name);
    at
}

/// "00", "01", ..., "99".
static PAIRS: &[u8; 200] = b"000102030405060708091011121314151617181920212223242526272829303132\
    33343536373839404142434445464748495051525354555657585960616263646566676869707172737475767778\
    798081828384858687888990919293949596979899";

/// One thread's parse of its slice: one record per line, in file order (see
/// ENDPOINT_SHIFT); the latencies of 7 or more digits, in the same order; and where
/// each run of lines in one hour starts.
/// (`repr(C)` keeps this field order: in the compiler's own, records last, the
/// worker compiled to other registers, the parse loop included.)
#[repr(C)]
struct Part {
    escapes: Vec<u32>,
    records: &'static [u32],
    starts: Vec<Start>,
}

/// Where a run of a part's lines in one hour starts: its hour key (the timestamp's
/// first 13 bytes "YYYY-MM-DDTHH", read as a big-endian number), the index of its
/// first record (`at`) and of its first escape.
#[derive(Clone, Copy)]
struct Start {
    key: u128,
    at: usize,
    escape: usize,
}

/// A run of one part's records in one hour: where it begins, the part, and the end
/// of its records (the part's next start, or the part's end for its last run). Its
/// record count is `end - begin.at`.
struct Run {
    begin: Start,
    part: usize,
    end: usize,
}

/// What the merge between the passes hands to pass 2: the runs in order of hour, cut
/// into hours; the output mapping's address; and each hour's output length once
/// known.
type Merged<'a> = (Vec<&'a [Run]>, usize, Vec<OnceLock<usize>>);

/// Parses two slices of the mapped log side by side, one line of each in turn: a
/// line's end, and so the next line's start, waits on its loads, and the other
/// slice's line fills the wait. The records go into the start of `free`, which keeps
/// the rest; then the slices' pages are given back. Every line is
/// "ts SP method SP endpoint SP status SP latency LF" (27-byte ts).
///
/// # Safety
///
/// The PAD bytes after each slice must be readable: the loads of a line run on past
/// its end. And the slices must be adjacent (the second starts where the first ends)
/// in one private file mapping that this process never writes, as their whole pages
/// go back to the kernel at the end, in one call (see `release`).
unsafe fn parse(chunks: [&[u8]; 2], free: &mut &'static mut [u32]) -> [Part; 2] {
    // The loop writes without capacity checks: it moves on at least MIN_LINE bytes
    // per line whatever the bytes are (the endpoint ends at byte 32 or later, then
    // come the 5 bytes up to the latency, 0 or more digits and the LF: 32 + 5 + 0 + 1),
    // so no slice has more lines than its room. The second slice's records start
    // after room for the first one's.
    let most = chunks.map(|chunk| chunk.len() / MIN_LINE + 1);
    assert!(free.len() >= most[0] + most[1]);
    let (first, second) = std::mem::take(free).split_at_mut(most[0]);
    // Each slice's unparsed bytes (from its next line to its end), its records' start
    // and next place, the previous line's hour bytes, the latencies of 7 or more
    // digits, its runs.
    let mut unparsed = chunks.map(|c| c.as_ptr_range());
    let start = [first.as_mut_ptr(), second.as_mut_ptr()];
    let mut record = start;
    let (mut last, mut escapes, mut runs) = ([u64::MAX; 2], [vec![], vec![]], [vec![], vec![]]);
    // (u64::MAX is not 8 ASCII bytes, so each slice's first line starts a run.)
    // Parses slice k's next line into one record, if there is one.
    // SAFETY: whatever the bytes, every load starts at a line start below its
    // slice's end and ends within PAD (256) bytes of it, which the caller makes
    // readable. Counted from the line start:
    // - the space window ends by byte 95 (31 + 64);
    // - the endpoint starts by 96 (32 + 64); the inline lookup reads from 8 bytes
    //   before it to 64 after it (by 160), the long way 128 after it (by 224);
    // - it ends by 94 (31 + 63), not before its start, or in the long way by 221
    //   (96 + 125), where the assert stops an over-125-byte name;
    // - the latency's 16 bytes start by 226 (221 + 5) and end by 242.
    // The digit tables are read only for count <= 6: 16 of DIGITS.0's 72 bytes from
    // byte 8 * count (so by byte 64), 8 of DIGITS.1's 36 entries from entry 4 * count
    // (by entry 32).
    // Records are written within each slice's room (asserted above): at most
    // `most[k]` lines, one record each. `free` is not used meanwhile. The slices may
    // go back to the kernel at the end (the caller's contract): their endpoint names
    // were copied (see Interner::add).
    let mut step = |k: usize| unsafe {
        let line = &mut unparsed[k].start;
        if *line >= unparsed[k].end {
            return false; // slice k is done
        }
        // Lines from one shard arrive in time order, so most lines repeat the
        // previous line's hour. Its bytes "MM-DDTHH" (5..13) decide: every timestamp
        // lies in one 72-hour span (the spec), so lines that share them share the year.
        let hour = line.add(5).cast::<u64>().read_unaligned();
        if hour != last[k] {
            last[k] = hour;
            let key = u128::from_be_bytes(*line.cast()) >> 24; // the first 13 bytes
            let (at, escape) = (record[k].offset_from_unsigned(start[k]), escapes[k].len());
            runs[k].push(Start { key, at, escape });
        }
        // One 64-byte window from byte 31: its first two spaces end the method and
        // the endpoint (no branch on the method or the length). Byte 27 is the space
        // after the timestamp and every method has 3 to 7 letters, so bytes 28-30 are
        // letters and the method's space is at byte 31 to 35, in the window. Started
        // there rather than at byte 28, the window reaches 3 bytes further into the
        // endpoint: 6.6% of seed 1's lines end past it, against 7.6%. The endpoint
        // starts one byte after the method's space.
        let load = |p: *const u8| _mm256_loadu_si256(p.cast());
        let window = eq_mask(load(line.add(31)), SPACE) | eq_mask(load(line.add(63)), SPACE) << 32;
        let first = window.trailing_zeros() as usize;
        let from = line.add(32 + first);
        // The window without its lowest bit: its lowest bit is now the second space
        // (none: the endpoint goes on past the window, and the long way finds its
        // end; a zero window stays zero and goes the long way too).
        let rest = window & window.wrapping_sub(1);
        let len = (rest.trailing_zeros() as usize).wrapping_sub(first + 1);
        // Endpoints that end in the window (93% of lines) are looked up inline; the
        // rest, and the inline lookup's misses, go the long way.
        let hit = (rest != 0).then(|| TABLE.find(&[load(from), load(from.add(32))], from, len));
        let (endpoint, len) = hit.flatten().map_or_else(|| TABLE.id(from), |id| (id, len));
        // The latency: 1 to 10 digits from 5 bytes after the endpoint (its space,
        // the three status digits and a space), to the LF, all within the 16 bytes
        // from there: up to 6 digits (98.8% of lines) are converted where they lie, and
        // fit a record.
        let to = from.add(len);
        let tail = _mm_loadu_si128(to.add(5).cast());
        let lf = _mm_cmpeq_epi8(tail, _mm_set1_epi8(b'\n' as i8));
        // 32 if there is no LF in the 16 bytes (a file that breaks the spec).
        let count = _mm_movemask_epi8(lf).trailing_zeros() as usize;
        let latency = if count <= 6 {
            let v = _mm_sub_epi8(tail, _mm_set1_epi8(b'0' as i8));
            let v = _mm_maddubs_epi16(v, _mm_loadu_si128(DIGITS.0.as_ptr().add(8 * count).cast()));
            let v = _mm_madd_epi16(v, _mm_loadu_si128(DIGITS.1.as_ptr().add(4 * count).cast()));
            _mm_cvtsi128_si32(_mm_add_epi32(v, _mm_srli_epi64(v, 32))) as u32
        } else {
            parse_digits(to.add(5), count, &mut escapes[k])
        };
        // The status's first digit is right after the endpoint's space; '5' is 5xx.
        *record[k] = endpoint | ((*to.add(1) == b'5') as u32) << ERROR_SHIFT | latency;
        record[k] = record[k].add(1);
        *line = to.add(5 + count + 1);
        true
    };
    // One line of each slice in turn until both are done (a done slice just returns false).
    while step(0) | step(1) {}
    // Both slices go back to the kernel in one call (they are adjacent, the caller's
    // contract): every call is a round of TLB shootdowns on the other threads' CPUs.
    let pair = ptr::slice_from_raw_parts(chunks[0].as_ptr(), chunks[0].len() + chunks[1].len());
    // SAFETY: the slices are adjacent in one private file mapping (the caller's
    // contract), so `pair` is exactly their bytes, within that mapping, and they may go
    // back to the kernel (the same contract). Only the cuts at the pair's two ends may
    // read some of its bytes again, and those fault back in unchanged (see release).
    unsafe { release(pair) };
    // SAFETY: each slice's records run from its start to its next place.
    let lines = [0, 1].map(|k| unsafe { record[k].offset_from_unsigned(start[k]) });
    let (second, rest) = second.split_at_mut(lines[1]);
    *free = rest;
    // Each slice's escapes (e0, e1) and run starts (s0, s1), moved into its Part. (Built
    // with `[0, 1].map` instead, the worker compiled differently under the profile.)
    let ([e0, e1], [s0, s1]) = (escapes, runs);
    let part = |records, escapes, starts| Part {
        records,
        escapes,
        starts,
    };
    [part(&first[..lines[0]], e0, s0), part(second, e1, s1)]
}

/// 128 bytes of 0xFF then 128 zeros: the 128 bytes from `128 - n` keep the first `n`;
/// `id` keeps n = len + 1, the endpoint and its space.
static KEEP: [[u8; 128]; 2] = [[0xFF; 128], [0; 128]];

/// For n = 0..=6 digits at bytes 0..n of a vector, minus '0': at DIGITS.0[8n..],
/// maddubs weights that make pairs (10a + b, or a alone for the last of an odd
/// count), and at DIGITS.1[4n..], madd weights that give the pairs their place
/// values, in two sums then added. Bytes from n on weigh 0, so the digits need no
/// moving. Entries are 8 bytes apart, for one scaled address: a 16-byte load also
/// reads the next entry, which only weighs the vector's upper half, not summed.
/// Row n of the weights: 10, 1 for each pair, then 1 for an odd last digit; row n of
/// the place values: 10^(n - 2), 10^(n - 4), ... for the pairs, 1 for the last one.
static DIGITS: ([i8; 72], [i16; 36]) = (
    [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 10, 1, 0, 0, 0, 0, 0, 0, 10, 1, 1, 0, 0, 0,
        0, 0, 10, 1, 10, 1, 0, 0, 0, 0, 10, 1, 10, 1, 1, 0, 0, 0, 10, 1, 10, 1, 10, 1, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
    [
        0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 10, 1, 0, 0, 100, 1, 0, 0, 1000, 10, 1, 0, 10000, 100,
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ],
);

/// An endpoint's identity: its bytes (at most 125), a space, then zeros to 128 bytes.
/// No endpoint byte is a space or zero, so equal keys mean equal endpoints.
type Key = [__m256i; 4];

/// 32 spaces: compared with a line's bytes, they give the space masks of `parse` and
/// `Interner::id`.
// SAFETY: any 32 bytes are a valid `__m256i`.
const SPACE: __m256i = unsafe { std::mem::transmute([b' '; 32]) };

/// `vpcmpeqb` then `vpmovmskb`: bit i of the mask is set where byte i of `a` equals
/// byte i of `b`.
fn eq_mask(a: __m256i, b: __m256i) -> u64 {
    // SAFETY: AVX2 is enabled for this build.
    unsafe { _mm256_movemask_epi8(_mm256_cmpeq_epi8(a, b)) as u32 as u64 }
}

/// The record's latency for the `count` ASCII digits at `at`, 7 to 10 (1.2% of lines;
/// the parse converts up to 6 itself): ESCAPE, with the value, converted one digit at a
/// time, appended to `escapes` (see ESCAPE). Out of line: inlined into the parse
/// loop's cold branch, it measured +0.29% on the replica (PERF.md, experiments).
///
/// The spec guarantees those digits, and nothing checks them here: a latency that
/// breaks the spec gives a wrong value, never a read out of bounds, as at most the
/// 16 bytes at `at` are read.
///
/// # Safety
///
/// The 16 bytes at `at` must be readable.
#[cold]
#[inline(never)]
unsafe fn parse_digits(at: *const u8, count: usize, escapes: &mut Vec<u32>) -> u32 {
    // SAFETY: at most the 16 readable bytes at `at` (the caller's contract).
    let digits = unsafe { std::slice::from_raw_parts(at, count.min(16)) };
    // '0'..='9' & 15 is the digit; at most 16 digits of at most 15 cannot overflow a u64.
    let latency = digits.iter().fold(0, |v, &d| v * 10 + (d & 15) as u64);
    escapes.push(latency as u32); // exact: latencies fit 32 bits (the spec)
    ESCAPE
}

/// Open-addressing endpoint table shared by the parsing threads: lookups are plain
/// loads, the rare insert claims its slot by compare-and-swap. A slot holds an id as
/// a record holds it (`id << ENDPOINT_SHIFT`), so a lookup needs no shift. Ids start
/// at 1: a slot holding 0 is empty, and id 0's key stays zero, which no endpoint matches.
/// Fixed at SLOTS, 8x the most endpoints (an 11% load): ids must fit 11 bits anyway.
struct Interner {
    slots: [AtomicU32; SLOTS], // 0 = empty, else id << ENDPOINT_SHIFT
    keys: [std::cell::UnsafeCell<Key>; MAX_ENDPOINTS],
    names: [std::cell::UnsafeCell<*const [u8]>; MAX_ENDPOINTS], // the endpoints' bytes
    count: AtomicUsize,                                         // ids handed out
}

// SAFETY: an id's key and name are written only by the thread that took the id, and
// only before a slot publishes the id (Release): a spare id (see `add`) is written
// again by its thread's next insert, still unpublished. They are read only once a
// slot holding the id has been seen (Acquire), or after the parse.
unsafe impl Sync for Interner {}

const SLOTS: usize = 8 * MAX_ENDPOINTS;

/// The endpoint table, a zeroed static (in .bss): all-zero bytes are an empty table
/// (see `Interner`), so nothing clears it, and its pages are mapped as the parse first
/// touches them.
// SAFETY: all-zero bytes are a valid `Interner`: every field is an atomic or an array
// of atomics, of SIMD vectors or of raw slice pointers (null, length 0), the last two
// in `UnsafeCell`s, and each of these is valid as zeros.
static TABLE: Interner = unsafe { std::mem::zeroed() };

impl Interner {
    /// The slot an endpoint starts its probe at: its length, its first 8 bytes and
    /// its last 8, two CRC steps. SLOTS is a power of two, so `%` is a mask.
    fn home(len: usize, first: u64, last: u64) -> usize {
        // SAFETY: SSE4.2 is enabled for this build.
        unsafe { _mm_crc32_u64(_mm_crc32_u64(len as u64, first), last) as usize % SLOTS }
    }

    /// `id << ENDPOINT_SHIFT` of the endpoint of `len` bytes (0 to 62) at `from`,
    /// followed by a space, whose first 64 bytes are `bytes`, if it is in its home
    /// slot; `None` sends the caller the long way (`id`). Under 8 bytes the last 8
    /// bytes reach before `from`, into the method, so the slot is almost never the
    /// home one: the compare is exact all the same, and a miss goes the long way.
    ///
    /// # Safety
    ///
    /// The 8 bytes before `from` and the `len` bytes from it must be readable.
    unsafe fn find(&self, bytes: &[__m256i; 2], from: *const u8, len: usize) -> Option<u32> {
        // SAFETY: AVX2, BMI2 (`bzhi`) and SSE4.2 are enabled (target-cpu=native: the box's
        // i5-13500 has all three; the compile_error! guard checks AVX2, which implies
        // SSE4.2); `from - 8 .. from + len` is readable
        // (the caller's contract). A published id is below MAX_ENDPOINTS, and its key
        // is written (see Sync); a slot's value >> (ENDPOINT_SHIFT - 7) is its id
        // times the key's 128 bytes, so `known` is that id's key.
        unsafe {
            let first = _mm_cvtsi128_si64(_mm256_castsi256_si128(bytes[0])) as u64;
            let last = from.add(len).sub(8).cast::<u64>().read_unaligned();
            let id = self.slots[Self::home(len, first, last)].load(Acquire);
            let at = (id >> (ENDPOINT_SHIFT - 7)) as usize;
            let known = &*self.keys.as_ptr().cast::<u8>().add(at).cast::<Key>();
            let same = |i: usize| eq_mask(bytes[i], known[i]);
            // Bytes 0..=len must match: the endpoint and its space, which the stored
            // key has at its own length.
            let differ = !(same(0) | same(1) << 32) & _bzhi_u64(!0, len as u32 + 1);
            (differ == 0).then_some(id)
        }
    }

    /// `id << ENDPOINT_SHIFT` of the endpoint at `from` and its length, for any
    /// length, added if new.
    ///
    /// # Safety
    ///
    /// The 128 bytes from `from` must be readable.
    #[cold]
    #[inline(never)]
    unsafe fn id(&self, from: *const u8) -> (u32, usize) {
        // SAFETY: AVX2, BMI2 (`bzhi`) and SSE4.2 are enabled (see `find`). The reads
        // stay within the 128 bytes from `from` (the caller's contract; `at` reads 8
        // bytes from byte 0 or max(len, 8) - 8, so by byte max(len, 8) <= 125) and
        // within KEEP (len <= 125, asserted). A published id is below MAX_ENDPOINTS,
        // and its key and name are written (see Sync).
        unsafe {
            let load = |p: *const u8| _mm256_loadu_si256(p.cast());
            let line: Key = std::array::from_fn(|i| load(from.add(32 * i)));
            // The space mask built from two 64-bit halves: as one u128 of the four
            // masks, it compiled to `vpinsrd` inserts on the path of every later step.
            let half = |i| (eq_mask(line[i], SPACE) | eq_mask(line[i + 1], SPACE) << 32) as u128;
            // 128 if there is none: a 125-byte endpoint's space is in the 128 bytes.
            let len = (half(2) << 64 | half(0)).trailing_zeros() as usize;
            // Tripwire for the spec's endpoint length: a file that breaks it stops here.
            assert!(len <= MAX_NAME, "requests.log: an endpoint is too long");
            // The key: the bytes up to and with the space after the endpoint, then zeros.
            let keep = KEEP.as_ptr().cast::<u8>().add(127 - len);
            let key = std::array::from_fn(|i| _mm256_and_si256(line[i], load(keep.add(32 * i))));
            // The hash's words, the endpoint's first and last 8 bytes (both from byte 0
            // under 8), read from the line: `bits` keeps only a short endpoint and its
            // space, as the key holds them. From 8 bytes up, `find` hashes the same bytes;
            // under 8 it hashes bytes around the endpoint, so it almost never probes this
            // slot (see `find`). (Read back from `key`, the loads waited on the stores of
            // the vectors just built: this cold path took 10% of the run's cycles; reading
            // the line made the run 0.3% faster.)
            let bits = 8 * len.min(7) as u32 + 8;
            let at = |i: usize| _bzhi_u64(from.add(i).cast::<u64>().read_unaligned(), bits);
            let mut i = Self::home(len, at(0), at(len.max(8) - 8));
            loop {
                let id = match self.slots[i].load(Acquire) {
                    0 => self.add(&self.slots[i], &key, std::slice::from_raw_parts(from, len)),
                    slot => (slot >> ENDPOINT_SHIFT) as usize,
                };
                // The keys are equal when all 128 bytes are: each mask has its 32 bits set.
                let same = |i: usize| eq_mask((*self.keys[id].get())[i], key[i]);
                if same(0) & same(1) & same(2) & same(3) == 0xFFFF_FFFF {
                    return ((id as u32) << ENDPOINT_SHIFT, len);
                }
                i = (i + 1) % SLOTS;
            }
        }
    }

    /// Adds an endpoint not found up to the empty `slot`, and returns the id that
    /// slot then holds: if another thread filled it first, its id wins, whether or not
    /// that is this endpoint, and the caller probes on. The id taken here is then not
    /// published (no slot or record holds it), and the thread keeps it for its next
    /// insert, which overwrites its key and name. So at most one id per thread is ever
    /// unpublished: 1,842 endpoints and 12 threads need 1,854 of the 2,047 ids, and
    /// running out of ids (more endpoints than the spec allows) stops the program
    /// rather than writing a wrong record. The bytes are copied: they are compared and
    /// printed after the input's pages are released.
    fn add(&self, slot: &AtomicU32, key: &Key, name: &[u8]) -> usize {
        // An id that lost its slot stays this thread's, for its next insert: at most
        // one id per thread is ever unpublished.
        thread_local!(static SPARE: Cell<Option<usize>> = const { Cell::new(None) });
        let fresh = || self.count.fetch_add(1, Relaxed) + 1;
        let id = SPARE.take().unwrap_or_else(fresh);
        assert!(id < MAX_ENDPOINTS, "requests.log: too many endpoints");
        // SAFETY: the id is this thread's alone until a slot publishes it.
        unsafe { (*self.keys[id].get(), *self.names[id].get()) = (*key, name.to_vec().leak()) };
        let won = slot.compare_exchange(0, (id as u32) << ENDPOINT_SHIFT, Release, Acquire);
        SPARE.set(won.is_err().then_some(id)); // (taken above: None unless kept here)
        won.map_or_else(|held| (held >> ENDPOINT_SHIFT) as usize, |_| id)
    }
}

/// A group's line numbers (see Stats): its count (0, no line, for an empty group),
/// nearest-rank p50, p95 and p99 (the value at 1-indexed rank ceil(p * n / 100) in
/// sorted order), its errors and the error rate. Groups of up to 64 values (95% of
/// the groups) count ranks (see `small_ranks`); larger ones select: p50 first, then
/// each next rank within the part above the last one. Each value is read at once:
/// the next selection reorders that part, its own slot included.
fn stats(values: &mut [u32], errors: u32) -> Stats {
    let n = values.len();
    let ranks = [50, 95, 99].map(|p| (p * n).div_ceil(100).saturating_sub(1));
    let [p50, p95, p99] = match n {
        0 => return [0; 6],
        ..=8 => small_ranks::<1>(values, ranks),
        ..=32 => small_ranks::<4>(values, ranks),
        ..=64 => small_ranks::<8>(values, ranks),
        _ => {
            let [r50, r95, r99] = ranks;
            let p50 = *values.select_nth_unstable(r50).1;
            let p95 = *values[r50..].select_nth_unstable(r95 - r50).1;
            [p50, p95, *values[r95..].select_nth_unstable(r99 - r95).1]
        }
    };
    // The error rate in parts per million: errors * 1000000 / count, in integers.
    let rate = (errors as u64 * 1_000_000 / n as u64) as u32;
    [n as u32, p50, p95, p99, errors, rate]
}

/// The values at three ranks (0-indexed, in sorted order) of 1 to 8 * V values,
/// without sorting or a branch on the values. The value at rank r is the largest
/// value v with at most r values below v: the r + 1 smallest values are not above
/// the one at rank r, so a larger v has at least r + 1 values below it. The values
/// sit in V vectors of eight, zeros past the end (never the largest; never counted,
/// as only the real values are compared against), and one vector compare per value
/// and vector counts the values below each lane. (Sorting groups of up to 32 values
/// took twice as long in a microbenchmark on the groups' real sizes.)
#[inline(always)]
fn small_ranks<const V: usize>(values: &[u32], ranks: [usize; 3]) -> [u32; 3] {
    // SAFETY: AVX2 is enabled for this build. Each masked load reads only the
    // lanes below values.len() - 8k, which are in the slice; the pointer of a
    // vector past the end is formed with wrapping_add, and nothing is read there.
    // Any 32 bytes are a valid [u32; 8].
    unsafe {
        let lane = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);
        let n = values.len() as i32;
        let v: [__m256i; V] = std::array::from_fn(|k| {
            let mask = _mm256_cmpgt_epi32(_mm256_set1_epi32(n - 8 * k as i32), lane);
            _mm256_maskload_epi32(values.as_ptr().cast::<i32>().wrapping_add(8 * k), mask)
        });
        // Unsigned order as signed order: the top bit flipped.
        let flip = |x| _mm256_xor_si256(x, _mm256_set1_epi32(i32::MIN));
        let (x, mut below) = (v.map(flip), [_mm256_setzero_si256(); V]);
        for &value in values {
            let value = flip(_mm256_set1_epi32(value as i32));
            for k in 0..V {
                // A lane above `value` has one more value below it (true is -1).
                below[k] = _mm256_sub_epi32(below[k], _mm256_cmpgt_epi32(x[k], value));
            }
        }
        ranks.map(|rank| {
            let rank = _mm256_set1_epi32(rank as i32 + 1);
            let held = (0..V).map(|k| _mm256_and_si256(_mm256_cmpgt_epi32(rank, below[k]), v[k]));
            let most = held.reduce(|a, b| _mm256_max_epu32(a, b)).unwrap();
            let lanes: [u32; 8] = std::mem::transmute(most);
            lanes.into_iter().max().unwrap()
        })
    }
}

// SAFETY: these are the C library's own prototypes on x86-64 Linux, where C's int
// is i32 and long is i64; declared by hand so that the crate has no dependencies.
unsafe extern "C" {
    fn mmap(at: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn madvise(at: *mut u8, len: usize, advice: i32) -> i32;
    fn munmap(at: *mut u8, len: usize) -> i32;
    fn prctl(option: i32, ...) -> i32;
}

// Constants they take, as <sys/mman.h> and <sys/prctl.h> define them on Linux.
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_SHARED: i32 = 1;
const MAP_PRIVATE: i32 = 2;
const MAP_FIXED: i32 = 0x10;
const MAP_ANONYMOUS: i32 = 0x20;
const MADV_DONTNEED: i32 = 4;
const PR_SET_THP_DISABLE: i32 = 41;
/// x86-64's base page size: `release` gives back whole pages only.
const PAGE: usize = 4096;

/// mmap at file offset 0, at an address the kernel picks: `at` is only a hint, as
/// MAP_FIXED, which would replace memory in use, is refused. Only the log is
/// unmapped, after the parse; the process exits right after using the others.
fn map(at: *mut u8, len: usize, prot: i32, flags: i32, fd: i32) -> *mut u8 {
    assert!(flags & MAP_FIXED == 0, "map: MAP_FIXED");
    // SAFETY: without MAP_FIXED the kernel maps an unused range, so no memory that
    // the program uses changes; a failure is checked below.
    let ptr = unsafe { mmap(at, len, prot, flags, fd, 0) };
    assert!(ptr as isize != -1, "mmap: {}", Error::last_os_error());
    ptr
}

/// Gives the whole pages inside `slice` back to the kernel (MADV_DONTNEED); the
/// pages it shares with memory around it are kept. Used on the log's slices, a parsed
/// pair at a time, on a part's records once no hour reads them, and on the endpoint
/// keys after the sort.
/// The parse threads drop the log's pages this way, in parallel: left to one unmap
/// (the exit's when measured, the merging thread's `munmap` now), its 58,000 pages go
/// one by one on one core (1.3 ms slower overall when measured, see PERF.md). A failed
/// call only keeps the pages.
///
/// # Safety
///
/// `slice` must be in a private mapping, and if that is anonymous memory (the
/// records, the keys), nothing may read the slice again: its pages are freed and
/// read as zeros afterwards. A page of a file mapping that this process never
/// wrote (the log) is only dropped from the process: read again, it faults back in
/// from the page cache unchanged. It takes a raw pointer (a slice coerces to one;
/// `parse` builds one over its two adjacent slices), so that the call needs no live
/// reference to the memory.
unsafe fn release<T>(slice: *const [T]) {
    let start = (slice.cast::<T>() as usize).next_multiple_of(PAGE);
    // SAFETY: `slice` lies in one mapping (see # Safety), so its end is in bounds.
    let end = unsafe { slice.cast::<T>().add(slice.len()) } as usize / PAGE * PAGE;
    // SAFETY: madvise only changes how the whole pages inside `slice` are backed,
    // which the caller allows (see # Safety).
    unsafe { madvise(start as *mut u8, end.saturating_sub(start), MADV_DONTNEED) };
}
