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
//! 4. Distinct names per country: a name's identity is its raw inner bytes (inside
//!    quotes, `""` maps one to one to the unescaped value; an empty name is empty
//!    whether quoted or not), reduced to a 56-bit hash; with the spec's 100,000
//!    names the chance that two share one is about 7e-8. Parsing only files a (hash,
//!    group) pair under one of 4 partitions per thread picked by the hash. After
//!    each run it parses, a thread hands its buffers of pairs to their owners'
//!    inboxes, and its own tables take what waits in its inbox: small tables of
//!    names with a bit per group, which stay in its cache.
//! 5. No thread waits for a given other one until the end: runs are claimed one at
//!    a time, so a thread slowed down (another process sharing its core) parses
//!    fewer of them, and its partitions' pairs wait in their inboxes. (With a
//!    barrier after each round, one core shared with a busy loop made the run
//!    take twice as long.)

#[cfg(not(all(target_feature = "avx2", target_feature = "pclmulqdq")))]
compile_error!("track-c needs AVX2 and PCLMULQDQ: build with RUSTFLAGS=\"-C target-cpu=native\"");

use std::arch::x86_64::*;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::*};
use std::{sync::Mutex, thread};

/// Each thread parses this many runs, on average: they are claimed as threads go,
/// so a thread slowed down (another process on its core) parses fewer of them.
const RUNS_PER_THREAD: usize = 32;
/// Runs are dropped from the mapping in blocks of this many.
const BLOCK: usize = 4;
/// Names are split by their top hash bits into this many partitions per thread;
/// a thread's own partitions take their pairs, so their tables stay in its cache.
const PARTITIONS_PER_THREAD: usize = 4;
/// Group indices are bytes, so that tables indexed by a group need no bounds
/// checks: countries in the order they are first seen, `_UNMATCHED` the last one.
const GROUPS: usize = 256;
/// Words of group bits per name.
const WORDS: usize = GROUPS / 64;
/// regions.csv is entered in this many parts, which threads claim as they start,
/// so the ones that start first do more of it.
const REGION_CHUNKS: usize = 64;

/// A group's columns: count, id_sum, amount_sum, amount_min, amount_max,
/// names_distinct, and name_bytes << 32 | note_bytes (amounts in cents; every
/// byte column is below 2^31, spec section 3).
type Totals = [i64; 7];
const EMPTY: Totals = [0, 0, 0, i64::MAX, i64::MIN, 0, 0];

/// One partition's names: an open-addressing table of name hashes (0 = empty,
/// else the hash with its low byte set to 0xFF) in buckets of 4 slots, at most 5/8
/// full so that a lookup is nearly always one bucket; a dense id per slot; and a
/// row of WORDS words of group bits per id, added per new name. (Rows by slot
/// instead, in a 7/8 full table: 23% less memory, but +0.8M branch misses.)
type Table = (Vec<u64>, Vec<u16>, Vec<u64>);

/// One parsed record: id, region_id, amount in cents, name_bytes << 32 |
/// note_bytes, the name's hash, and where the next record starts.
type Record = (u64, u64, i64, u64, u64, usize);

// libc, declared by hand so that the crate has no dependencies.
unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, at: i64) -> *mut u8;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    fn mallopt(param: i32, value: i32) -> i32;
    fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
}

/// Errors (a missing file, input outside spec 2.2) are panics: the build aborts on
/// panic, so the message is printed and the exit status is nonzero.
fn main() {
    // SAFETY: plain libc calls; failure only keeps the defaults.
    // - M_TOP_PAD (-2), 32 MB: a worker thread's first allocations get their own
    //   heap, which otherwise grows a few pages per mprotect, and each mprotect
    //   stalls every other thread's page faults (800 of them per run: -2.8 ms).
    //   Reserved but untouched pages cost no memory.
    // - PR_SET_THP_DISABLE (41): with transparent huge pages, touching one byte of
    //   a sparse table can fault in a whole 2 MB page, so the memory peak would
    //   depend on how fragmented the host is (13 to 24 MB between runs).
    unsafe { (mallopt(-2, 32 << 20), prctl(41, 1, 0, 0, 0)) };
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    run(&data_dir, &out_dir)
}

/// Maps a whole file read-only for the rest of the process. It is never unmapped:
/// the process exits right after using it, and exit unmaps everything.
fn map(path: &str) -> &'static [u8] {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let len = file.metadata().unwrap().len() as usize;
    // SAFETY: a fresh mapping of `len` bytes, PROT_READ (1) and MAP_PRIVATE (2); the
    // dataset is mounted read-only, so the bytes cannot change under us.
    let ptr = unsafe { mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0) };
    assert_ne!(ptr as isize, -1, "mmap {path}");
    // SAFETY: the mapping is valid for `len` bytes and lives until exit.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// Drops this process's page-table entries for the whole pages inside `slice`, a
/// part of the input that is done with. At exit the kernel would otherwise unmap
/// the 50,000 pages of the input one by one, on one core (6 ms on the box); this
/// does it in each parsing thread, in parallel. Reading the bytes again would
/// still be correct: the pages fault back in from the page cache.
fn release(slice: &[u8]) {
    let start = (slice.as_ptr() as usize).next_multiple_of(4096);
    let end = (slice.as_ptr() as usize + slice.len()) / 4096 * 4096;
    // SAFETY: MADV_DONTNEED (4) on whole pages inside a private read-only file
    // mapping that is never written: dropping them loses nothing.
    unsafe { madvise(start as *mut u8, end.saturating_sub(start), 4) };
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
        if spin >= 1 << 14 {
            thread::yield_now();
        }
    }
}

/// Builds the join table from regions.csv, then parses and aggregates every run,
/// and writes the output. One set of threads does all of it, this one included;
/// the thread that writes the output exits the process.
fn run(data_dir: &str, out_dir: &str) -> ! {
    let lines = map(&format!("{data_dir}/regions.csv"));
    let lines = lines.strip_prefix(b"region_id,country\n").unwrap();
    let text = map(&format!("{data_dir}/records.csv"));
    // The header is the first line (spec 2.2: a BOM, the header, CRLF).
    let body_start = 1 + text.iter().position(|&b| b == b'\n').unwrap();
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    // regions.csv lists its ids in ascending order (spec 2.3), so the last line
    // has the largest. One entry more, for every id above it.
    let last_line = lines.trim_ascii_end().rsplit(|&b| b == b'\n').next();
    let ids = digits(last_line.unwrap_or_default(), &mut 0) as usize + 2;
    // The generator's pool has 100,000 names (and 4 more per block of edge cases).
    // A partition's table may fill to 5/8: room for its share, plus 8 standard
    // deviations, a power of two. More names than that is an error.
    let parts = threads * PARTITIONS_PER_THREAD;
    let names = 101_000 / parts;
    let slots = ((names + 8 * names.isqrt()) * 8 / 5).next_power_of_two();
    let rows = slots / 8 * 5 * WORDS;
    // Pairs per partition in one run, with room to spare (records are about 60
    // bytes), so that a buffer never grows.
    let runs = threads * RUNS_PER_THREAD;
    let capacity = text.len() / (runs * 60 * parts) * 5 / 4 + 16;
    let claim = |next: &AtomicUsize, n: usize| Some(next.fetch_add(1, Relaxed)).filter(|&k| k < n);
    // The join table, dense by region id: group + 1, or 0 for ids it does not
    // contain. SAFETY: zero is a valid AtomicU8. Zeroed by calloc, so the pages
    // fault in on the threads that fill them.
    let group_of: Box<[AtomicU8]> = unsafe { Box::new_zeroed_slice(ids).assume_init() };
    // By country code index (26^3 of them): its group + 1, or 0 while not seen;
    // and the codes in group order, held while a code gets the next number.
    let (codes, keys): (Vec<AtomicU8>, _) = (many(26 * 26 * 26), Mutex::new(Vec::new()));
    // Enters the `region_id,CCC` LF lines (spec 2.3) that start in the `k`th of
    // `REGION_CHUNKS` equal parts of `lines`. Every LF in regions.csv is followed
    // by a digit or the end, so `run_start` cuts it into whole lines.
    let fill = |k: usize| {
        let mut at = run_start(lines, 0, k, REGION_CHUNKS);
        let end = run_start(lines, 0, k + 1, REGION_CHUNKS);
        while at < end {
            let slot = &group_of[digits(lines, &mut at) as usize];
            let [a, b, c] = [1, 2, 3].map(|k| (lines[at + k] - b'A') as usize);
            let group = &codes[a * 676 + b * 26 + c];
            if group.load(Relaxed) == 0 {
                let mut keys = keys.lock().unwrap();
                if group.load(Relaxed) == 0 {
                    keys.push(lines[at + 1..at + 4].to_vec());
                    // Group numbers 1..=255; the last group, 256, is `_UNMATCHED`.
                    let number = u8::try_from(keys.len()).expect("at most 255 countries");
                    group.store(number, Relaxed);
                }
            }
            slot.store(group.load(Relaxed), Relaxed);
            at += 5;
        }
    };
    // Each thread's tables (its partitions, w * 4 .. w * 4 + 4), and its inbox:
    // buffers of pairs filed under them that they have not taken yet, 4 at a time
    // (one per partition, in order), and empty ones to file more in. Buffers are
    // handed over, not copied (copying them: +1.2 ms).
    let tables: Vec<Mutex<Vec<Table>>> = many(threads);
    let inboxes: Vec<Mutex<(Vec<Vec<u64>>, Vec<Vec<u64>>)>> = many(threads);
    // Per block of runs, how many are parsed.
    let parsed: Vec<AtomicUsize> = many(runs / BLOCK);
    let [next_chunk, chunks_done, next_run, flushed]: [AtomicUsize; 4] = Default::default();
    // Each thread's totals.
    let finished = Mutex::new(Vec::new());
    let worker = &|w: usize| {
        let mut totals = [EMPTY; GROUPS];
        // This thread's tables and pair buffers: made here, so they are in its cache.
        let table = || (vec![0; slots], vec![0; slots], Vec::with_capacity(rows));
        *tables[w].lock().unwrap() = (0..PARTITIONS_PER_THREAD).map(|_| table()).collect();
        let mut pairs: Vec<_> = (0..parts).map(|_| Vec::with_capacity(capacity)).collect();
        // Thread t's tables take the pairs in its inbox, and give the buffers back
        // empty.
        let mut taken = Vec::new();
        let mut take = |mine: &mut Vec<Table>, t: usize, totals: &mut _| {
            std::mem::swap(&mut taken, &mut inboxes[t].lock().unwrap().0);
            for (k, pairs) in taken.iter_mut().enumerate() {
                add(&mut mine[k % PARTITIONS_PER_THREAD], pairs, totals);
                pairs.clear();
            }
            inboxes[t].lock().unwrap().1.append(&mut taken);
        };
        while let Some(chunk) = claim(&next_chunk, REGION_CHUNKS) {
            fill(chunk);
            chunks_done.fetch_add(1, Release);
        }
        wait(&chunks_done, REGION_CHUNKS);
        // Runs are claimed one at a time, so a thread slowed down parses fewer of
        // them, and no thread waits for another one until the end.
        while let Some(claim) = claim(&next_run, runs) {
            // Claims go round the file's `threads` stripes, so that runs parsed at
            // the same time are far apart and do not share page tables (whose
            // lock their page faults and releases contended on: -0.5 ms).
            let run = claim % threads * RUNS_PER_THREAD + claim / threads;
            // Each thread finds its own cuts, so their pages fault in in parallel.
            let cut = |run| run_start(text, body_start, run, runs);
            let span = (cut(run), cut(run + 1));
            parse_run(text, span, &group_of, &mut totals, &mut pairs);
            for (t, pairs) in pairs.chunks_mut(PARTITIONS_PER_THREAD).enumerate() {
                let mut inbox = inboxes[t].lock().unwrap();
                let (full, empty) = &mut *inbox;
                for pairs in pairs {
                    let fresh = empty.pop().unwrap_or_else(|| Vec::with_capacity(capacity));
                    full.push(std::mem::replace(pairs, fresh));
                }
                // A thread that falls behind (it lost its core) would let its inbox,
                // and the memory peak, grow: past a bound, the filer takes it over.
                // (Normally an inbox holds a few dozen buffers. No tables yet: their
                // thread has not made them.)
                let behind = full.len() > 16 * threads;
                drop(inbox);
                if behind
                    && let Ok(mut theirs) = tables[t].try_lock()
                    && !theirs.is_empty()
                {
                    take(&mut theirs, t, &mut totals);
                }
            }
            // The last of a block's runs to be parsed drops the block's pages.
            // (A call per run: +2 ms, from 4 times the TLB shootdowns.)
            let block = run / BLOCK;
            if parsed[block].fetch_add(1, AcqRel) == BLOCK - 1 {
                release(&text[cut(block * BLOCK)..cut(block * BLOCK + BLOCK)]);
            }
            // This thread's tables, unless another thread holds them.
            if let Ok(mut mine) = tables[w].try_lock() {
                take(&mut mine, w, &mut totals);
            }
        }
        // Once no thread files pairs any more, every thread's tables take their
        // inbox, this thread's first.
        flushed.fetch_add(1, Release);
        wait(&flushed, threads);
        for t in std::iter::once(w).chain(0..threads) {
            take(&mut tables[t].lock().unwrap(), t, &mut totals);
        }
        // The last thread to finish writes the output and exits, without waiting
        // for the others to exit and without dropping anything.
        let mut finished = finished.lock().unwrap();
        finished.push(totals);
        if finished.len() == threads {
            write_output(out_dir, &finished, &keys.lock().unwrap());
            std::process::exit(0);
        }
    };
    thread::scope(|s| {
        for w in 1..threads {
            s.spawn(move || worker(w));
        }
        worker(0);
    });
    unreachable!("the last worker exits the process")
}

/// Writes each group's line, summing the threads' totals.
fn write_output(dir: &str, parts: &[[Totals; GROUPS]], codes: &[Vec<u8>]) {
    // Keys sorted bytewise: the codes, then `_UNMATCHED` ("_" > "Z").
    let mut keys: Vec<_> = codes.iter().cloned().zip(0..).collect();
    keys.push((b"_UNMATCHED".to_vec(), GROUPS - 1));
    keys.sort();
    let mut out = String::new();
    for (key, group) in keys {
        let [count, ids, sum, min, max, names, bytes] = parts.iter().fold(EMPTY, |t, p| {
            // The columns add up, except the min and the max.
            let mut sums: Totals = std::array::from_fn(|k| t[k] + p[group][k]);
            (sums[3], sums[4]) = (t[3].min(p[group][3]), t[4].max(p[group][4]));
            sums
        });
        let ([sum, min, max], key) = ([sum, min, max].map(money), String::from_utf8_lossy(&key));
        let (name_bytes, note_bytes) = (bytes >> 32, bytes & 0xFFFF_FFFF);
        if count > 0 {
            out += &format!("{key} {count} {ids} {sum} {min} {max} {names} ");
            out += &format!("{name_bytes} {note_bytes}\n");
        }
    }
    let path = format!("{dir}/aggregates.txt");
    std::fs::write(&path, out).unwrap_or_else(|e| panic!("write {path}: {e}"));
}

/// Cents as money: sign, integer part, two decimals ("-0.05", "0.00").
fn money(cents: i64) -> String {
    let (sign, abs) = (if cents < 0 { "-" } else { "" }, cents.unsigned_abs());
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// Marks each (name hash, group) pair, and counts in `totals` the pairs seen for
/// the first time. Out of line, like `parse_run`, so that edits to the worker
/// around it cannot change how its loop is compiled.
#[inline(never)]
fn add(table: &mut Table, pairs: &[u64], totals: &mut [Totals; GROUPS]) {
    // Fields as locals: no store through them can change the others.
    let (keys, ids, seen) = table;
    let (buckets, full) = (keys.len() / 4, keys.len() / 8 * 5 * WORDS);
    for &pair in pairs {
        let (key, group) = (pair | 0xFF, (pair & 0xFF) as usize);
        // The top hash bits picked the partition; the bits above the group pick a
        // bucket of 4 slots, compared at once. Slots fill in order and are never
        // emptied, so the first slot that holds the key or is empty is the name's.
        let mut bucket = (pair >> 8) as usize & (buckets - 1);
        // SAFETY: buckets are masked to the table (a multiple of 4 slots), a slot's
        // id has its row of WORDS in `seen`, which covers every group. AVX2 is
        // enabled.
        unsafe {
            let wanted = _mm256_set1_epi64x(key as i64);
            let slot = loop {
                let v = _mm256_loadu_si256(keys.as_ptr().add(bucket * 4).cast());
                let free = _mm256_cmpeq_epi64(v, _mm256_setzero_si256());
                let hit = _mm256_or_si256(_mm256_cmpeq_epi64(v, wanted), free);
                let hit = _mm256_movemask_pd(_mm256_castsi256_pd(hit));
                if hit != 0 {
                    break bucket * 4 + hit.trailing_zeros() as usize;
                }
                bucket = (bucket + 1) & (buckets - 1);
            };
            if *keys.get_unchecked(slot) == 0 {
                assert!(seen.len() < full, "more names than the tables hold");
                *keys.get_unchecked_mut(slot) = key;
                *ids.get_unchecked_mut(slot) = (seen.len() / WORDS) as u16;
                seen.extend_from_slice(&[0; WORDS]);
            }
            let id = *ids.get_unchecked(slot) as usize;
            let word = seen.get_unchecked_mut(id * WORDS + group / 64);
            let bit = 1 << (group % 64);
            totals[group][5] += (*word & bit == 0) as i64; // names_distinct
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
    let start =
        |&c: &usize| c == from || text[c - 1] == b'\n' && matches!(text[c], b'0'..=b'9' | 0xEF);
    (nominal..text.len()).find(start).unwrap_or(text.len())
}

/// Parses the records in `text[start..end]` into `totals`, and files each
/// record's (name hash, group) pair under its partition. The last record must end
/// at `end`. (It takes the table, not a struct holding a Mutex too, which would
/// make every record reload the table's address.) Out of line: inlined into the
/// worker, its registers were allocated with the worker's, and it ran 2-4% slower.
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
#[inline(never)]
fn parse_run(
    text: &[u8],
    (start, end): (usize, usize),
    group_of: &[AtomicU8],
    totals: &mut [Totals; GROUPS],
    pairs: &mut [Vec<u64>],
) {
    let fast_end = end.min(text.len().saturating_sub(256));
    let record = |i: usize| {
        // A non-temporal prefetch ~16 records ahead: the stream then takes less of
        // L2, so more of the 1 MB region table stays there.
        // SAFETY: a prefetch never faults, and wrapping_add keeps the address defined.
        unsafe { _mm_prefetch::<_MM_HINT_NTA>(text.as_ptr().wrapping_add(i + 1024).cast()) };
        match fast_record(text, i, fast_end) {
            Some(r) => r,
            None => walk_record(text, i),
        }
    };
    let mut count = |r: &Record| {
        let &(id, region_id, cents, bytes, hash, _) = r;
        // The stored group + 1, so 0 (not in the table) wraps to `_UNMATCHED`.
        let k = (region_id as usize).min(group_of.len() - 1);
        // SAFETY: k is below the length (which is at least 2).
        let stored = unsafe { group_of.get_unchecked(k) }.load(Relaxed);
        let group = stored.wrapping_sub(1);
        let t = &mut totals[group as usize];
        t[0] += 1;
        t[1] += id as i64;
        t[2] += cents;
        // Branches, not cmov and a store: a new min or max is rare.
        if cents < t[3] {
            t[3] = cents;
        }
        if cents > t[4] {
            t[4] = cents;
        }
        t[6] += bytes as i64;
        // The partition from the top hash bits, by multiply-shift: any count.
        let p = ((hash >> 32) * pairs.len() as u64 >> 32) as usize;
        pairs[p].push(hash & !0xFF | group as u64);
    };
    let mid = run_start(&text[..end], start, 1, 2);
    let (mut i, mut j) = (start, mid);
    while i < mid && j < end {
        let (a, b) = (record(i), record(j));
        count(&a);
        count(&b);
        (i, j) = (a.5, b.5);
    }
    // The rest of the longer half, one record at a time; each half must end exactly
    // at its end (for the first, the cut in the middle).
    for (mut i, end) in [(i, mid), (j, end)] {
        while i < end {
            let r = record(i);
            count(&r);
            i = r.5;
        }
        assert_eq!(i, end, "records.csv is outside spec 2.2");
    }
}

/// A record, `id,name,note,region_id,amount` and a terminator, whose name and note
/// are empty or quoted (spec 2.2), from the bitmaps of the 64 bytes at `i` (rarely
/// 128) and of 32 bytes after the note. None if the name or the note runs past
/// them, or if `i` is not before `fast_end`, which is at least 256 bytes before the
/// end of the text.
#[inline(always)]
fn fast_record(text: &[u8], i: usize, fast_end: usize) -> Option<Record> {
    if i < 8 || i >= fast_end {
        return None;
    }
    // SAFETY (for every access below): `i - 8..i + 256` is inside the text, and every load ends by offset 198 (a comma
    // at 128 or less, a bit index below 64 in the tail's masks, and a few bytes).
    let p = unsafe { text.as_ptr().add(i) };
    let at = |k: usize| unsafe { *p.add(k) };
    let load = |k: usize| unsafe { p.add(k).sub(8).cast::<u64>().read_unaligned() }; // ends at k
    let (quotes, commas) = masks(unsafe { &*p.cast() });
    let inside = prefix_xor(quotes);
    let mut out = commas & !inside;
    let id_end = out.trailing_zeros() as usize;
    out &= out.wrapping_sub(1);
    let name_end = out.trailing_zeros() as usize; // 64 if none
    out &= out.wrapping_sub(1);
    if name_end >= 64 {
        return None;
    }
    // Empty, or quoted with every inner quote half of a `""` pair (spec 2.2). (A
    // one-byte field counts as unquoted, so that the hash's range is never negative.)
    let name_quoted = (name_end > id_end + 2) as usize;
    let name_quotes = (quotes & !(u64::MAX << name_end)).count_ones() as usize;
    let name_bytes = name_end - id_end - 1 - name_quoted - name_quotes / 2;
    // SAFETY: the name is inside the 64 bytes at `i`, which has 8 bytes before it.
    let (s, e) = (i + id_end + 1 + name_quoted, i + name_end - name_quoted);
    let hash = unsafe { hash_words(text, s, e) };
    let (note_end, quotes_to_note) = if out != 0 {
        let e = out.trailing_zeros() as usize;
        (e, (quotes & !(u64::MAX << e)).count_ones())
    } else {
        // The note runs past the first 64 bytes: the next 64, carrying the quote state.
        let (quotes2, commas2) = masks(unsafe { &*p.add(64).cast() });
        let out2 = commas2 & !(prefix_xor(quotes2) ^ ((inside as i64) >> 63) as u64);
        let e = out2.trailing_zeros() as usize; // 64 if none
        let before = quotes.count_ones() + (quotes2 & !(u64::MAX << (e & 63))).count_ones();
        (64 + e, before)
    };
    if note_end >= 128 {
        return None;
    }
    let note_quoted = (note_end > name_end + 1) as usize;
    let note_quotes = quotes_to_note as usize - name_quotes;
    let note_bytes = note_end - name_end - 1 - note_quoted - note_quotes / 2;
    // The tail, `region_id,amount`: its first comma and dot, from its 32 bytes.
    let tail = unsafe { _mm256_loadu_si256(p.add(note_end + 1).cast()) };
    let region_len = bytes_eq(tail, b',').trailing_zeros() as usize;
    let dot = bytes_eq(tail, b'.').trailing_zeros() as usize;
    let id_len = id_end - if load(3) >> 40 == 0xBF_BBEF { 3 } else { 0 }; // a BOM: not data
    let negative = at(note_end + 2 + region_len) == b'-';
    let (region_end, amount_end) = (note_end + 1 + region_len, note_end + 4 + dot);
    let whole = dot - region_len - 1 - negative as usize;
    let words = [load(id_end), load(region_end), load(amount_end)];
    let [id, region_id, c] = numbers(words, [id_len, region_len, whole + 2]);
    let cents = if negative { -(c as i64) } else { c as i64 };
    let bytes = (name_bytes << 32 | note_bytes) as u64;
    let next = i + amount_end + 1 + (at(amount_end) == b'\r') as usize; // CRLF or LF
    Some((id, region_id, cents, bytes, hash, next))
}

/// Any record of spec 2.2 starting at `i`, byte by byte: the ones whose name or
/// note is too long for `fast_record`'s windows, and the last few of the file. Out
/// of line, so that it does not crowd the registers of the fast path's loop.
#[cold]
#[inline(never)]
fn walk_record(text: &[u8], i: usize) -> Record {
    let mut at = i + (text[i] == 0xEF) as usize * 3; // a BOM is not data
    let id = digits(text, &mut at);
    let (name_start, name_end, name_bytes) = text_field(text, &mut at);
    let (_, _, note_bytes) = text_field(text, &mut at);
    at += 1;
    let region_id = digits(text, &mut at);
    let negative = text[at + 1] == b'-';
    at += 1 + negative as usize;
    let whole = digits(text, &mut at);
    at += 1; // the dot
    let cents = (whole * 100 + digits(text, &mut at)) as i64;
    let next = at + 1 + (text[at] == b'\r') as usize; // CRLF or LF
    // The name is hashed from a padded copy, so that the hash's loads stay in bounds
    // whatever the input. (This path is rare.)
    let copy = [&[0; 8], &text[name_start..name_end], &[0; 8]].concat();
    // SAFETY: the copy has 8 bytes on each side of the name.
    let hash = unsafe { hash_words(&copy, 8, copy.len() - 8) };
    let bytes = name_bytes << 32 | note_bytes;
    let cents = if negative { -cents } else { cents };
    (id, region_id, cents, bytes, hash, next)
}

/// The digits at `*at`, which it moves past: their value.
fn digits(text: &[u8], at: &mut usize) -> u64 {
    let mut value = 0;
    while let Some(&digit @ b'0'..=b'9') = text.get(*at) {
        value = value * 10 + (digit - b'0') as u64;
        *at += 1;
    }
    value
}

/// The name or note after the comma at `*at`, empty or quoted (spec 2.2): the range
/// of its value inside the quotes and its unescaped length. Moves `*at` to the comma
/// after it.
fn text_field(text: &[u8], at: &mut usize) -> (usize, usize, u64) {
    let quoted = text[*at + 1] == b'"';
    let (start, mut pairs) = (*at + 1 + quoted as usize, 0);
    *at = start;
    // A quoted value ends at the next `"` that is not one of a `""` pair.
    while quoted {
        *at += text[*at..].iter().position(|&b| b == b'"').unwrap();
        if text[*at + 1] != b'"' {
            break;
        }
        (*at, pairs) = (*at + 2, pairs + 1);
    }
    let field = (start, *at, (*at - start - pairs) as u64);
    *at += quoted as usize;
    field
}

/// Bitmaps of a 64-byte block: bit k is set if byte k is `"`, and if it is `,`.
#[inline(always)]
fn masks(block: &[u8; 64]) -> (u64, u64) {
    // SAFETY: AVX2 is enabled for this build (see the compile_error), and the two
    // unaligned 32-byte loads are inside the 64-byte array.
    let [lo, hi] = [0, 32].map(|k| unsafe { _mm256_loadu_si256(block.as_ptr().add(k).cast()) });
    let bits = |byte| bytes_eq(lo, byte) | bytes_eq(hi, byte) << 32;
    (bits(b'"'), bits(b','))
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

/// Bit k of the result is the XOR of bits 0..=k. Applied to quote bits, it tells
/// for each byte whether it is inside quotes (an opening quote counts as inside, a
/// closing one as outside, and a `""` pair leaves the state unchanged).
fn prefix_xor(bits: u64) -> u64 {
    // A carry-less multiply by all ones XORs every lower bit into each bit.
    // SAFETY: PCLMULQDQ is enabled for this build (see the compile_error).
    unsafe {
        let product = _mm_clmulepi64_si128(_mm_set_epi64x(0, bits as i64), _mm_set1_epi8(-1), 0);
        _mm_cvtsi128_si64(product) as u64
    }
}

/// Three numbers at once, from the 8 bytes ending at each (the id, the region id,
/// and the amount with its dot, which is squeezed out), and how many digits each
/// has (1 to 8; spec 2.2 keeps every one of them within that).
#[inline(always)]
fn numbers(words: [u64; 3], lens: [usize; 3]) -> [u64; 3] {
    // SAFETY: AVX2 is enabled for this build (see the compile_error).
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
        let a = _mm256_extract_epi32::<0>(v) as u32 as u64;
        let b = _mm256_extract_epi32::<1>(v) as u32 as u64;
        [a, b, _mm256_extract_epi32::<4>(v) as u32 as u64]
    }
}

/// A wyhash-style 64-bit hash of `text[s..e]`: each 8-byte word goes through a
/// full 64x64->128 multiply, folded, so every input bit reaches every output bit.
/// The last (up to) 16 bytes are two words without a branch on their length: the
/// first masked to the field, the second ending at `e` (or 0 for 8 bytes or fewer).
///
/// SAFETY: `e - 8..e + 8` must be inside the text.
#[inline(always)]
unsafe fn hash_words(text: &[u8], s: usize, e: usize) -> u64 {
    const P0: u64 = 0xa076_1d64_78bd_642f;
    const P1: u64 = 0xe703_7ed1_a0b4_28db;
    let mix = |a: u64, b: u64| {
        let r = a as u128 * b as u128;
        r as u64 ^ (r >> 64) as u64
    };
    // SAFETY: every load is inside `e - 8..e + 8` (see the function's contract).
    let load = |at: usize| unsafe { text.as_ptr().add(at).cast::<u64>().read_unaligned() };
    let len = (e - s) as u64;
    let (mut h, mut at) = (P0 ^ len, s);
    while e - at > 16 {
        h = mix(h ^ load(at), P1);
        at += 8;
    }
    let rest = e - at;
    // SAFETY: BMI2 is enabled by target-cpu=native on the box (and comes with AVX2 on
    // every Intel and AMD CPU that has it).
    let first = unsafe { _bzhi_u64(load(at), 8 * rest as u32) };
    let last = load(e - 8) & ((rest > 8) as u64).wrapping_neg();
    mix(h ^ first ^ P1, last ^ P0 ^ len)
}
