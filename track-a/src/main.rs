//! Track A (Ingest): for every (UTC hour, endpoint) group in requests.log, the
//! request count, nearest-rank p50/p95/p99 latency and the 5xx error rate.
//!
//! How it is fast:
//! 1. The log is mmap'd, not read. The harness pre-warms it into the page cache,
//!    and pages cached by the host are not charged to our cgroup, so the 239 MB
//!    input costs nothing on the memory axis.
//! 2. One thread per hardware thread parses slices of the file, taken from a shared
//!    counter two at a time (four pairs per thread, so a thread slowed by a busy CPU
//!    does not hold up the rest), one line of each slice in turn, into packed u32
//!    records (endpoint, error flag, latency; the 1.2% of latencies that do not fit
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
//!    that no hour reads any more, and selects exact percentiles per group.
//! 5. Each hour is formatted straight into the output file, mapped shared, as soon
//!    as the hours before it have their lengths; a thread takes further hours
//!    meanwhile. Records shrink as the output grows. Once all of it is written, the
//!    threads drop its page-table entries in parallel, so the exit does not.

#[cfg(not(target_feature = "avx2"))]
compile_error!("track-a needs AVX2: build with RUSTFLAGS=\"-C target-cpu=native\"");

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::*};
use std::{arch::x86_64::*, fs::File, io::Error, os::fd::AsRawFd, sync::OnceLock, thread};

/// A record is `endpoint id << 21 | error << 20 | latency`: 11 bits of endpoint id,
/// bit 20 set for a 5xx status, and 20 bits of latency. A latency of ESCAPE or more
/// is stored as ESCAPE, its value appended to the part's escapes (1.2% of lines).
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
const MIN_LINE: usize = 35;
/// The longest endpoint the spec allows, in bytes; the parse stops on a longer one.
const MAX_NAME: usize = 125;
/// An output line starts with the hour label "YYYY-MM-DDTHH:00:00Z" and a space.
const LABEL: usize = 21;

fn main() {
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
        return; // the spec's empty input gives an empty summary
    };
    // The log followed by PAD zero bytes: the whole range is reserved as zero
    // pages, then the file is mapped over its start (the rest of the file's last
    // page reads as zero). The dataset is mounted read-only, so the bytes cannot
    // change under us.
    let (anonymous, fixed) = (MAP_PRIVATE | MAP_ANONYMOUS, MAP_PRIVATE | MAP_FIXED);
    let base = map(std::ptr::null_mut(), len + PAD, PROT_READ, anonymous, -1);
    // SAFETY: MAP_FIXED replaces only the start of the reservation just made, which
    // nothing uses yet; the assert checks that the call did.
    let at = unsafe { mmap(base, len, PROT_READ, fixed, input.as_raw_fd(), 0) };
    assert!(at == base, "mmap the log: {}", Error::last_os_error());
    // SAFETY: `len + PAD` bytes are mapped, read-only and unchanging (see above), and
    // stay mapped until the parse is over; `log` is not used after that (see munmap).
    let log: &[u8] = unsafe { std::slice::from_raw_parts(base, len) };

    // Pass 1, parallel: the threads parse slices of the log, cut at line ends, into
    // packed records, with one endpoint table shared by all threads. Cut p is just
    // after the last LF before byte len * p / slices (0 if there is none; the last
    // cut is the end of the log, which ends with an LF). Cuts never decrease, so the
    // slices hold every line once; two cuts in one line give an empty slice.
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    // Two pairs of slices per thread, taken in file order from a shared counter.
    // With one slice each, the scheduler sometimes put two threads on one CPU and
    // they kept it for the whole parse (4 of 30 runs on the test server, dev build):
    // the last thread finished its parse 4.9 to 8.0 ms after the first, and the run
    // took 18.1-20.8 ms against 14.6-15.6 ms (PERF.md, step 38). Now a slowed thread
    // just parses fewer slices. (Four pairs a thread: 0.3 ms slower on the test
    // server, 61 interleaved runs, from the per-slice work, mostly each slice's
    // release; pinning each thread to a CPU instead: worse, a pinned thread cannot
    // leave a CPU that something else is using.) A pair is parsed side by side (see
    // `parse`); the count is even, so a pair's second slice always exists.
    let slices = threads * 4;
    // cut(p) may read the end of slice p - 1 after its pages were released: they
    // fault back in from the page cache unchanged (see release).
    let lf = |p| log[..len * p / slices].iter().rposition(|&b| b == b'\n');
    let cut = |p| lf(p).map_or(0, |i| i + 1);
    // SAFETY: all-zero bytes are an empty table: every field is an atomic, a SIMD
    // vector or a raw slice pointer (null, length 0), all valid as zeros.
    // Allocated zeroed, its pages are mapped as the parse first touches them.
    let table: &Interner = Box::leak(unsafe { Box::new_zeroed().assume_init() });
    // One pool of threads for both passes: each parses slices, then waits for
    // the merge below (done on this thread once every part is in) and goes on with
    // pass 2, so no thread is joined or started between the passes.
    let parts: Vec<OnceLock<Part>> = (0..slices).map(|_| OnceLock::new()).collect();
    let merged: OnceLock<Merged> = OnceLock::new();
    // How many hours still read each part's records: the hour that brings its
    // part's count to zero gives the rest of them back.
    let readers: Vec<AtomicUsize> = parts.iter().map(|_| AtomicUsize::new(0)).collect();
    let (slice, next) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let (written, share) = (std::sync::Barrier::new(threads), AtomicUsize::new(0));
    thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                // This thread's records, each part taking the start of what is left.
                // Room for every line of the log, allocated zeroed: only the pages
                // written are ever backed by memory. One mapping per thread, not per
                // slice: a mapping per slice made eight slices a thread 2 ms slower.
                // (A slice of n bytes has at most n / MIN_LINE + 1 lines.)
                let most = len / MIN_LINE + slices;
                let mut free = &mut vec![0; most].leak()[..];
                let claim = || slice.fetch_add(2, Relaxed);
                for p in std::iter::repeat_with(claim).take_while(|&p| p < slices) {
                    let pair = [&log[cut(p)..cut(p + 1)], &log[cut(p + 1)..cut(p + 2)]];
                    // SAFETY: the slices are part of the log: a read-only file mapping,
                    // which PAD zero bytes follow.
                    let [a, b] = unsafe { parse(pair, table, &mut free) };
                    // Each index comes from the counter once, so these are the only sets
                    // of parts p and p + 1 and cannot fail. (Unchecked on purpose:
                    // `expect` or an assert here changed the instructions of the parse
                    // loop above.)
                    let _ = (parts[p].set(a), parts[p + 1].set(b));
                }
                let (hours, file, lens, endpoints) = merged.wait();
                // Where an hour's output starts, once the lengths of all earlier hours are known.
                let offset = |hour| lens[..hour].iter().try_fold(0, |at, n| Some(at + n.get()?));
                // Writes the pending hours whose start is known.
                let flush = |pending: &mut Vec<(usize, usize, Vec<Stats>)>| {
                    pending.retain(|&(hour, len, ref groups)| {
                        let Some(at) = offset(hour) else { return true };
                        // SAFETY: bytes at..at + len of the mapping are this hour's alone, and
                        // at + len <= bound: every line is at most 214 bytes.
                        let out = unsafe { std::slice::from_raw_parts_mut((file + at) as _, len) };
                        let head = (hours[hour][0].0.0 << 24).to_be_bytes(); // its first 13 bytes
                        // Last line first, each one ending where the next one starts.
                        let mut at = len;
                        let named = endpoints.iter().zip(groups).filter(|g| g.1[0] > 0);
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
                // The number of records in hour h: each run's end minus its first record.
                let size = |h: usize| hours[h].iter().map(|&((_, at, _), _, to)| to - at).sum();
                let largest = (0..hours.len()).map(size).max().unwrap_or(0);
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
                    // start of id i's group and place each latency at it: start[i]
                    // then runs from id i's first value to its end.
                    counts.fill(0);
                    for &((_, from, _), p, to) in runs {
                        for &record in &parts[p].wait().0[from..to] {
                            let id = (record >> ENDPOINT_SHIFT) as usize;
                            counts[id] += 1 | ((record >> 20 & 1) as u64) << 32;
                        }
                    }
                    // The low half of a count word is the record count (`as u32`).
                    let mut sum = 0;
                    for (at, &count) in start[1..].iter_mut().zip(&counts) {
                        (*at, sum) = (sum, sum + count as u32);
                    }
                    let values = &mut values[..sum as usize];
                    for (k, &((_, from, escape), p, to)) in runs.iter().enumerate() {
                        let (records, escapes, _) = parts[p].wait();
                        let mut escapes = escapes[escape..].iter();
                        for &record in &records[from..to] {
                            let latency = match record & ESCAPE {
                                ESCAPE => *escapes.next().unwrap(),
                                latency => latency,
                            };
                            let at = &mut start[(record >> ENDPOINT_SHIFT) as usize + 1];
                            // SAFETY: each id's cursor stays below the next id's
                            // start, which is at most values.len(): the counts
                            // above are this hour's.
                            unsafe { *values.get_unchecked_mut(*at as usize) = latency };
                            *at += 1;
                        }
                        // The records are no longer needed once placed: the hour that
                        // reads a part's last ones gives them all back. Parts are small
                        // (a 48th of the log), so one call per part is soon enough, and
                        // every call is a round of TLB shootdowns. Runs of one part are
                        // adjacent: after its last one, the part has one reader less.
                        // (A part's escapes are a few KB: no whole page to give back.)
                        let last = runs.get(k + 1).is_none_or(|run| run.1 != p);
                        if last && readers[p].fetch_sub(1, AcqRel) == 1 {
                            // SAFETY: anonymous memory (see `free`) that every hour reading
                            // it has placed, so nothing reads it again.
                            unsafe { release(*records) };
                        }
                    }
                    // The groups in byte order of their names: one entry per
                    // endpoint, in the order of `endpoints`, so no name is stored.
                    let (mut groups, mut len) = (Vec::with_capacity(endpoints.len()), 0);
                    for &(id, name) in endpoints {
                        let group = &mut values[start[id] as usize..start[id + 1] as usize];
                        groups.push(stats(group, (counts[id] >> 32) as u32));
                        len += line_len(name, groups.last().unwrap());
                    }
                    lens[hour].set(len).unwrap();
                    pending.push((hour, len, groups));
                    flush(&mut pending);
                }
                drop(values); // not needed while the last hours wait to be written
                // Sleep until the hours before this thread's last one have their
                // lengths, then write the rest. (A yield loop here spun on CPUs whose
                // other hardware thread was still computing an hour.)
                let last = pending.iter().map(|p| p.0).max().unwrap_or(0);
                lens[..last].iter().for_each(|n| _ = n.wait());
                flush(&mut pending);
                // Once every thread is here the output is written: each then drops the
                // page-table entries of a share of its mapping (the pages stay in the
                // file), which the exit would do for all of it on one core (0.6 ms on
                // the test server). Dropped while other threads still wrote, they slowed
                // those writes by as much (apost-1 work). Shares are cut at pages.
                written.wait();
                let total = offset(lens.len()).unwrap();
                let k = share.fetch_add(1, Relaxed);
                let at = |k: usize| (file + total * k / threads).next_multiple_of(PAGE);
                // SAFETY: whole pages of the shared output mapping (page-aligned), all
                // written and not touched again. MADV_DONTNEED drops only their
                // page-table entries: the pages stay in the file.
                unsafe { madvise(at(k) as *mut u8, at(k + 1) - at(k), MADV_DONTNEED) };
            });
        }

        // Every part's runs, in order of hour then part, gathered as the parts come in.
        // Two adjacent run starts of a part make one run: (the first start, the part,
        // the second's first record, where the run ends).
        let mut runs = Vec::new();
        for (p, part) in parts.iter().enumerate() {
            runs.extend(part.wait().2.windows(2).map(|w| (w[0], p, w[1].1)));
        }
        runs.sort_unstable_by_key(|&((hour, ..), part, _)| (hour, part));
        // The runs of one hour in one part are now adjacent: each such group is one
        // reader of the part (hour a, part p against hour b, part q).
        for runs in runs.chunk_by(|((a, ..), p, _), ((b, ..), q, _)| (a, p) == (b, q)) {
            readers[runs[0].1].fetch_add(1, Relaxed);
        }
        // Cut into hours. Leaked: pass 2 reads the runs until the exit.
        let same_hour = |((a, ..), ..): &Run, ((b, ..), ..): &Run| a == b;
        let hours: Vec<&[Run]> = runs.leak().chunk_by(same_hour).collect();

        // (id, name) of every endpoint (ids start at 1), in byte order of the names:
        // first by the first 32 bytes of the keys as two big-endian numbers, so few
        // comparisons need the names, then by the full bytes on ties. A key has a
        // space after its endpoint, below every endpoint byte, then zeros, so a
        // proper prefix still sorts first.
        // SAFETY: every part is in (waited for above, which orders every parse
        // thread's writes before this), so the parse is over: each id up to `count`
        // has its name written, and nothing writes the table any more.
        let count = table.count.load(Relaxed);
        let name = |id: usize| unsafe { &**table.names[id].get() };
        // SAFETY: as above, for the keys. A Key's first 32 bytes, read as two
        // [u8; 16], are plain bytes: any value is valid, and their alignment is 1.
        let key = |id: usize| unsafe { *table.keys[id].get().cast::<[[u8; 16]; 2]>() };
        let mut endpoints: Vec<_> = (1..=count).map(|id| (id, name(id))).collect();
        endpoints.sort_unstable_by_key(|&(id, name)| (key(id).map(u128::from_be_bytes), name));

        // The output is mapped shared at a size no output can exceed, and cut to its
        // length at the end. On tmpfs, where /out is, one write call per hour would
        // serialise on the file's lock; page faults on the mapping do not. A line is
        // at most 213 bytes, and the bound allows 214 (a byte to spare): the 20-byte
        // label "YYYY-MM-DDTHH:00:00Z" and its space, the name (at most 125 bytes, checked by
        // the parse), six numbers of at most 10 digits after a space each, and LF.
        let bound = hours.len() * endpoints.len() * (21 + 1 + 125 + 6 * 11 + 1);
        output.set_len(bound as u64).expect("size the summary");
        // Kept as an address: a raw pointer could not be shared by the threads,
        // which each write only their own hours' bytes.
        let file = map(std::ptr::null_mut(), bound, 3, 1, output.as_raw_fd()) as usize; // READ | WRITE; SHARED
        let lens = hours.iter().map(|_| OnceLock::new()).collect();
        merged.set((hours, file, lens, endpoints)).ok(); // set once, here: it cannot fail

        // Off the critical path, while pass 2 runs: the parse threads gave back the
        // log's pages, but not the page tables that mapped them (0.5 MB, charged to
        // us): unmapping frees those too. A failure would only keep them.
        // SAFETY: every part is in, so nothing reads the log any more: every
        // endpoint's bytes and key were copied, and `log` is not used again.
        unsafe { munmap(base, len + PAD) };
        // SAFETY: the keys (anonymous memory, in the table) are only looked up during
        // the parse and the sort above, so nothing reads them again; names are kept.
        unsafe { release(&table.keys[..]) };
    });
    let lens = &merged.get().unwrap().2;
    let total: usize = lens.iter().map(|len| len.get().unwrap()).sum();
    output.set_len(total as u64).expect("size the summary");
    // The output is complete in the page cache. Exit now: dropping the tables
    // above, one free at a time, took a few milliseconds on the scoring box.
    std::process::exit(0)
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
/// copied. (Put together in a stack buffer and copied, the copy's wide loads waited
/// for the buffer's byte stores to reach the cache: a store-forwarding stall a line.)
fn write(head: &[u8; 16], name: &[u8], stat: Stats, out: &mut [u8], end: usize) -> usize {
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
    // The 20-byte label (the hour key's 13 bytes "YYYY-MM-DDTHH", then ":00:00Z"), its
    // space, then the name.
    at -= LABEL + name.len();
    out[at..at + 13].copy_from_slice(&head[..13]);
    out[at + 13..at + LABEL].copy_from_slice(b":00:00Z ");
    out[at + LABEL..at + LABEL + name.len()].copy_from_slice(name);
    at
}

/// "00", "01", ..., "99".
static PAIRS: &[u8; 200] = b"000102030405060708091011121314151617181920212223242526272829303132\
    33343536373839404142434445464748495051525354555657585960616263646566676869707172737475767778\
    798081828384858687888990919293949596979899";

/// One thread's parse of its slice: one record per line, in file order (see
/// ENDPOINT_SHIFT); the latencies that did not fit, in the same order; and where
/// each run of lines in one hour starts, as (hour key, first record, first
/// escape), then an end marker. So `.0` is the records, `.1` the escapes and `.2`
/// the run starts.
type Part = (&'static [u32], Vec<u32>, Vec<(u128, usize, usize)>);

/// A run of one part's records in one hour: ((hour key, first record, first
/// escape), part, end record). So `run.0.0` is its hour key, `run.1` its part and
/// `run.2 - run.0.1` its record count.
type Run = ((u128, usize, usize), usize, usize);

/// What the merge between the passes hands to pass 2: the runs in order of hour
/// then part, cut into hours; the output mapping's address; each hour's output
/// length once known; and the endpoints in byte order of their names.
type Merged<'a> = (Vec<&'a [Run]>, usize, Vec<OnceLock<usize>>, Order<'a>);

/// (id, name) of every endpoint, in byte order of the names.
type Order<'a> = Vec<(usize, &'a [u8])>;

/// Parses two slices of the mapped log side by side, one line of each in turn: a
/// line's end, and so the next line's start, waits on its loads, and the other
/// slice's line fills the wait. The records go into the start of `free`, which keeps
/// the rest; then the slices' pages are given back. Every line is
/// "ts SP method SP endpoint SP status SP latency LF" (27-byte ts).
///
/// # Safety
///
/// The PAD bytes after each slice must be readable: the loads of a line run on past
/// its end. And the slices must be file-mapped memory that this process never
/// writes, as their whole pages go back to the kernel at the end (see `release`).
unsafe fn parse(chunks: [&[u8]; 2], table: &Interner, free: &mut &'static mut [u32]) -> [Part; 2] {
    // The loop writes without capacity checks: it moves on at least MIN_LINE bytes
    // per line whatever the bytes are (the endpoint ends at byte 29 or later, then
    // 5 + 0 + 1), so no slice has more lines than its room. The second slice's
    // records start after room for the first one's.
    let most = chunks.map(|chunk| chunk.len() / MIN_LINE + 1);
    assert!(free.len() >= most[0] + most[1]);
    let (first, second) = std::mem::take(free).split_at_mut(most[0]);
    // Each slice's next line and its end, its records' start and next place, the
    // previous line's hour bytes, the latencies that do not fit a record, its runs.
    let mut line = chunks.map(|c| c.as_ptr());
    let stop = chunks.map(|c| c.as_ptr_range().end);
    let start = [first.as_mut_ptr(), second.as_mut_ptr()];
    let mut record = start;
    let (mut last, mut escapes, mut runs) = ([u64::MAX; 2], [vec![], vec![]], [vec![], vec![]]);
    // (u64::MAX is not 8 ASCII bytes, so each slice's first line starts a run.)
    // SAFETY: AVX2 is enabled for this build.
    let space = unsafe { _mm256_set1_epi8(b' ' as i8) };
    // Parses slice k's next line into one record, if there is one.
    // SAFETY: whatever the bytes, every load starts at a line start below its
    // slice's end and ends within PAD (256) bytes of it, which the caller makes
    // readable. Counted from the line start:
    // - the space window ends by byte 92 (28 + 64);
    // - the endpoint starts by 93 (29 + 64); the inline lookup reads from 8 bytes
    //   before it to 64 after it (by 157), the long way 128 after it (by 221);
    // - it ends by 91 (28 + 63), not before its start, or in the long way by 218
    //   (93 + 125), where the assert stops an over-125-byte name;
    // - the latency's 8 or 16 bytes start by 223 (218 + 5) and end by 239.
    // Records are written within each slice's room (asserted above): at most
    // `most[k]` lines, one record each. `free` is not used meanwhile. The slices may
    // go back to the kernel at the end (the caller's contract): their endpoint names
    // were copied (see Interner::add).
    let mut step = |k: usize| unsafe {
        let line = &mut line[k];
        if *line >= stop[k] {
            return false;
        }
        // Lines from one shard arrive in time order, so most lines repeat the
        // previous line's hour. Its bytes "MM-DDTHH" (5..13) decide: every timestamp
        // lies in one 72-hour span (the spec), so lines that share them share the year.
        let hour = line.add(5).cast::<u64>().read_unaligned();
        if hour != last[k] {
            last[k] = hour;
            let key = u128::from_be_bytes(*line.cast()) >> 24; // the first 13 bytes
            let at = record[k].offset_from_unsigned(start[k]);
            runs[k].push((key, at, escapes[k].len()));
        }
        // One 64-byte window after the timestamp: its first two spaces end the
        // method and the endpoint (no branch on the method or the length). Byte 27
        // is the space after the timestamp, so the window starts at the method,
        // byte 28; the endpoint starts one byte after the method's space.
        let load = |p: *const u8| _mm256_loadu_si256(p.cast());
        let spaces = |at: usize| movemask(_mm256_cmpeq_epi8(load(line.add(28 + at)), space));
        let window = spaces(0) | spaces(32) << 32;
        let first = window.trailing_zeros() as usize;
        let from = line.add(29 + first);
        // The window without its lowest bit: its lowest bit is now the second space
        // (none: the endpoint goes on past the window, and the long way finds its
        // end; a zero window stays zero and goes the long way too).
        let rest = window & window.wrapping_sub(1);
        let mut len = (rest.trailing_zeros() as usize).wrapping_sub(first + 1);
        // Endpoints that end in the window (92% of lines) are looked up inline; the
        // rest, and the inline lookup's misses, go the long way.
        let (mut endpoint, mut hit) = (0, false);
        if rest != 0 {
            (endpoint, hit) = table.find(&[load(from), load(from.add(32))], from, len);
        }
        if !hit {
            (endpoint, len) = table.id(from);
        }
        // The latency: 1 to 10 digits from 5 bytes after the endpoint (its space,
        // the three status digits and a space), to the LF. Its first 8 bytes,
        // zero-extended: up to 6 digits (99% of lines) are converted where they lie,
        // and fit a record.
        let to = from.add(len);
        let tail = _mm_loadl_epi64(to.add(5).cast());
        let lf = _mm_cmpeq_epi8(tail, _mm_set1_epi8(b'\n' as i8));
        let mut count = _mm_movemask_epi8(lf).trailing_zeros() as usize;
        let latency = if count <= 6 {
            let v = _mm_sub_epi8(tail, _mm_set1_epi8(b'0' as i8));
            let v = _mm_maddubs_epi16(v, _mm_loadu_si128(DIGITS.0.as_ptr().add(8 * count).cast()));
            let v = _mm_madd_epi16(v, _mm_loadu_si128(DIGITS.1.as_ptr().add(4 * count).cast()));
            _mm_cvtsi128_si32(_mm_add_epi32(v, _mm_srli_epi64(v, 32))) as u32
        } else {
            let latency;
            (latency, count) = parse_digits(to.add(5), &mut escapes[k]);
            latency
        };
        // The status's first digit is right after the endpoint's space; '5' is 5xx.
        let error = ((*to.add(1) == b'5') as u32) << ERROR_SHIFT;
        *record[k] = endpoint | error | latency;
        record[k] = record[k].add(1);
        *line = to.add(5 + count + 1);
        true
    };
    while step(0) & step(1) {}
    while step(0) | step(1) {}
    // SAFETY: the slices go back to the kernel (the caller's contract); nothing reads
    // them again.
    chunks.iter().for_each(|chunk| unsafe { release(*chunk) });
    // SAFETY: each slice's records run from its start to its next place.
    let lines = [0, 1].map(|k| unsafe { record[k].offset_from_unsigned(start[k]) });
    let (second, rest) = second.split_at_mut(lines[1]);
    *free = rest;
    for k in 0..2 {
        runs[k].push((u128::MAX, lines[k], escapes[k].len())); // the end markers
    }
    let ([escapes0, escapes1], [runs0, runs1]) = (escapes, runs);
    let first = (&first[..lines[0]], escapes0, runs0);
    [first, (second, escapes1, runs1)]
}

/// 128 bytes of 0xFF then 128 zeros: the 128 bytes from `128 - len` keep the first `len`.
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
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 10, 1, 0, 0, 0, 0, 0, 0, 10, 1, 1, 0, 0, 0, 0, 0, 10, 1, 10, 1, 0, 0, 0, 0, 10, 1, 10, 1, 1, 0, 0, 0, 10, 1, 10, 1, 10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 10, 1, 0, 0, 100, 1, 0, 0, 1000, 10, 1, 0, 10000, 100, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
);

/// An endpoint's identity: its bytes (at most 125), a space, then zeros to 128 bytes.
/// No endpoint byte is a space or zero, so equal keys mean equal endpoints.
type Key = [__m256i; 4];

/// `vpmovmskb`: bit i of the mask is the top bit of byte i of `v`.
#[inline(always)]
fn movemask(v: __m256i) -> u64 {
    // SAFETY: AVX2 is enabled for this build.
    unsafe { _mm256_movemask_epi8(v) as u32 as u64 }
}

/// The record's latency and the number of the 7 to 10 ASCII digits at `at` (1.2% of
/// lines; the parse converts up to 6 itself), one digit at a time up to the LF. A
/// latency that does not fit a record goes to `escapes`, and the record holds ESCAPE.
///
/// The spec guarantees those digits and that LF, and nothing checks them here: a
/// latency that breaks the spec gives a wrong value, never a read out of bounds, as
/// at most the 16 bytes at `at` are read.
///
/// # Safety
///
/// The 16 bytes at `at` must be readable.
#[cold]
#[inline(never)]
unsafe fn parse_digits(at: *const u8, escapes: &mut Vec<u32>) -> (u32, usize) {
    let (mut latency, mut count) = (0u64, 0);
    // SAFETY: count < 16, so each read is one of the 16 readable bytes (the caller's
    // contract).
    while count < 16 && unsafe { *at.add(count) } != b'\n' {
        latency = latency * 10 + unsafe { *at.add(count) }.wrapping_sub(b'0') as u64;
        count += 1;
    }
    if latency >= ESCAPE as u64 {
        escapes.push(latency as u32); // exact: latencies fit 32 bits (the spec)
    }
    (latency.min(ESCAPE as u64) as u32, count)
}

/// Open-addressing endpoint table shared by the parsing threads: lookups are plain
/// loads, the rare insert claims its slot by compare-and-swap. A slot holds an id as
/// a record holds it (`id << ENDPOINT_SHIFT`), so a lookup needs no shift. Ids start
/// at 1: slot 0 means empty, and id 0's key stays zero, which no endpoint matches.
/// Fixed at SLOTS, 8x the most endpoints (an 11% load): ids must fit 11 bits anyway.
struct Interner {
    slots: [AtomicU32; SLOTS], // 0 = empty, else id << ENDPOINT_SHIFT
    keys: [std::cell::UnsafeCell<Key>; MAX_ENDPOINTS],
    names: [std::cell::UnsafeCell<*const [u8]>; MAX_ENDPOINTS], // the endpoints' bytes
    count: AtomicUsize,                                         // ids handed out
}

// SAFETY: an id's key and name are written once, by the thread that took the id,
// before a slot publishes the id (Release). They are read only once a slot holding
// the id has been seen (Acquire), or after the parse.
unsafe impl Sync for Interner {}

const SLOTS: usize = 8 * MAX_ENDPOINTS;

impl Interner {
    /// The slot an endpoint starts its probe at: its length, its first 8 bytes and
    /// its last 8, two CRC steps. SLOTS is a power of two, so `%` is a mask.
    #[inline(always)]
    fn home(len: usize, first: u64, last: u64) -> usize {
        // SAFETY: SSE4.2 is enabled for this build.
        unsafe { _mm_crc32_u64(_mm_crc32_u64(len as u64, first), last) as usize % SLOTS }
    }

    /// `id << ENDPOINT_SHIFT` of the endpoint of `len` bytes (0 to 62) at `from`,
    /// followed by a space, whose first 64 bytes are `bytes`, and whether it is in its
    /// home slot. Under 8 bytes the last 8 bytes reach before `from`, into the
    /// method, so the slot is not the home one: the compare is exact all the same,
    /// and a miss goes the long way.
    ///
    /// # Safety
    ///
    /// The 8 bytes before `from` and the `len` bytes from it must be readable.
    #[inline(always)]
    unsafe fn find(&self, bytes: &[__m256i; 2], from: *const u8, len: usize) -> (u32, bool) {
        // SAFETY: AVX2 and SSE4.2 are enabled; `from - 8 .. from + len` is readable
        // (the caller's contract). A published id is below MAX_ENDPOINTS, and its key
        // is written (see Sync); a slot's value >> (ENDPOINT_SHIFT - 7) is its id
        // times the key's 128 bytes, so `known` is that id's key.
        unsafe {
            let first = _mm_cvtsi128_si64(_mm256_castsi256_si128(bytes[0])) as u64;
            let last = from.add(len).sub(8).cast::<u64>().read_unaligned();
            let id = self.slots[Self::home(len, first, last)].load(Acquire);
            let at = (id >> (ENDPOINT_SHIFT - 7)) as usize;
            let known = &*self.keys.as_ptr().cast::<u8>().add(at).cast::<Key>();
            let same = |i: usize| movemask(_mm256_cmpeq_epi8(bytes[i], known[i]));
            // Bytes 0..=len must match: the endpoint and its space, which the stored
            // key has at its own length.
            let differ = !(same(0) | same(1) << 32) & _bzhi_u64(!0, len as u32 + 1);
            (id, differ == 0)
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
        // SAFETY: AVX2 and SSE4.2 are enabled; the reads stay within the 128 bytes
        // from `from` (the caller's contract) and within KEEP (len <= 125, asserted).
        // A published id is below MAX_ENDPOINTS, and its key and name are written (see
        // Sync).
        unsafe {
            let load = |p: *const u8| _mm256_loadu_si256(p.cast());
            let line: Key = std::array::from_fn(|i| load(from.add(32 * i)));
            let space = _mm256_set1_epi8(b' ' as i8);
            let mask = |i: usize| (movemask(_mm256_cmpeq_epi8(line[i], space)) as u128) << (32 * i);
            // 128 if there is none: a 125-byte endpoint's space is in the 128 bytes.
            let len = (mask(0) | mask(1) | mask(2) | mask(3)).trailing_zeros() as usize;
            // Tripwire for the spec's endpoint length: a file that breaks it stops here.
            assert!(len <= MAX_NAME, "requests.log: an endpoint is too long");
            // The key: the bytes up to and with the space after the endpoint, then zeros.
            let keep = KEEP.as_ptr().cast::<u8>().add(127 - len);
            let key = std::array::from_fn(|i| _mm256_and_si256(line[i], load(keep.add(32 * i))));
            // The same bytes as `find` hashes, from 8 bytes up (under 8 it never finds one).
            let bytes = key.as_ptr().cast::<u8>();
            let at = |i: usize| bytes.add(i).cast::<u64>().read_unaligned();
            let mut i = Self::home(len, at(0), at(len.max(8) - 8));
            loop {
                let id = match self.slots[i].load(Acquire) {
                    0 => self.add(i, &key, std::slice::from_raw_parts(from, len)),
                    slot => (slot >> ENDPOINT_SHIFT) as usize,
                };
                let known = &*self.keys[id].get();
                let diff = |i: usize| _mm256_xor_si256(known[i], key[i]);
                let diff = (1..4).fold(diff(0), |all, i| _mm256_or_si256(all, diff(i)));
                if _mm256_testz_si256(diff, diff) != 0 {
                    return ((id as u32) << ENDPOINT_SHIFT, len);
                }
                i = (i + 1) % SLOTS;
            }
        }
    }

    /// Adds an endpoint not found up to the empty slot `i`, and returns the id that
    /// slot then holds: if another thread filled it first, its id wins, whether or not
    /// that is this endpoint, and the caller probes on. The id taken here is then not
    /// published (no slot or record holds it), and the thread keeps it for its next
    /// insert, which overwrites its key and name. So at most one id per thread is ever
    /// unpublished: 1,842 endpoints and 12 threads need 1,854 of the 2,047 ids, and
    /// running out of ids (more endpoints than the spec allows) stops the program
    /// rather than writing a wrong record. The bytes are copied: they are compared and
    /// printed after the input's pages are released.
    #[cold]
    #[inline(never)]
    fn add(&self, i: usize, key: &Key, name: &[u8]) -> usize {
        // An id that lost its slot stays this thread's, for its next insert: at most
        // one id per thread is ever unpublished.
        thread_local!(static SPARE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
        let id = match SPARE.take() {
            0 => self.count.fetch_add(1, Relaxed) + 1,
            spare => spare,
        };
        assert!(id < MAX_ENDPOINTS, "requests.log: too many endpoints");
        // SAFETY: the id is this thread's alone until a slot publishes it.
        unsafe { (*self.keys[id].get(), *self.names[id].get()) = (*key, name.to_vec().leak()) };
        let won =
            self.slots[i].compare_exchange(0, (id as u32) << ENDPOINT_SHIFT, Release, Acquire);
        if won.is_err() {
            SPARE.set(id);
        }
        won.map_or_else(|slot| (slot >> ENDPOINT_SHIFT) as usize, |_| id)
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
/// pages it shares with memory around it are kept. Used on the log's slices, on a
/// part's records once no hour reads them, and on the endpoint keys after the sort.
/// The parse threads drop the log's pages this way, in parallel: left to the exit,
/// its 58,000 pages are unmapped one by one on one core (1.3 ms slower overall when
/// measured, see PERF.md). A failed call only keeps the pages.
///
/// # Safety
///
/// `slice` must be in a private mapping, and if that is anonymous memory (the
/// records, the keys), nothing may read the slice again: its pages are freed and
/// read as zeros afterwards. A page of a file mapping that this process never
/// wrote (the log) is only dropped from the process: read again, it faults back in
/// from the page cache unchanged. It takes a raw pointer (callers pass a slice,
/// which coerces), so that no reference to the memory is live across the call.
unsafe fn release<T>(slice: *const [T]) {
    // SAFETY: `slice` is one allocation (the caller's slice), so its end is in
    // bounds; madvise only changes how the whole pages inside it are backed, which
    // the caller allows (see # Safety).
    unsafe {
        let start = (slice.cast::<T>() as usize).next_multiple_of(PAGE);
        let end = slice.cast::<T>().add(slice.len()) as usize / PAGE * PAGE;
        madvise(start as *mut u8, end.saturating_sub(start), MADV_DONTNEED)
    };
}
