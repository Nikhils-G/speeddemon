//! Track C (Join): parse the hostile records.csv, join each record to its country
//! through regions.csv, and write per-country aggregates.
//!
//! The parser relies on spec section 2.2 (what the generator writes, for every seed
//! and scale), not only on the general grammar of 2.1. Where 2.2 is relied on, a
//! wrong assumption stops the process with a nonzero exit (an assert, or an index
//! out of bounds) rather than writing a wrong answer.
//!
//! How it is fast, and why it is still exact:
//! 1. Both files are mmap'd (pre-warmed page cache: no memory charge, no copy).
//! 2. The file is cut into runs of whole records without reading it twice. By spec
//!    2.2, an LF inside quotes is followed by a letter or `"`, and a record
//!    terminator by the next record's BOM or id digit, so a cut goes after the first
//!    LF that is followed by a digit or a BOM. As a tripwire, the run before each cut
//!    must end exactly there.
//! 3. Records are found with SIMD bitmaps, as simdjson does for JSON: one bitmap of
//!    `"`, one of `,`, and a carry-less multiply that turns the quote bits into an
//!    "inside quotes" mask. What is left are exactly the field separators, and
//!    counting quote bits gives each text field's unescaped length (each `""` is
//!    one byte) without looking at its bytes. The common record takes one such
//!    window of 64 bytes at its start, one 32-byte compare for the region's comma
//!    and the amount's dot, and one AVX2 pass that converts its three numbers. A
//!    record whose name or note runs past the windows, or one near the end of the
//!    file, goes to a byte-by-byte walker.
//! 4. Distinct names per country. A name's identity is its raw bytes inside the
//!    quotes: there, `""` maps one to one to the unescaped value, and the empty name
//!    is empty whether quoted or not. Parsing hashes those bytes and files a
//!    (hash, group) pair under one of 4 partitions per thread, picked by the hash.
//!    After each run it parses, a thread hands its buffers of pairs to their owners'
//!    inboxes, and its own tables take what waits in its inbox: small tables of
//!    names with a bit per group, which stay in its cache. The tables compare 49.6
//!    of the hash's bits at 12 threads and 46 on one (see `add`): with the
//!    generator's 100,000 names, two of them agree on all those bits with odds of
//!    about 5.9e-6 per dataset at 12 threads, 7.1e-5 on one.
//! 5. No thread waits for a given other one until the end: runs are claimed one at
//!    a time, so a thread slowed down (another process sharing its core) parses
//!    fewer of them, and its partitions' pairs wait in their inboxes. (With a
//!    barrier after each round, one core shared with a busy loop made the run
//!    take twice as long.)

#[cfg(not(all(target_feature = "avx2", target_feature = "pclmulqdq")))]
compile_error!("track-c needs AVX2 and PCLMULQDQ: build with RUSTFLAGS=\"-C target-cpu=native\"");
#[cfg(not(target_feature = "aes"))]
compile_error!("track-c needs AES (the name hash): build with RUSTFLAGS=\"-C target-cpu=native\"");

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::*};
use std::{arch::x86_64::*, fmt::Write, os::fd::AsRawFd, ptr::null_mut, sync::Mutex, thread};

/// Each thread parses this many runs, on average: they are claimed as threads go,
/// so a thread slowed down (another process on its core) parses fewer of them.
/// Pairs wait in buffers for about a run's time, so the buffers hold half as much
/// as with 32 runs (memory peak -0.9 to -1.1 MB, +0.2 ms on the server).
const RUNS_PER_THREAD: usize = 64;
/// The input's pages are dropped in blocks of this many runs, about 2 MB: every
/// call is a round of TLB shootdowns (48 runs in blocks of 4: +0.35 ms).
const BLOCK: usize = 8;
/// Names are split by their top hash bits into this many partitions per thread;
/// a thread's own partitions take their pairs, so their tables stay in its cache.
const PARTITIONS_PER_THREAD: usize = 4;
/// Group indices are bytes, so that tables indexed by a group need no bounds
/// checks: countries in the order they are first seen, `_UNMATCHED` the last one.
const GROUPS: usize = 256;
/// Words of group bits per name. Groups 240..254 are never used (spec 2.3: at most
/// 240 countries; 255 is `_UNMATCHED`), so word 3's bits 48..62 hold 15 more bits
/// of the name's hash.
const WORDS: usize = GROUPS / 64;
/// regions.csv is entered in this many parts, which threads claim as they start,
/// so the ones that start first do more of it.
const REGION_CHUNKS: usize = 64;
/// `fast_record` reads up to 196 bytes past a record's start, so it takes only the
/// records that start at least this far before the end; the walker takes the rest.
const FAST_MARGIN: usize = 256;

/// A group's columns, in the order of the output line (spec section 4), amounts in
/// cents. name_bytes and note_bytes share the last one, `BYTES`, as
/// name_bytes << 32 | note_bytes: each is below 2^31 (spec section 3).
type Totals = [i64; 7];
const COUNT: usize = 0;
const ID_SUM: usize = 1;
const AMOUNT_SUM: usize = 2;
const AMOUNT_MIN: usize = 3;
const AMOUNT_MAX: usize = 4;
const NAMES_DISTINCT: usize = 5;
const BYTES: usize = 6;
/// No records yet: the min and the max start at the far ends.
const EMPTY: Totals = [0, 0, 0, i64::MAX, i64::MIN, 0, 0];

/// One partition's names: an open-addressing table of 32-bit keys (0 = empty) in
/// buckets of 8 slots, at most 5/8 full so that a lookup is nearly always one
/// bucket; and a row of WORDS words of group bits per name, added per new name. A
/// key holds the name's row + 1 (never 0) and hash bits above it, so that a hit
/// needs no second load for the row. (Rows by slot instead, in a 7/8 full table:
/// 23% less memory, but +0.8M branch misses. 64-bit keys: 0.8 MB more.)
type Table = (Vec<u32>, Vec<u64>);

/// One parsed record.
struct Record {
    id: u64,
    region_id: u64,
    /// The amount in cents.
    cents: i64,
    /// name_bytes << 32 | note_bytes, as in the `BYTES` column.
    bytes: u64,
    /// The name's hash (`hash_words` of its raw bytes inside the quotes).
    hash: u64,
    /// Where the next record starts.
    next: usize,
}

/// A field that `field` found: where its value is (inside the quotes, if quoted)
/// and its unescaped length.
struct Field {
    start: usize,
    end: usize,
    len: u64,
}

// libc, declared by hand so that the crate has no dependencies.
// SAFETY: glibc's signatures on x86-64 Linux; prctl is declared variadic, as in C.
unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, at: i64) -> *mut u8;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn mallopt(param: i32, value: i32) -> i32;
    fn prctl(option: i32, ...) -> i32;
}
// Their constants, from the Linux and glibc headers.
const PROT_READ: i32 = 1;
const MAP_PRIVATE: i32 = 2;
const MADV_DONTNEED: i32 = 4;
const M_TOP_PAD: i32 = -2;
const PR_SET_THP_DISABLE: i32 = 41;

/// Errors (a missing file, input outside spec 2.2) are panics: the build aborts on
/// panic, so the message is printed and the exit status is nonzero. (For the same
/// reason no lock is ever poisoned: `lock().unwrap()` cannot fail.)
fn main() {
    // M_TOP_PAD, 32 MB: a worker thread's first allocations get their own heap, which
    // otherwise grows a few pages per mprotect, and each mprotect stalls every other
    // thread's page faults (800 of them per run: -2.8 ms). Reserved but untouched
    // pages cost no memory.
    // SAFETY: a libc call that touches no memory of ours; a failure keeps the default.
    unsafe { mallopt(M_TOP_PAD, 32 << 20) };
    // PR_SET_THP_DISABLE: with transparent huge pages, touching one byte of a sparse
    // table can fault in a whole 2 MB page, so the memory peak would depend on how
    // fragmented the host is (13 to 24 MB between runs).
    // SAFETY: a system call that touches no memory of ours; a failure keeps the default.
    unsafe { prctl(PR_SET_THP_DISABLE, 1u64, 0u64, 0u64, 0u64) };
    let dir = |k: usize, or: &str| std::env::args().nth(k).unwrap_or(or.into());
    run(&dir(1, "/data"), &dir(2, "/out"))
}

/// Maps a whole file read-only for the rest of the process. It is never unmapped:
/// the process exits right after using it, and exit unmaps everything.
fn map(path: &str) -> &'static [u8] {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let len = file.metadata().expect(path).len() as usize; // the panic names the file
    // SAFETY: a fresh read-only private mapping of `len` bytes where the kernel
    // chooses; the dataset is mounted read-only, so the bytes cannot change under us.
    let ptr = unsafe { mmap(null_mut(), len, PROT_READ, MAP_PRIVATE, file.as_raw_fd(), 0) };
    assert_ne!(ptr as isize, -1, "mmap {path}");
    // SAFETY: the mapping succeeded (above), is valid for `len` bytes, and is never
    // written or unmapped (closing the file does not unmap it), so the slice can
    // live until exit.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// Drops the whole pages inside `bytes` from the page tables. Exit would otherwise
/// unmap the input's 50,000 pages one by one, on one core (6 ms on the box); this
/// does it in each parsing thread, in parallel. Best effort: if the call fails, the
/// pages just stay.
///
/// # Safety
/// `bytes` must lie in a private file mapping that is never written, as `map`'s
/// are: a dropped page then faults back in from the page cache with the same bytes.
/// (Anonymous or written memory would read back as zeros.)
unsafe fn drop_pages(bytes: &[u8]) {
    // madvise needs page-aligned bounds; x86-64 pages are 4096 bytes.
    let start = (bytes.as_ptr() as usize).next_multiple_of(4096);
    let end = (bytes.as_ptr() as usize + bytes.len()) / 4096 * 4096;
    // SAFETY: MADV_DONTNEED on the whole pages inside `bytes` (none if it holds no
    // whole page), which read back the same by the caller's contract.
    unsafe { madvise(start as *mut u8, end.saturating_sub(start), MADV_DONTNEED) };
}

/// `n` default values: empty tables, inboxes and zeroed counters.
fn many<T: Default>(n: usize) -> Vec<T> {
    (0..n).map(|_| T::default()).collect()
}

/// Spins for a while, then yields the core between checks, until `count` reaches
/// `n`. The waits (for regions.csv, and for the last runs) are short, and waking a
/// dozen sleeping threads would take a good part of them (std's Barrier between
/// rounds was about 1 ms slower per run). Yielding after the spin keeps a thread
/// that lost its core from being starved by spinning siblings.
fn wait(count: &AtomicUsize, n: usize) {
    for spin in (0u32..).take_while(|_| count.load(Acquire) < n) {
        std::hint::spin_loop();
        (spin >= 1 << 14).then(thread::yield_now); // from the 2^14th spin on
    }
}

/// Builds the join table from regions.csv, then parses and aggregates every run,
/// and writes the output. One set of threads does all of it, this one included;
/// the thread that writes the output exits the process.
fn run(data_dir: &str, out_dir: &str) -> ! {
    let lines = map(&format!("{data_dir}/regions.csv")).strip_prefix(b"region_id,country\n");
    let lines = lines.expect("regions.csv: the header is not region_id,country");
    let text = map(&format!("{data_dir}/records.csv"));
    // The header is the first line (spec 2.2: a BOM, the header, CRLF).
    let header_end = text.iter().position(|&b| b == b'\n');
    let body_start = 1 + header_end.expect("records.csv: no header line");
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    // regions.csv lists its ids in ascending order (spec 2.3), so the last line
    // has the largest. One entry more, for every id above it.
    let last_line = lines.trim_ascii_end().rsplit(|&b| b == b'\n').next();
    // number() skips non-digits, so the ",CCC" after the id adds nothing.
    let ids = number(last_line.unwrap_or_default()) as usize + 2;
    // The generator's pool has 100,000 names (and 4 more per block of edge cases).
    // A partition's table may fill to 5/8: room for its share (at least one name,
    // so at least 16 slots), plus 8 standard deviations, a power of two. More
    // names than that is an error.
    let parts = threads * PARTITIONS_PER_THREAD;
    // 100,000 pool names, plus room for the edge-case blocks' names.
    let names = (101_000 / parts).max(1);
    let slots = ((names + 8 * names.isqrt()) * 8 / 5).next_power_of_two();
    // Pairs per partition in one run (records are about 60 bytes), and 2 standard
    // deviations more. A buffer that overflows grows, and is dropped once emptied.
    // (Room for nearly every run, 3.4 deviations: 0.3 MB more.)
    let runs = threads * RUNS_PER_THREAD;
    let mean = text.len() / (runs * 60 * parts);
    let capacity = mean + 2 * mean.isqrt() + 4;
    // The next of `n` items from a shared counter, or None once all are claimed.
    let claim = |next: &AtomicUsize, n: usize| Some(next.fetch_add(1, Relaxed)).filter(|&k| k < n);
    // The join table, dense by region id: group + 1, or 0 for ids it does not
    // contain. Zeroed by calloc, so the pages fault in on the threads that fill them.
    // SAFETY: all zero bits are a valid AtomicU8 (0).
    let group_of: Box<[AtomicU8]> = unsafe { Box::new_zeroed_slice(ids).assume_init() };
    // By country code index (26^3 of them): its group + 1, or 0 while not seen;
    // and the country codes in group order (see `fill`).
    let (codes, countries): (Vec<AtomicU8>, _) = (many(26 * 26 * 26), Mutex::new(Vec::new()));
    // regions.csv chunks claimed and entered, runs claimed, threads done filing pairs.
    let [next_chunk, chunks_done, next_run, flushed]: [AtomicUsize; 4] = Default::default();
    // Each thread's tables (its partitions, w * 4 .. w * 4 + 4), and its inbox:
    // buffers of pairs filed under them that they have not taken yet, 4 at a time
    // (one per partition, in order). Buffers are handed over, not copied (copying
    // them: +1.2 ms). Emptied buffers go to one pool that every filer draws from
    // (with a pool per owner, buffers sat idle in one while a filer made new ones
    // for another: 0.3 MB more, up to 2 MB when a thread fell behind).
    let tables: Vec<Mutex<Vec<Table>>> = many(threads);
    let inboxes: Vec<Mutex<Vec<Vec<u64>>>> = many(threads);
    let empties = Mutex::new(Vec::new());
    // Per block of runs, how many are parsed.
    let parsed: Vec<AtomicUsize> = many(runs / BLOCK);
    // The threads' totals, summed as each one finishes (kept one per thread, with
    // the Vec's regrowth: 0.2 MB more), and how many have.
    let finished = Mutex::new((0, [EMPTY; GROUPS]));
    let worker = &|w: usize| {
        let mut totals = [EMPTY; GROUPS];
        // This thread's tables and pair buffers: made here, so they are in its cache.
        // Room for a row of WORDS words per name the table can hold (5/8 of its slots).
        let table = || (vec![0; slots], Vec::with_capacity(slots / 8 * 5 * WORDS));
        *tables[w].lock().unwrap() = (0..PARTITIONS_PER_THREAD).map(|_| table()).collect();
        let mut pairs: Vec<_> = (0..parts).map(|_| Vec::with_capacity(capacity)).collect();
        // Thread t's tables take the pairs in its inbox, and give the buffers back
        // empty.
        let (mut taken, mut spare) = (Vec::new(), Vec::new());
        let mut take = |mine: &mut Vec<Table>, t: usize, totals: &mut _| {
            std::mem::swap(&mut taken, &mut inboxes[t].lock().unwrap());
            for (k, buffer) in taken.iter_mut().enumerate() {
                // SAFETY: every table is made by `table()` (its slots a power of two,
                // at least 16) and changed by nothing but `add`.
                unsafe { add(&mut mine[k % PARTITIONS_PER_THREAD], buffer, totals) };
                buffer.clear();
            }
            // A buffer that grew (the empty name's partition overflows) is dropped,
            // so that the shared pool's buffers stay small.
            taken.retain(|buffer| buffer.capacity() == capacity);
            empties.lock().unwrap().append(&mut taken);
        };
        // regions.csv first, in REGION_CHUNKS equal parts. Every LF in it is followed
        // by a digit or the end, so `run_start` cuts it into whole lines.
        while let Some(k) = claim(&next_chunk, REGION_CHUNKS) {
            let cut = |k| run_start(lines, 0, k, REGION_CHUNKS);
            fill(&lines[cut(k)..cut(k + 1)], &group_of, &codes, &countries);
            chunks_done.fetch_add(1, Release);
        }
        wait(&chunks_done, REGION_CHUNKS);
        // Runs are claimed one at a time, so a thread slowed down parses fewer of
        // them, and no thread waits for another one until the end.
        while let Some(nth) = claim(&next_run, runs) {
            // Claims go round the file's `threads` stripes, so that runs claimed at
            // the same time lie a stripe apart and rarely share a page-table page
            // (whose lock their page faults and releases contended on: -0.5 ms).
            let run_index = nth % threads * RUNS_PER_THREAD + nth / threads;
            // Each thread finds its own cuts, so their pages fault in in parallel.
            let cut = |k| run_start(text, body_start, k, runs);
            let span = (cut(run_index), cut(run_index + 1));
            // SAFETY: group_of has ids >= 2 entries.
            unsafe { parse_run(text, span, &group_of, &mut totals, &mut pairs) };
            // Empty buffers for the next run's pairs, all at once.
            let mut pool = empties.lock().unwrap();
            let n = pool.len().saturating_sub(parts);
            spare.extend(pool.drain(n..));
            drop(pool);
            for (t, buffers) in pairs.chunks_mut(PARTITIONS_PER_THREAD).enumerate() {
                let mut inbox = inboxes[t].lock().unwrap();
                for buffer in buffers {
                    let fresh = spare.pop().unwrap_or_else(|| Vec::with_capacity(capacity));
                    inbox.push(std::mem::replace(buffer, fresh));
                }
                // A thread that falls behind (it lost its core) would let its inbox,
                // and the memory peak, grow: past a bound, the filer takes it over.
                // (Normally an inbox holds a few dozen buffers. A thread that has not
                // made its tables yet has none to take over.)
                let behind = inbox.len() > 16 * threads;
                drop(inbox);
                if behind
                    && let Ok(mut theirs) = tables[t].try_lock()
                    && !theirs.is_empty()
                {
                    take(&mut theirs, t, &mut totals);
                }
            }
            // The last of a block's runs to be parsed drops the block's pages. (A
            // call per run instead: +2 ms, from 8 times the TLB-shootdown rounds.)
            let block = run_index / BLOCK;
            if parsed[block].fetch_add(1, AcqRel) == BLOCK - 1 {
                // SAFETY: `text` is `map`'s: a private mapping that is never written.
                // (The index cannot panic: cuts are monotonic and at most text.len().)
                unsafe { drop_pages(&text[cut(block * BLOCK)..cut(block * BLOCK + BLOCK)]) };
            }
            // This thread's tables, unless another thread holds them.
            if let Ok(mut mine) = tables[w].try_lock() {
                take(&mut mine, w, &mut totals);
            }
        }
        // Once no thread files pairs any more, every thread's tables take their
        // inbox, this thread's first (its own inbox twice: the second take finds it
        // empty).
        flushed.fetch_add(1, Release);
        wait(&flushed, threads);
        for t in std::iter::once(w).chain(0..threads) {
            take(&mut tables[t].lock().unwrap(), t, &mut totals);
        }
        // The last thread to finish writes the output and exits, without waiting
        // for the others to exit and without dropping anything.
        let (done, sums) = &mut *finished.lock().unwrap();
        merge(sums, &totals);
        *done += 1;
        if *done == threads {
            write_output(out_dir, sums, &countries.lock().unwrap());
            std::process::exit(0);
        }
    };
    thread::scope(|s| {
        // The handles are not needed: the scope joins the threads, and the last
        // worker exits before that.
        (1..threads).for_each(|w| drop(s.spawn(move || worker(w))));
        worker(0);
    });
    unreachable!("the last worker exits the process")
}

/// Enters the `region_id,CCC` LF lines of regions.csv in `lines` (spec 2.3) into
/// the join table `group_of`: each region id gets its country's group + 1. A
/// country code seen for the first time gets the next group, by `codes` (indexed
/// by the code, 0 while not seen) and `countries` (the codes in group order, held
/// while a code gets the next number).
fn fill(lines: &[u8], group_of: &[AtomicU8], codes: &[AtomicU8], countries: &Mutex<Vec<Vec<u8>>>) {
    let mut at = 0;
    while at < lines.len() {
        let (slot, comma) = (&group_of[digits(lines, &mut at) as usize], at);
        let [a, b, c] = [1, 2, 3].map(|i| (lines[comma + i] - b'A') as usize);
        let group = &codes[a * 676 + b * 26 + c];
        // Checked again under the lock: two threads can meet a new code at once.
        if group.load(Relaxed) == 0 {
            let mut countries = countries.lock().unwrap();
            if group.load(Relaxed) == 0 {
                countries.push(lines[comma + 1..comma + 4].to_vec());
                // Stored values 1..=240 are groups 0..239 (see WORDS); 0, an id
                // not listed, wraps to group 255, _UNMATCHED.
                assert!(countries.len() <= 240, "regions.csv: over 240 countries");
                group.store(countries.len() as u8, Relaxed);
            }
        }
        slot.store(group.load(Relaxed), Relaxed);
        at = comma + 5; // past ",CCC" and the LF
    }
}

/// Adds a thread's totals to the sums: every column adds up, except the min and
/// the max.
fn merge(sums: &mut [Totals; GROUPS], totals: &[Totals; GROUPS]) {
    for (sum, t) in sums.iter_mut().zip(totals) {
        for k in [COUNT, ID_SUM, AMOUNT_SUM, NAMES_DISTINCT, BYTES] {
            sum[k] += t[k];
        }
        sum[AMOUNT_MIN] = sum[AMOUNT_MIN].min(t[AMOUNT_MIN]);
        sum[AMOUNT_MAX] = sum[AMOUNT_MAX].max(t[AMOUNT_MAX]);
    }
}

/// Writes the line of each group that has records (spec sections 3 and 4).
fn write_output(dir: &str, totals: &[Totals; GROUPS], countries: &[Vec<u8>]) {
    // Keys sorted bytewise: the country codes, then `_UNMATCHED` ("_" > "Z").
    let mut keys: Vec<_> = countries.iter().cloned().zip(0..).collect();
    keys.push((b"_UNMATCHED".to_vec(), GROUPS - 1));
    keys.sort();
    // One String written into, not a String per line and per amount (0.2 ms on the
    // server, after the parse, when no other thread has work left: now 0.1).
    // Writing into a String cannot fail, so the `write!` results are unwrapped.
    let mut out = String::new();
    for (key, group) in keys.into_iter().filter(|&(_, g)| totals[g][COUNT] > 0) {
        let [count, ids, sum, min, max, names, bytes] = totals[group];
        write!(out, "{} {count} {ids}", String::from_utf8_lossy(&key)).unwrap();
        // Money: cents with a sign and two decimals ("-0.05", "0.00").
        for cents in [sum, min, max] {
            let (sign, abs) = (if cents < 0 { "-" } else { "" }, cents.unsigned_abs());
            write!(out, " {sign}{}.{:02}", abs / 100, abs % 100).unwrap();
        }
        // Name and note bytes are the two halves of `bytes`.
        writeln!(out, " {names} {} {}", bytes >> 32, bytes & 0xFFFF_FFFF).unwrap();
    }
    let path = format!("{dir}/aggregates.txt");
    std::fs::write(&path, out).unwrap_or_else(|e| panic!("write {path}: {e}"));
}

/// Marks each (name hash, group) pair, and counts in `totals` the pairs seen for
/// the first time. Out of line, like `parse_run`, so that edits to the worker
/// around it cannot change how its loop is compiled.
///
/// # Safety
/// `table` must have been made empty with a power-of-two number of key slots, at
/// least 8 (one bucket; `table()` makes at least 16), and changed since by nothing
/// but `add`: then every key that is not 0 holds the row + 1 of a row that `seen`
/// has.
#[inline(never)]
unsafe fn add(table: &mut Table, pairs: &[u64], totals: &mut [Totals; GROUPS]) {
    // Fields as locals: no store through them can change the others.
    let (keys, seen) = table;
    let (buckets, full) = (keys.len() / 8, keys.len() / 8 * 5 * WORDS);
    // A pair is the name's hash with the group in its low byte. Its bits, at 12
    // threads (4096 slots per table) and, in parentheses, on one thread (65,536):
    // - 0..8: the group;
    // - 8..17 (8..21): the bucket, 9 bits (13);
    // - 21..36: 15 check bits, stored in the name's row (word 3, bits 48..62);
    // - 24..56: the key, whose low 12 bits (16) hold the row + 1 instead, so that
    //   keys compare bits 36..56, 20 bits (40..56, 16 bits);
    // - the top bits picked the partition (`parse_run`): log2(48) = 5.6 bits
    //   (log2(4) = 2).
    // Two names are taken for one only if all of those agree: 9 + 15 + 20 + 5.6 =
    // 49.6 bits at 12 threads, 13 + 15 + 16 + 2 = 46 on one. With the generator's
    // 100,000 names, that happens with odds of about C(100000, 2) / 2^49.6 = 5.9e-6
    // per dataset at 12 threads, and C(100000, 2) / 2^46 = 7.1e-5 on one. (A name
    // that spilled into the next bucket, about 1% of them at 12 threads, is also
    // compared with that bucket's names, which raises the odds by under 2%.)
    let rows_mask = keys.len() as u32 - 1;
    for &pair in pairs {
        let group = (pair & 0xFF) as usize;
        let (key, check) = ((pair >> 24) as u32 & !rows_mask, pair >> 21 & 0x7FFF);
        // The top hash bits picked the partition; the bits above the group pick a
        // bucket of 8 slots, compared at once. Slots fill in order and are never
        // emptied, so the first slot that holds the name or is empty is the name's.
        let mut bucket = (pair >> 8) as usize & (buckets - 1);
        // SAFETY:
        // - buckets is a power of two (the caller's contract), so the mask keeps
        //   `bucket` below it: the 8 keys loaded at bucket * 8 are in `keys`, and a
        //   slot is one of them (`hits` has 8 bits);
        // - a key that is not 0 holds the row + 1 of a row whose WORDS words are in
        //   `seen`: a new key's row is pushed right after it, and the assert keeps
        //   every row + 1 at or below 5/8 of the slots, inside the bits of rows_mask;
        // - group / 64 < WORDS, since group < 256;
        // - AVX2 is enabled for this build (see the compile_error).
        // The loop ends: the assert keeps the table at most 5/8 full, so some bucket
        // has a free slot.
        unsafe {
            let wanted = _mm256_set1_epi32(key as i32);
            let mask = _mm256_set1_epi32(!rows_mask as i32);
            let row = 'found: loop {
                let v = _mm256_loadu_si256(keys.as_ptr().add(bucket * 8).cast());
                let free = _mm256_cmpeq_epi32(v, _mm256_setzero_si256());
                let hit = _mm256_cmpeq_epi32(_mm256_and_si256(v, mask), wanted);
                let hit = _mm256_or_si256(hit, free);
                // Slots whose hash bits match, or empty, in order: a match whose
                // check bits differ is another name.
                let mut hits = _mm256_movemask_ps(_mm256_castsi256_ps(hit));
                while hits != 0 {
                    let slot = keys.get_unchecked_mut(bucket * 8 + hits.trailing_zeros() as usize);
                    if *slot == 0 {
                        assert!(seen.len() < full, "records.csv: too many distinct names");
                        *slot = key | (seen.len() / WORDS + 1) as u32;
                        // A new row: WORDS (4) words, the check bits in word 3's
                        // bits 48..62.
                        seen.extend_from_slice(&[0, 0, 0, check << 48]);
                        break 'found seen.len() / WORDS - 1;
                    }
                    let row = (*slot & rows_mask) as usize - 1;
                    // Word 3's bits 48..62, without bit 63 (`_UNMATCHED`).
                    if seen.get_unchecked(row * WORDS + 3) << 1 >> 49 == check {
                        break 'found row;
                    }
                    hits &= hits - 1;
                }
                bucket = (bucket + 1) & (buckets - 1);
            };
            let word = seen.get_unchecked_mut(row * WORDS + group / 64);
            let bit = 1 << (group % 64);
            totals[group][NAMES_DISTINCT] += (*word & bit == 0) as i64;
            *word |= bit;
        }
    }
}

/// Where run `k` of the `parts` runs of `text[from..]` starts (`parts`: the end):
/// at `from`, or at a guessed record start (see the top of the file), the first
/// one at or after the nominal cut. Monotonic in k: a search that passes the next
/// nominal point finds the same cut.
fn run_start(text: &[u8], from: usize, k: usize, parts: usize) -> usize {
    let nominal = from + (text.len() - from) * k / parts;
    let ok = |&c: &_| c == from || text[c - 1] == b'\n' && matches!(text[c], b'0'..=b'9' | 0xEF);
    (nominal..text.len()).find(ok).unwrap_or(text.len())
}

/// Parses the records in `text[start..end]` into `totals`, and files each
/// record's (name hash, group) pair under its partition. The last record must end
/// at `end`. (It takes group_of as a plain slice, not a struct that also holds a
/// Mutex: through such a struct LLVM could not keep the slice's address in a
/// register across the stores into totals, and every record reloaded it.) Out of
/// line: inlined into the worker, its registers were allocated with the worker's,
/// and it ran 2-4% slower.
///
/// The run is parsed as two halves at once: each pass of the loop parses one
/// record from each half and then counts both, so their two chains of dependent
/// work, above all their two random region-table loads, are in the core at the
/// same time (as 1BRC solutions run several cursors per thread). Server, 12
/// threads: -0.5 ms; single-threaded the region-load stall went from about 35% of
/// this function's cycles to 13%. (Three cursors: no faster, more instructions;
/// cursors in arrays: +3 ms, the arrays stay in memory.) The second half starts at
/// a cut found like the runs' cuts, so the first half must end exactly there.
/// Every total, the min and max, and the set of pairs do not depend on the order in
/// which records are counted.
///
/// # Safety
/// `group_of` must not be empty: a region id is clamped to its last entry, which is
/// then read without a bounds check.
#[inline(never)]
unsafe fn parse_run(
    text: &[u8],
    (start, end): (usize, usize),
    group_of: &[AtomicU8],
    totals: &mut [Totals; GROUPS],
    pairs: &mut [Vec<u64>],
) {
    let fast_end = end.min(text.len().saturating_sub(FAST_MARGIN));
    let record = |i: usize| {
        // A non-temporal prefetch ~16 records ahead: the stream then takes less of
        // L2, so more of the 1 MB region table stays there.
        // SAFETY: a prefetch never faults, and wrapping_add keeps the address defined.
        unsafe { _mm_prefetch::<_MM_HINT_NTA>(text.as_ptr().wrapping_add(i + 1024).cast()) };
        // SAFETY: fast_end is at most text.len() - FAST_MARGIN, or 0 (above).
        unsafe { fast_record(text, i, fast_end) }.unwrap_or_else(|| walk_record(text, i))
    };
    let mut count = |r: &Record| {
        // Copied out first: read through `r` after the stores into `t` below, the
        // hash was loaded again from memory.
        let (id, region_id, cents, bytes, hash) = (r.id, r.region_id, r.cents, r.bytes, r.hash);
        // Ids above the table read its last entry, which is 0 (one entry more than
        // the largest listed id).
        let k = (region_id as usize).min(group_of.len() - 1);
        // SAFETY: k <= group_of.len() - 1, and group_of is not empty (the caller's
        // contract).
        let stored = unsafe { group_of.get_unchecked(k) }.load(Relaxed);
        // The stored group + 1, so 0 (an id not listed) wraps to group 255, _UNMATCHED.
        let group = stored.wrapping_sub(1);
        let t = &mut totals[group as usize];
        (t[COUNT], t[ID_SUM], t[AMOUNT_SUM]) =
            (t[COUNT] + 1, t[ID_SUM] + id as i64, t[AMOUNT_SUM] + cents);
        t[BYTES] += bytes as i64;
        // Branches, not cmov and a store: a new min or max is rare.
        if cents < t[AMOUNT_MIN] {
            t[AMOUNT_MIN] = cents;
        }
        if cents > t[AMOUNT_MAX] {
            t[AMOUNT_MAX] = cents;
        }
        // The partition from the top hash bits, by multiply-shift: any count.
        let p = (((hash >> 32) * pairs.len() as u64) >> 32) as usize;
        pairs[p].push(hash & !0xFF | group as u64);
    };
    let mid = run_start(&text[..end], start, 1, 2);
    let (mut i, mut j) = (start, mid);
    while i < mid && j < end {
        let (a, b) = (record(i), record(j));
        count(&a);
        count(&b);
        (i, j) = (a.next, b.next);
    }
    // The rest of the longer half, one record at a time; each half must end exactly
    // at its end (for the first, the cut in the middle).
    for (mut i, end) in [(i, mid), (j, end)] {
        while i < end {
            let r = record(i);
            count(&r);
            i = r.next;
        }
        assert_eq!(i, end, "records.csv is outside spec 2.2");
    }
}

/// A record, `id,name,note,region_id,amount` and a terminator, whose name and note
/// are empty or quoted (spec 2.2), from the bitmaps of the 64 bytes at `i` (rarely
/// 128) and of 32 bytes after the note. None if the name or the note runs past
/// them, or if `i` is not in `8..fast_end`.
///
/// # Safety
/// `fast_end` must be at most `text.len() - FAST_MARGIN`, or 0. Every read is in
/// `i - 8..i + 196` (see below), so it is then inside the text.
#[inline(always)]
unsafe fn fast_record(text: &[u8], i: usize, fast_end: usize) -> Option<Record> {
    // i >= 8: load() reads the 8 bytes before an offset; `?` gives None if not.
    (i >= 8 && i < fast_end).then_some(())?;
    // From here on 8 <= i and i + FAST_MARGIN <= text.len(). The reads, as offsets
    // from i, end by 196:
    // - the windows, 0..64, and 64..128 for a long note;
    // - the hash's, from the name in the first window to 32 bytes past it: by 96;
    // - the tail's 32 bytes, from note_end + 1 <= 128: by 160;
    // - load(k), the 8 bytes before k, and at(k), the byte at k, for k = 3, id_end
    //   (< 64), region_end and amount_end. region_len and dot are below 32, or 64
    //   if their byte is missing, so region_end = note_end + 1 + region_len <= 192
    //   and amount_end = note_end + 4 + dot <= 195.
    // SAFETY: i < text.len() (above).
    let p = unsafe { text.as_ptr().add(i) };
    // SAFETY: k <= 195 at every call (above).
    let at = |k: usize| unsafe { *p.add(k) };
    // SAFETY: k <= 195 at every call, and i >= 8 (above).
    let load = |k: usize| unsafe { p.add(k).sub(8).cast::<u64>().read_unaligned() }; // ends at k
    // SAFETY: the 64 bytes at i are inside the text (above).
    let (quotes, commas, inside) = masks(unsafe { &*p.cast() });
    let mut out = commas & !inside;
    let id_end = out.trailing_zeros() as usize;
    out &= out.wrapping_sub(1);
    let name_end = out.trailing_zeros() as usize; // 64 if none
    out &= out.wrapping_sub(1);
    (name_end < 64).then_some(())?;
    // Empty, or quoted with every inner quote half of a `""` pair (spec 2.2). (A
    // one-byte field counts as unquoted, so that the hash's range is never negative.)
    let name_quoted = (name_end > id_end + 2) as usize;
    let name_quotes = (quotes & !(u64::MAX << name_end)).count_ones() as usize;
    let name_bytes = name_end - id_end - 1 - name_quoted - name_quotes / 2;
    let (s, e) = (i + id_end + 1 + name_quoted, i + name_end - name_quoted);
    // SAFETY: s <= e < i + 64, and the text goes on to i + 256 at least (above).
    let hash = unsafe { hash_words(text, s, e) };
    let (note_end, quotes_to_note) = if out != 0 {
        let e = out.trailing_zeros() as usize;
        (e, (quotes & !(u64::MAX << e)).count_ones())
    } else {
        // The note runs past the first 64 bytes: the next 64, carrying the quote state.
        // SAFETY: the 64 bytes at i + 64 are inside the text (above).
        let (quotes2, commas2, inside2) = masks(unsafe { &*p.add(64).cast() });
        let out2 = commas2 & !(inside2 ^ ((inside as i64) >> 63) as u64);
        let e = out2.trailing_zeros() as usize; // 64 if none
        let before = quotes.count_ones() + (quotes2 & !(u64::MAX << (e & 63))).count_ones();
        (64 + e, before)
    };
    (note_end < 128).then_some(())?;
    let note_quoted = (note_end > name_end + 1) as usize;
    let note_quotes = quotes_to_note as usize - name_quotes;
    let note_bytes = note_end - name_end - 1 - note_quoted - note_quotes / 2;
    // The tail, `region_id,amount`: its first comma and dot, from its 32 bytes.
    // SAFETY: note_end < 128, so the 32 bytes end by i + 160 (above).
    let tail = unsafe { _mm256_loadu_si256(p.add(note_end + 1).cast()) };
    let region_len = bytes_eq(tail, b',').trailing_zeros() as usize;
    let dot = bytes_eq(tail, b'.').trailing_zeros() as usize;
    // load(3) >> 40 is the record's first 3 bytes: a BOM there is not part of the id.
    let id_len = id_end - if load(3) >> 40 == 0xBF_BBEF { 3 } else { 0 };
    let negative = bytes_eq(tail, b'-') >> (region_len + 1) & 1 != 0; // a mask bit: no byte load
    // The amount ends two digits after its dot (spec 2.2).
    let (region_end, amount_end) = (note_end + 1 + region_len, note_end + 4 + dot);
    let whole = dot - region_len - 1 - negative as usize;
    let words = [load(id_end), load(region_end), load(amount_end)];
    let [id, region_id, c] = numbers(words, [id_len, region_len, whole + 2]);
    Some(Record {
        id,
        region_id,
        cents: if negative { -(c as i64) } else { c as i64 },
        bytes: (name_bytes << 32 | note_bytes) as u64,
        hash,
        next: i + amount_end + 1 + (at(amount_end) == b'\r') as usize, // CRLF or LF
    })
}

/// Any record of spec 2.2 starting at `i`, field by field: the ones whose name or
/// note is too long for `fast_record`'s windows, and the last few of the file. Out
/// of line, so that it does not crowd the registers of the fast path's loop.
#[cold]
#[inline(never)]
fn walk_record(text: &[u8], i: usize) -> Record {
    let mut at = i + (text[i] == 0xEF) as usize * 3; // a BOM is not data
    let [id, name, note, region, amount] = [(); 5].map(|_| field(text, &mut at));
    let value = |f: &Field| number(&text[f.start..f.end]);
    let cents = value(&amount) as i64 * if text[amount.start] == b'-' { -1 } else { 1 };
    let next = at + (text[at - 1] == b'\r') as usize; // CRLF or LF
    // The name is hashed from a padded copy, so that the hash's loads stay in bounds
    // whatever the input. (This path is rare.)
    let copy = [&text[name.start..name.end], &[0; 32]].concat();
    // The values in the order they are computed (cents and next above), not the struct's.
    Record {
        // SAFETY: the copy has 32 bytes after the name.
        hash: unsafe { hash_words(&copy, 0, copy.len() - 32) },
        bytes: name.len << 32 | note.len,
        id: value(&id),
        region_id: value(&region),
        cents,
        next,
    }
}

/// The field at `*at`, quoted or not, and the byte after it (a comma or the
/// terminator's first byte), which it moves past.
fn field(text: &[u8], at: &mut usize) -> Field {
    let quoted = text[*at] == b'"';
    let (start, mut escapes) = (*at + quoted as usize, 0);
    *at = start;
    loop {
        // A quoted value ends at the next `"` that is not one of a `""` pair.
        match (quoted, &text[*at..]) {
            (true, [b'"', b'"', ..]) => (*at, escapes) = (*at + 2, escapes + 1),
            (true, [b'"', ..]) | (false, [b',' | b'\r' | b'\n', ..]) => break,
            _ => *at += 1,
        }
    }
    let (end, len) = (*at, (*at - start - escapes) as u64);
    *at += 1 + quoted as usize;
    Field { start, end, len }
}

/// The value of the decimal digits in `bytes`, skipping any other byte: an amount's
/// digits, without its sign and dot, are its cents.
fn number(bytes: &[u8]) -> u64 {
    let digits = bytes.iter().filter(|b| b.is_ascii_digit());
    digits.fold(0, |value, d| value * 10 + (d - b'0') as u64)
}

/// Bitmaps of a 64-byte block: bit k is set if byte k is `"`, if it is `,`, and if
/// it is inside quotes, counting from the block's start (an opening quote counts as
/// inside, a closing one as outside, and a `""` pair leaves the state unchanged).
#[inline(always)]
fn masks(block: &[u8; 64]) -> (u64, u64, u64) {
    // SAFETY: AVX2 is enabled for this build (see the compile_error), and the two
    // unaligned 32-byte loads are inside the 64-byte array.
    let [lo, hi] = [0, 32].map(|k| unsafe { _mm256_loadu_si256(block.as_ptr().add(k).cast()) });
    let bits = |byte| bytes_eq(lo, byte) | bytes_eq(hi, byte) << 32;
    // A carry-less multiply by all ones XORs every lower bit into each bit.
    // SAFETY: SSE2 is part of x86-64, and the call touches no memory.
    let (quotes, ones) = (bits(b'"'), unsafe { _mm_set1_epi8(-1) });
    // SAFETY: PCLMULQDQ is enabled for this build (see the compile_error); no memory.
    let product = unsafe { _mm_clmulepi64_si128(_mm_set_epi64x(0, quotes as i64), ones, 0) };
    // SAFETY: SSE2, as above; no memory.
    let inside = unsafe { _mm_cvtsi128_si64(product) } as u64;
    (quotes, bits(b','), inside)
}

/// Bit k is set if byte k of `v` is `byte`: `vpcmpeqb`, then `vpmovmskb` kept
/// opaque to the optimizer. LLVM rewrites the intrinsic as a vector of booleans,
/// and on CPUs without AVX-512 (the scoring box) it then assembled our 64-bit
/// masks one bit at a time: a quarter of the run time.
#[inline(always)]
fn bytes_eq(v: __m256i, byte: u8) -> u64 {
    // A u64: writing the 32-bit register clears the top half, so no zero-extension.
    let mask: u64;
    // SAFETY: AVX2 is enabled for this build; the instruction reads one register.
    unsafe {
        let eq = _mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8));
        std::arch::asm!("vpmovmskb {0:e}, {1}", out(reg) mask, in(ymm_reg) eq, options(pure, nomem, nostack))
    };
    mask
}

/// Three numbers at once, from the 8 bytes ending at each (the id, the region id,
/// and the amount with its dot, which is squeezed out), and how many digits each
/// has: 1 to 8, since ids are below 4 x 10^7 (spec section 3, "Magnitudes"), region
/// ids at most 9,999,999 and amounts at most 99,999.99 (spec 2.2).
#[inline(always)]
fn numbers(words: [u64; 3], lens: [usize; 3]) -> [u64; 3] {
    // SAFETY: AVX2 is enabled for this build (see the compile_error), and the
    // transmute turns 32 bytes into 32 bytes, all of whose bit patterns are valid.
    unsafe {
        let [a, b, c] = words.map(|w| w as i64);
        let v = _mm256_xor_si256(_mm256_set_epi64x(0, c, b, a), _mm256_set1_epi8(b'0' as i8));
        // Within each 128-bit half: the first two words as they are; the amount's
        // bytes 0..5 one up and its dot (byte 5) dropped (0x80 makes a zero byte).
        let same = 0x0F0E_0D0C_0B0A_0908;
        let squeeze = _mm256_set_epi64x(same, 0x0706_0403_0201_0080, same, 0x0706_0504_0302_0100);
        let v = _mm256_shuffle_epi8(v, squeeze);
        // Each word's last n bytes are its digits: keep those (n is 1 to 8).
        let [x, y, z] = lens.map(|n| (u64::MAX << (64 - 8 * n)) as i64);
        let v = _mm256_and_si256(v, _mm256_set_epi64x(0, z, y, x));
        // Pairs of digits (x10 + 1), then pairs of those (x100 + 1), packed to
        // 16 bits, then pairs again (x10000 + 1): one number per 64-bit lane.
        let v = _mm256_maddubs_epi16(v, _mm256_set1_epi16(0x010A));
        let v = _mm256_madd_epi16(v, _mm256_set1_epi32(0x0001_0064));
        let v = _mm256_packus_epi32(v, v);
        let v = _mm256_madd_epi16(v, _mm256_set1_epi32(0x0001_2710));
        let v: [u32; 8] = std::mem::transmute(v); // lanes 0, 1 and 4 hold the numbers
        [v[0], v[1], v[4]].map(u64::from)
    }
}

/// A 64-bit hash of `text[s..e]`, 32 bytes at a time: each block masked to the
/// field (`KEEP`), without a branch on the length, goes through three AES rounds
/// (two for its bytes 16..32, enough for every output byte to depend on every input
/// byte), the length in the first. Names are nearly always one block. (The earlier
/// 8-byte word loop's branch mispredicted on the 11% of names of 17-24 bytes.)
///
/// # Safety
/// `s <= e`, and `s..e + 32` must be inside the text (`s..s + 32` if the field is
/// empty).
#[inline(always)]
unsafe fn hash_words(text: &[u8], s: usize, e: usize) -> u64 {
    // SAFETY: each block's 32 bytes start before `e` (or at `s`), and each mask is
    // inside the 64 bytes of `KEEP`. AES and AVX2 are enabled (see the compile_error).
    unsafe {
        // A fixed round key: any constant works, AES only needs it to be the same
        // for every name.
        let key = _mm_set_epi64x(0x2076_1d64_78bd_642f, 0x6703_7ed1_a0b4_28db);
        let (mut x, mut at) = (_mm_cvtsi64_si128((e - s) as i64), s);
        // A loop with its test at the end (one block for an empty field too).
        while {
            let keep = KEEP.as_ptr().cast::<u8>().add(32 - (e - at).min(32));
            let v = _mm256_loadu_si256(text.as_ptr().add(at).cast());
            let v = _mm256_and_si256(v, _mm256_loadu_si256(keep.cast()));
            let (lo, hi) = (_mm256_castsi256_si128(v), _mm256_extracti128_si256::<1>(v));
            x = _mm_aesenc_si128(_mm_xor_si128(x, lo), hi);
            x = _mm_aesenc_si128(_mm_aesenc_si128(x, key), key);
            at += 32;
            at < e
        } {}
        _mm_cvtsi128_si64(x) as u64
    }
}

/// The 32 bytes at `32 - n` keep the first n bytes of 32 (little-endian words).
static KEEP: [u64; 8] = [u64::MAX, u64::MAX, u64::MAX, u64::MAX, 0, 0, 0, 0];

/// The digits at `*at`, which it moves past: their value.
fn digits(text: &[u8], at: &mut usize) -> u64 {
    let mut value = 0;
    while let Some(&digit @ b'0'..=b'9') = text.get(*at) {
        value = value * 10 + (digit - b'0') as u64;
        *at += 1;
    }
    value
}
