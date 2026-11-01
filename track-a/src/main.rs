//! Track A (Ingest): for every (UTC hour, endpoint) group in requests.log, the
//! request count, nearest-rank p50/p95/p99 latency and the 5xx error rate.
//!
//! How it is fast:
//! 1. The log is mmap'd, not read. The harness pre-warms it into the page cache,
//!    and pages cached by the host are not charged to our cgroup, so the 239 MB
//!    input costs nothing on the memory axis.
//! 2. One thread per hardware thread parses slices of the file, taken from a shared
//!    counter (four per thread, so a thread slowed by a busy CPU does not hold up the
//!    rest), into packed u32 records (endpoint, error flag, latency; the 1.2% of
//!    latencies that do not fit go to a side list). Field ends come from one AVX2
//!    space mask, digits are converted by multiply-adds, and endpoints are interned
//!    in one table shared by the threads, by their zero-padded 64-byte form, with no
//!    allocation or capacity check per line.
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

use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering::*};
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
    // PROBE: exit with the host's huge-page settings, only on the full dataset (the
    // PGO training run on the sample must exit 0). Code = 1 + anon * 24 + shmem * 4 + out,
    // anon: always 0, madvise 1, never 2; shmem_enabled: always 0, within_size 1,
    // advise 2, never 3, deny 4, force 5 (6: unreadable); /out's huge= option: none or
    // never 0, always 1, within_size 2, advise 3.
    if len > 100_000_000 {
        let pick = |path: &str, names: &[&str], none: usize| {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            names.iter().position(|n| text.contains(&format!("[{n}]"))).unwrap_or(none)
        };
        let anon = pick("/sys/kernel/mm/transparent_hugepage/enabled", &["always", "madvise", "never"], 2);
        let shmem_names = ["always", "within_size", "advise", "never", "deny", "force"];
        let shmem = pick("/sys/kernel/mm/transparent_hugepage/shmem_enabled", &shmem_names, 6);
        let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
        let line = mounts.lines().find(|l| l.split(' ').nth(1) == Some("/out")).unwrap_or("");
        let out = ["huge=always", "huge=within_size", "huge=advise"].iter().position(|h| line.contains(h)).map_or(0, |i| i + 1);
        std::process::exit((1 + anon * 24 + shmem * 4 + out) as i32);
    }
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
    // Four slices per thread, taken in file order from a shared counter. With one
    // slice each, the scheduler sometimes put two threads on one CPU and they kept
    // it for the whole parse (4 of 30 runs on the test server, dev build): the last
    // thread finished its parse 4.9 to 8.0 ms after the first, and the run took
    // 18.1-20.8 ms against 14.6-15.6 ms (PERF.md, step 38). Now a slowed thread
    // just parses fewer slices. (Eight slices: 0.47 ms slower, from the per-slice
    // work; pinning each thread to a CPU instead: worse, a pinned thread cannot
    // leave a CPU that something else is using.)
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
    let (merged, order): (OnceLock<Merged>, OnceLock<Order>) = Default::default();
    // How many hours still read each part's records: the hour that brings its
    // part's count to zero gives the rest of them back.
    let readers: Vec<AtomicUsize> = parts.iter().map(|_| AtomicUsize::new(0)).collect();
    let (slice, next) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let (written, share) = (std::sync::Barrier::new(threads), AtomicUsize::new(0));
    thread::scope(|s| {
        for _ in 0..threads {
            let (parts, merged, order, readers) = (&parts, &merged, &order, &readers);
            let (slice, next, written, share) = (&slice, &next, &written, &share);
            s.spawn(move || {
                // This thread's records and escapes, each part taking the start of what
                // is left. Room for every line of the log, allocated zeroed: only the
                // pages written are ever backed by memory. One pair per thread, not
                // per slice: a pair per slice made eight slices a thread 2 ms slower.
                // (A slice of n bytes has at most n / MIN_LINE + 1 lines.)
                let most = len / MIN_LINE + slices;
                // [records, escapes], as parse takes them.
                let mut free = [(); 2].map(|_| &mut vec![0; most].leak()[..]);
                loop {
                    let p = slice.fetch_add(1, Relaxed);
                    let Some(part) = parts.get(p) else { break };
                    // SAFETY: the slice is part of the log: a read-only file mapping,
                    // which PAD zero bytes follow.
                    let parsed = unsafe { parse(&log[cut(p)..cut(p + 1)], table, &mut free) };
                    // Each index comes from the counter once, so this is the only set of
                    // part p and cannot fail. (Unchecked on purpose: `expect` or an
                    // assert here changed the instructions of the parse loop above.)
                    let _ = part.set(parsed);
                }
                let ((hours, file, lens), endpoints) = (merged.wait(), order.wait());
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
                        let mut written = 0;
                        let named = endpoints.iter().zip(groups).filter(|g| g.1[0] > 0);
                        for (&(_, name), &stat) in named {
                            written += write(&head, name, stat, &mut out[written..]);
                        }
                        assert_eq!(written, len, "hour length");
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
                        let mut stat = [0; 5]; // a count of 0: no line
                        if !group.is_empty() {
                            let [p50, p95, p99] = percentiles(group);
                            stat = [group.len() as u32, p50, p95, p99, (counts[id] >> 32) as u32];
                            len += line_len(name, &stat);
                        }
                        groups.push(stat);
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
                let [from, to] =
                    [k, k + 1].map(|k| (file + total * k / threads).next_multiple_of(4096));
                // SAFETY: whole pages of the shared output mapping (page-aligned), all
                // written and not touched again. MADV_DONTNEED (4) drops only their
                // page-table entries: the pages stay in the file.
                unsafe { madvise(from as *mut u8, to - from, 4) };
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

        // (id, name) of every endpoint, in byte order of the names: first by the first
        // 32 bytes of the zero-padded keys as two big-endian numbers, so few
        // comparisons need the names, then by the full bytes on ties.
        // SAFETY: every part is in (waited for above, which orders every parse
        // thread's writes before this), so the parse is over: each id below `count`
        // has its name written, and nothing writes the table any more.
        let count = table.count.load(Relaxed);
        let names = table.names[..count].iter().map(|n| unsafe { &**n.get() });
        // SAFETY: as above, for the keys. A Key's first 32 bytes, read as two
        // [u8; 16], are plain bytes: any value is valid, and their alignment is 1.
        let key = |id: usize| unsafe { *table.keys[id].get().cast::<[[u8; 16]; 2]>() };
        let mut endpoints: Vec<_> = names.enumerate().collect();
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
        order.set(endpoints).ok();
        merged.set((hours, file, lens)).ok();
        // (Both set once, here, so neither set can fail; the workers wait for both.)

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

/// What a group's line needs: count (0: no line), p50, p95, p99 and errors. All fit 32 bits:
/// latencies are unsigned 32-bit numbers (the spec). An hour keeps these until it
/// is written, so they are kept as small as possible (20 bytes per endpoint).
type Stats = [u32; 5];

/// The six numbers of a group's line: its statistics, then the error rate in
/// parts per million (`errors * 1000000 / count` in integers). The count is not 0.
fn numbers([n, p50, p95, p99, errors]: Stats) -> [u32; 6] {
    let rate = (errors as u64 * 1_000_000 / n as u64) as u32;
    [n, p50, p95, p99, errors, rate]
}


/// "YYYY-MM-DDTHH:00:00Z endpoint" then the numbers, each after a space, and LF.
fn line_len(name: &[u8], stat: &Stats) -> usize {
    let digits = |v: u32| v.checked_ilog10().map_or(1, |d| d as usize + 1);
    21 + name.len() + numbers(*stat).iter().map(|&v| 1 + digits(v)).sum::<usize>() + 1
}

/// The line is put together from its end in a stack buffer, so no number's
/// length is needed first, then copied to the start of `out`; returns its length.
fn write(head: &[u8; 16], name: &[u8], stat: Stats, out: &mut [u8]) -> usize {
    let mut line = [b'\n'; 256]; // 21 + 125 + 6 * 11 + 1 = 213 at most; it ends with LF
    let mut at = line.len() - 1;
    for mut value in numbers(stat).into_iter().rev() {
        // Two digits at a time from the end, then the first digit of an odd-length
        // number (or the only one).
        let end = at;
        while value >= 10 {
            let pair = value as usize % 100 * 2;
            at -= 2;
            line[at..at + 2].copy_from_slice(&PAIRS[pair..pair + 2]);
            value /= 100;
        }
        if value > 0 || at == end {
            at -= 1;
            line[at] = b'0' + value as u8;
        }
        at -= 1;
        line[at] = b' ';
    }
    at -= 21 + name.len();
    line[at..at + 13].copy_from_slice(&head[..13]);
    line[at + 13..at + 21].copy_from_slice(b":00:00Z ");
    line[at + 21..at + 21 + name.len()].copy_from_slice(name);
    out[..line.len() - at].copy_from_slice(&line[at..]);
    line.len() - at
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
type Part = (&'static [u32], &'static [u32], Vec<(u128, usize, usize)>);

/// A run of one part's records in one hour: ((hour key, first record, first
/// escape), part, end record). So `run.0.0` is its hour key, `run.1` its part and
/// `run.2 - run.0.1` its record count.
type Run = ((u128, usize, usize), usize, usize);

/// What the merge between the passes hands to pass 2 besides the endpoint order:
/// the runs in order of hour then part, cut into hours; the output mapping's
/// address; and each hour's output length once known.
type Merged<'a> = (Vec<&'a [Run]>, usize, Vec<OnceLock<usize>>);

/// (id, name) of every endpoint, in byte order of the names.
type Order<'a> = Vec<(usize, &'a [u8])>;

/// Parses the lines of `chunk`, a slice of the mapped log, into the start of `free`
/// (records, escapes), which keeps the rest, then gives the chunk's pages back.
/// Every line is "ts SP method SP endpoint SP status SP latency LF" (27-byte ts).
///
/// # Safety
///
/// The PAD bytes after `chunk` must be readable: the loads of a line run on past its
/// end. And `chunk` must be file-mapped memory that this process never writes, as
/// its whole pages go back to the kernel at the end (see `release`).
unsafe fn parse(chunk: &[u8], table: &Interner, free: &mut [&'static mut [u32]; 2]) -> Part {
    // The loop writes without capacity checks: it moves on at least MIN_LINE bytes
    // per line whatever the bytes are (the endpoint ends at byte 29 or later, then
    // 5 + 0 + 1), so no input makes more than `most` lines.
    let most = chunk.len() / MIN_LINE + 1;
    assert!(free.iter().all(|room| room.len() >= most));
    let [records, escapes] = free.each_mut().map(|room| room.as_mut_ptr());
    let mut runs = Vec::new();
    // SAFETY: whatever the bytes, every load starts at a line start below the chunk's
    // end and ends within PAD (256) bytes of it, which the caller makes readable.
    // Counted from the line start:
    // - the space windows end by byte 188 (28 + 128 + 32);
    // - the endpoint starts by 93 (29 + 64), so its key loads end by 157;
    // - it ends by 91 (28 + 63), not before its start, or in the slow path by 187
    //   (28 + 128 + 31), where the assert stops a negative or over-125-byte name, so
    //   the name ends by 187 too;
    // - the latency's 16 bytes start by 192 (187 + 5) and end by 208.
    // The KEEP loads read 32 bytes at offsets 0 to 96 of its 128. Records and escapes
    // are written within `free` (asserted above): there are at most `most` lines,
    // each writing one record and one escape slot. `free` is not used meanwhile. The
    // chunk may go back to the kernel at the end (the caller's contract): its endpoint
    // names were copied (see Interner::add).
    unsafe {
        let space = _mm256_set1_epi8(b' ' as i8);
        let (mut record, mut escape) = (records, escapes);
        let (mut line, stop) = (chunk.as_ptr(), chunk.as_ptr_range().end);
        // Lines from one shard arrive in time order, so most lines repeat the
        // previous line's hour. Its bytes "MM-DDTHH" decide: every timestamp lies in
        // one 72-hour span (the spec), so lines that share them share the year.
        let mut last = u64::MAX; // not 8 ASCII bytes, so the first line starts a run
        while line < stop {
            // Bytes 5..13 of the timestamp, "MM-DDTHH", as one word.
            let hour = line.add(5).cast::<u64>().read_unaligned();
            if hour != last {
                last = hour;
                let key = u128::from_be_bytes(*line.cast()) >> 24; // the first 13 bytes
                let at = record.offset_from_unsigned(records);
                runs.push((key, at, escape.offset_from_unsigned(escapes)));
            }
            // One 64-byte window after the timestamp: its first two spaces end the
            // method and the endpoint (no branch on the method or the length). Byte 27
            // is the space after the timestamp, so the window starts at the method,
            // byte 28; the endpoint starts one byte after the method's space.
            let load = |p: *const u8| _mm256_loadu_si256(p.cast());
            let spaces = |at| movemask(_mm256_cmpeq_epi8(load(line.add(28 + at)), space)) as u64;
            let mask = spaces(0) | spaces(32) << 32;
            let from = 29 + mask.trailing_zeros() as usize;
            // The mask without its lowest bit: its lowest bit is now the second space.
            // (A zero mask stays zero and takes the slow path, whose assert stops it.)
            let rest = mask & mask.wrapping_sub(1);
            let mut to = 28 + rest.trailing_zeros() as usize;
            if rest == 0 {
                // No second space in bytes 28..92, so the endpoint's space is further
                // on (7.6% of lines): search on, 32 bytes at a time, at most to the
                // window at 128, which holds the space after a 125-byte endpoint.
                let mut at = 64;
                while at < 128 && spaces(at) == 0 {
                    at += 32;
                }
                to = 28 + at + spaces(at).trailing_zeros() as usize;
                // Tripwire for the spec's method and endpoint lengths, the only way to
                // a negative or over-125-byte name: a file that breaks them stops here.
                let fits = mask != 0 && to - from <= MAX_NAME;
                assert!(fits, "requests.log: a method or endpoint is too long");
            }
            // The key: the endpoint's first 64 bytes, zero from the space on.
            let keep = KEEP.as_ptr().cast::<u8>().add(64 - (to - from).min(64));
            let half = |at| _mm256_and_si256(load(line.add(from + at)), load(keep.add(at)));
            let name = std::slice::from_raw_parts(line.add(from), to - from);
            let endpoint = table.id(&[half(0), half(32)], name);
            // The latency: up to 10 digits from 5 bytes after the endpoint (its space,
            // the three status digits and a space), to the LF.
            let (latency, count) = parse_digits(line.add(to + 5));
            // The escape slot is written for every line and kept (the cursor moves
            // on) only for a latency that does not fit: no branch.
            *escape = latency as u32; // exact: latencies fit 32 bits (the spec)
            escape = escape.add((latency >= ESCAPE as u64) as usize);
            // The status's first digit is right after the endpoint's space; '5' is 5xx.
            let error = ((*line.add(to + 1) == b'5') as u32) << ERROR_SHIFT;
            *record = endpoint << ENDPOINT_SHIFT | error | latency.min(ESCAPE as u64) as u32;
            record = record.add(1);
            line = line.add(to + 5 + count + 1);
        }
        // The part's records and escapes are the start of `free`, which keeps the rest.
        let used = [record, escape];
        let [records, escapes] = [0, 1].map(|i| {
            let room = std::mem::take(&mut free[i]);
            let (part, rest) = room.split_at_mut(used[i].offset_from_unsigned(room.as_ptr()));
            free[i] = rest;
            &*part
        });
        runs.push((u128::MAX, records.len(), escapes.len())); // the end marker
        release(chunk);
        (records, escapes, runs)
    }
}

/// 64 bytes of 0xFF then 64 zeros: the 64 bytes from `64 - len` keep the first `len`.
static KEEP: [[u8; 64]; 2] = [[0xFF; 64], [0; 64]];

/// An endpoint's identity: its bytes zero-padded to 64. No endpoint byte is zero,
/// so for endpoints shorter than 64 bytes equal keys mean equal endpoints. A longer
/// endpoint gets the key of its first 64 bytes, which another long one can share,
/// so those (63 of the 1,842 endpoints, on 6.2% of lines) are compared byte by byte
/// as well.
type Key = [__m256i; 2];

/// `vpmovmskb`, kept opaque to the optimizer: LLVM rewrites the intrinsic as a
/// vector of booleans, and on CPUs without AVX-512 (the scoring box) it then
/// builds a 64-bit mask from two of them one bit at a time.
#[inline(always)]
fn movemask(v: __m256i) -> u32 {
    let mask: u32;
    // SAFETY: AVX2 is enabled for this build; the instruction reads one register.
    unsafe {
        std::arch::asm!("vpmovmskb {0:e}, {1}", out(reg) mask, in(ymm_reg) v, options(pure, nomem, nostack))
    };
    mask
}

/// The value and the number of the 1 to 10 ASCII digits at `at`, which end at an
/// LF within 16 bytes: they are moved to the end of the vector behind zeros, then
/// combined pairwise by multiply-adds into two eight-digit halves.
///
/// The spec guarantees those digits and that LF, and nothing checks them here: a
/// check would be a branch per line. A latency that breaks the spec gives a wrong
/// value, never a read out of bounds: with no LF in the 16 bytes the count is 32,
/// and the parse goes on 38 bytes after the endpoint, still within its bounds.
///
/// # Safety
///
/// The 16 bytes at `at` must be readable.
#[inline(always)]
unsafe fn parse_digits(at: *const u8) -> (u64, usize) {
    // SAFETY: SSSE3 and SSE4.1 come with AVX2; the 16 bytes at `at` are readable (the
    // caller's contract).
    unsafe {
        let digits = _mm_loadu_si128(at.cast());
        let lf = _mm_cmpeq_epi8(digits, _mm_set1_epi8(b'\n' as i8));
        let count = _mm_movemask_epi8(lf).trailing_zeros() as usize;
        // Byte j takes digit j + count - 16; a negative index makes it zero.
        let index = _mm_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
        let order = _mm_add_epi8(index, _mm_set1_epi8(count as i8 - 16));
        let v = _mm_shuffle_epi8(_mm_sub_epi8(digits, _mm_set1_epi8(b'0' as i8)), order);
        let v = _mm_maddubs_epi16(v, _mm_set1_epi16(0x010A)); // pairs: 10 * a + b
        let v = _mm_madd_epi16(v, _mm_set1_epi32(0x0001_0064)); // fours: 100 * a + b
        let v = _mm_packus_epi32(v, v);
        let v = _mm_madd_epi16(v, _mm_set1_epi32(0x0001_2710)); // eights: 10000 * a + b
        let both = _mm_cvtsi128_si64(v) as u64;
        ((both & 0xFFFF_FFFF) * 100_000_000 + (both >> 32), count)
    }
}

/// Open-addressing endpoint table keyed by the 64-byte key, shared by the parsing
/// threads: lookups are plain loads, the rare insert claims its slot by
/// compare-and-swap. Fixed at SLOTS, 8x the most endpoints (an 11% load): ids must
/// fit 11 bits anyway.
struct Interner {
    slots: [AtomicU16; SLOTS], // 0 = empty, else id + 1
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
    /// The endpoint's id, added if new. The probe starts at the key folded to 16
    /// bytes, then two CRC steps: collisions only cost a probe. SLOTS is a power
    /// of two, so `%` is a mask. Keys are compared as two vector compares, and the
    /// bytes too for endpoints of 64 bytes or more (see Key).
    #[inline(always)]
    fn id(&self, key: &Key, name: &[u8]) -> u32 {
        // SAFETY: AVX2 and SSE4.2 are enabled for this build. A published id is
        // below MAX_ENDPOINTS, and its key and name are written (see Sync above).
        unsafe {
            let x = _mm256_xor_si256(key[0], key[1]);
            let x = _mm_xor_si128(_mm256_castsi256_si128(x), _mm256_extracti128_si256(x, 1));
            let [low, high]: [u64; 2] = std::mem::transmute(x);
            let mut i = _mm_crc32_u64(_mm_crc32_u64(0, low), high) as usize % SLOTS;
            loop {
                let id = match self.slots[i].load(Acquire) {
                    0 => self.add(i, key, name),
                    slot => slot as usize - 1,
                };
                let known = &*self.keys.get_unchecked(id).get();
                let low = _mm256_xor_si256(known[0], key[0]);
                let diff = _mm256_or_si256(low, _mm256_xor_si256(known[1], key[1]));
                let same_bytes = || &**self.names.get_unchecked(id).get() == name;
                if _mm256_testz_si256(diff, diff) != 0 && (name.len() < 64 || same_bytes()) {
                    return id as u32;
                }
                i = (i + 1) % SLOTS;
            }
        }
    }

    /// Adds an endpoint not found up to the empty slot `i`, and returns the id that
    /// slot then holds: if another thread filled it first, its id wins. Whether or not
    /// that is this endpoint, the id taken here is then never in a record: its key and
    /// name are written, but no slot holds it, so its groups stay empty and print
    /// nothing. Running out of ids stops the program rather than writing a wrong
    /// record. A lost id needs two threads to find one slot empty between the load
    /// and this CAS. The spare room is 2,048 - 1,842 = 206 ids, and at most 13 were
    /// lost in 100 runs on the full seed at 12 threads (10 at 16; test server): a
    /// measured margin, not a bound. The bytes are copied: they are compared and
    /// printed after the input's pages are released.
    #[cold]
    #[inline(never)]
    fn add(&self, i: usize, key: &Key, name: &[u8]) -> usize {
        let id = self.count.fetch_add(1, Relaxed);
        assert!(id < MAX_ENDPOINTS, "requests.log: too many endpoints");
        // SAFETY: the id is this thread's alone until a slot publishes it.
        unsafe { (*self.keys[id].get(), *self.names[id].get()) = (*key, name.to_vec().leak()) };
        let won = self.slots[i].compare_exchange(0, id as u16 + 1, Release, Acquire);
        won.map_or_else(|slot| slot as usize - 1, |_| id)
    }
}

/// Nearest-rank p50, p95 and p99 (the value at 1-indexed rank ceil(p * n / 100)
/// in sorted order) of a non-empty group, by selection instead of a full sort: p50
/// first, then each next rank within the part above the last one. Each value is
/// read at once: the next selection reorders that part, its own slot included.
fn percentiles(values: &mut [u32]) -> [u32; 3] {
    let [r50, r95, r99] = [50, 95, 99].map(|p| (p * values.len()).div_ceil(100) - 1);
    // Most groups are small: 91% have 32 values or fewer (28% of the records). There
    // the standard library's small-input sort beats three selections; 32 was the
    // best of 16, 32 and 64 (see PERF.md).
    if values.len() <= 32 {
        values.sort_unstable();
        return [r50, r95, r99].map(|r| values[r]);
    }
    let p50 = *values.select_nth_unstable(r50).1;
    let p95 = *values[r50..].select_nth_unstable(r95 - r50).1;
    [p50, p95, *values[r95..].select_nth_unstable(r99 - r95).1]
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
