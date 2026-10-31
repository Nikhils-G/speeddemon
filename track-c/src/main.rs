//! Track C (Join): parse the hostile records.csv, join each record to its country
//! through regions.csv, and write per-country aggregates.
//!
//! How it is fast, and why it is still exact:
//! 1. Both files are mmap'd (pre-warmed page cache: no memory charge, no copy).
//! 2. The file is cut into runs of whole records without reading it twice. By spec
//!    2.2, an LF inside quotes is followed by a letter or `"`, and a record
//!    terminator by the next record's BOM or id digit, so a cut goes after the first
//!    LF that is followed by a digit or a BOM. Every cut is then checked: the run
//!    before it must end exactly there. If one does not (impossible for generator
//!    data), the file is parsed again as one run, which needs only spec 2.1.
//! 3. Records are found with SIMD bitmaps, as simdjson does for JSON: one bitmap of
//!    `"`, one of `,`, and a carry-less multiply that turns the quote bits into an
//!    "inside quotes" mask. What is left are exactly the field separators, and
//!    counting quote bits gives each text field's unescaped length (each `""` is
//!    one byte) without looking at its bytes. The common record takes one such
//!    window of 64 bytes at its start, one 32-byte compare for the region's comma
//!    and the amount's dot, and one AVX2 pass that converts its three numbers; every
//!    check is a predictable branch to the general parser, which handles any other
//!    record (and gives the error message).
//! 4. Distinct names per country: a name's identity is its raw inner bytes (inside
//!    quotes, `""` maps one to one to the unescaped value, and an unquoted value
//!    cannot contain `"`), reduced to a 56-bit hash; with the spec's 100,000 names
//!    the chance that two share one is about 7e-8. Parsing only files a (hash,
//!    country) pair under one of 64 partitions picked by the hash. Between parsing
//!    rounds, each thread takes whole partitions and marks pairs in that
//!    partition's own small name table and bit rows, which stay in its L1/L2 cache.
//!    No two threads ever write the same memory, so there are no atomics or cache
//!    lines bouncing between cores, and the rounds keep the pair buffers small.

mod mmap;

#[cfg(not(all(target_feature = "avx2", target_feature = "pclmulqdq")))]
compile_error!("track-c needs AVX2 and PCLMULQDQ: build with RUSTFLAGS=\"-C target-cpu=native\"");

use std::cell::UnsafeCell;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering::*};
use std::sync::{Condvar, Mutex};
use std::thread;

const RECORDS_HEADER: &[u8] = b"id,name,note,region_id,amount";
const REGIONS_HEADER: &[u8] = b"region_id,country\n";
const BOM: &[u8] = b"\xEF\xBB\xBF";
/// Subtracts '0' from each of 8 digit bytes.
const ZEROS: u64 = 0x3030_3030_3030_3030;
const UNMATCHED: &[u8] = b"_UNMATCHED";
/// The file is parsed in this many rounds of one run per thread, so the pair
/// buffers only ever hold one round's pairs (about 3 MB in all).
const ROUNDS: usize = 8;
/// Names are split by their top hash bits into this many partitions.
const PARTITION_BITS: u32 = 6;
const PARTITIONS: usize = 1 << PARTITION_BITS;
/// Group indices are bytes: country codes from 0, `_UNMATCHED` the last one, so
/// that tables indexed by a group need no bounds checks.
const GROUPS: usize = 256;
const UNMATCHED_GROUP: usize = GROUPS - 1;
/// Words of group bits per name.
const WORDS: usize = GROUPS / 64;
/// Names per partition for the first attempt: the spec's 100,000 names are about
/// 1,560 per partition.
const FIRST_SLOTS: usize = 1 << 11;

fn main() {
    mmap::no_huge_pages();
    mmap::big_heap_steps();
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    if let Err(message) = run(&data_dir, &out_dir) {
        eprintln!("track-c: {message}");
        std::process::exit(1);
    }
}

fn run(data_dir: &str, out_dir: &str) -> Result<(), String> {
    let open = |name: &str| -> Result<&'static [u8], String> {
        let path = format!("{data_dir}/{name}");
        let file = File::open(&path).map_err(|e| format!("open {path}: {e}"))?;
        mmap::map(&file).map_err(|e| format!("mmap {path}: {e}"))
    };
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let regions = open("regions.csv")?;
    let region_lines = regions
        .strip_prefix(REGIONS_HEADER)
        .ok_or("unexpected regions.csv header")?;
    let region_chunks = split_at_lines(region_lines, threads);
    let records = open("records.csv")?;

    // The header is the first record; everything after it is data.
    let body_start = skip_header(records)?;
    let (mut guessed, mut slots) = (true, FIRST_SLOTS);
    let (parts, keys) = loop {
        match aggregate(records, body_start, guessed, threads, &region_chunks, slots) {
            Ok(done) => break done,
            Err(ParseError::NamesFull) => slots *= 2,
            // A wrong guessed cut shows up as a misaligned run or as a parse error.
            Err(ParseError::Misaligned | ParseError::Bad(_)) if guessed => {
                // Fall back to one run over the whole file: exact under spec 2.1.
                guessed = false;
            }
            Err(ParseError::Misaligned) => {
                return Err("records.csv: a record crosses a run boundary".into());
            }
            Err(ParseError::Bad(message)) => return Err(message),
        }
    };

    let mut out = Vec::new();
    // Group numbers are in output order: codes sorted, then `_UNMATCHED` ("_" > "Z").
    for g in 0..keys.len() {
        let index = if g + 1 == keys.len() {
            UNMATCHED_GROUP
        } else {
            g
        };
        let mut total = Totals::default();
        for part in &parts {
            total.merge(&part[index]);
        }
        if total.count == 0 {
            continue;
        }
        let t = total;
        let (sum, min, max) = (
            money(t.amount_sum),
            money(t.amount_min),
            money(t.amount_max),
        );
        let key = String::from_utf8_lossy(&keys[g]);
        let (count, ids, names) = (t.count, t.id_sum, t.names_distinct);
        let line = format!(
            "{key} {count} {ids} {sum} {min} {max} {names} {} {}\n",
            t.name_bytes, t.note_bytes
        );
        out.extend_from_slice(line.as_bytes());
    }
    let output_path = format!("{out_dir}/aggregates.txt");
    File::create(&output_path)
        .and_then(|mut f| f.write_all(&out))
        .map_err(|e| format!("write {output_path}: {e}"))?;
    // Done: exit without dropping the tables, one free at a time.
    std::process::exit(0)
}

/// Builds the join table from regions.csv, then parses and aggregates every run,
/// one round of `threads` runs at a time, and returns each thread's totals and
/// the groups' keys. One set of threads does all of it.
fn aggregate(
    text: &[u8],
    body_start: usize,
    guessed: bool,
    threads: usize,
    region_chunks: &[&[u8]],
    slots: usize,
) -> Result<(Vec<Box<[Totals; GROUPS]>>, Vec<Vec<u8>>), ParseError> {
    let seen: Vec<AtomicBool> = (0..CODES).map(|_| AtomicBool::new(false)).collect();
    let ids = AtomicUsize::new(0); // the largest region id + 1
    let table: Turns<Option<Result<(Regions, Vec<u8>), String>>> = Turns::new(None);
    // Each table is made by the first thread to take it, so their pages fault in
    // in parallel rather than all on this thread.
    let partitions: Vec<Turns<Option<Partition>>> =
        (0..PARTITIONS).map(|_| Turns::new(None)).collect();
    // Pairs per thread per partition in one run, with room to spare (records are
    // about 60 bytes), so the buffers are allocated once, by their own thread.
    let units = threads * ROUNDS;
    let capacity = text.len() / (units * 60 * PARTITIONS) * 5 / 4 + 16;
    let pairs: Vec<Turns<Vec<Vec<u64>>>> = (0..threads).map(|_| Turns::new(Vec::new())).collect();
    let next_partition: Vec<AtomicUsize> = (0..ROUNDS).map(|_| AtomicUsize::new(0)).collect();
    let barrier = SpinBarrier::new(threads);
    let results: Vec<Result<Box<[Totals; GROUPS]>, ParseError>> = thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|w| {
                let (partitions, pairs, next_partition, barrier) =
                    (&partitions, &pairs, &next_partition, &barrier);
                let (seen, ids, table) = (&seen, &ids, &table);
                s.spawn(move || {
                    let mut totals: Box<[Totals; GROUPS]> =
                        vec![Totals::default(); GROUPS].try_into().ok().unwrap();
                    let mut error = None;
                    // SAFETY: before the first barrier, only this thread touches its buffers.
                    // (Not `vec![...; PARTITIONS]`: cloning a Vec does not keep its capacity.)
                    *unsafe { pairs[w].get() } = (0..PARTITIONS)
                        .map(|_| Vec::with_capacity(capacity))
                        .collect();
                    // regions.csv, in two passes over this thread's lines: the first finds
                    // the countries, which numbers the groups; the second fills the table.
                    let lines = region_chunks.get(w).copied().unwrap_or_default();
                    // (A load first: storing every time would bounce the lines between cores.)
                    let mark = |_, code: usize| {
                        _ = seen[code].load(Relaxed) || seen[code].swap(true, Relaxed)
                    };
                    match scan_regions(lines, mark) {
                        Ok(n) => _ = ids.fetch_max(n, Relaxed),
                        Err(e) => error = Some(ParseError::Bad(e)),
                    }
                    barrier.wait();
                    if w == 0 {
                        // SAFETY: between these two barriers only this thread touches it.
                        *unsafe { table.get() } = Some(Regions::number(seen, ids.load(Relaxed)));
                    }
                    barrier.wait();
                    // SAFETY: from here on the table is only read (its atomics stored to).
                    let regions = match unsafe { table.read() } {
                        Some(Ok((regions, codes))) if error.is_none() => {
                            let fill = |id: usize, code: usize| {
                                regions.group_of[id].store(codes[code], Relaxed)
                            };
                            if let Err(e) = scan_regions(lines, fill) {
                                error = Some(ParseError::Bad(e));
                            }
                            Some(regions)
                        }
                        Some(Err(e)) => {
                            error = Some(ParseError::Bad(e.clone()));
                            None
                        }
                        _ => None,
                    };
                    barrier.wait();
                    for round in 0..ROUNDS {
                        let run = round * threads + w;
                        if let (None, Some(regions)) = (&error, regions) {
                            // SAFETY: while parsing, each thread touches only its own buffers.
                            let mine = unsafe { pairs[w].get() };
                            mine.iter_mut().for_each(Vec::clear);
                            // Each thread finds its own cuts, so their pages fault in in parallel.
                            let start = run_start(text, body_start, run, units, guessed);
                            let end = run_start(text, body_start, run + 1, units, guessed);
                            let mine = mine.as_mut_slice().try_into().unwrap();
                            match parse_run(text, start, end, regions, &mut totals, mine) {
                                // Drop the run's pages now, in parallel, instead of
                                // the kernel unmapping all 50,000 at exit on one core.
                                Ok(last) if last == end => mmap::release(&text[start..end]),
                                Ok(_) => error = Some(ParseError::Misaligned),
                                Err(e) => error = Some(e),
                            }
                        }
                        barrier.wait();
                        loop {
                            let p = next_partition[round].fetch_add(1, Relaxed);
                            if p >= PARTITIONS || error.is_some() {
                                break;
                            }
                            // SAFETY: the counter gives partition p to this thread alone
                            // for this round, and every buffer's writer is past the barrier.
                            let partition = unsafe { partitions[p].get() }
                                .get_or_insert_with(|| Partition::new(slots));
                            for buffers in pairs {
                                let mine = &unsafe { buffers.read() }[p];
                                if let Err(e) = partition.add(mine, &mut totals) {
                                    error = Some(e);
                                }
                            }
                        }
                        barrier.wait();
                    }
                    error.map_or(Ok(totals), Err)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // A bad cut can also make the table overflow, so the cut errors come first.
    let mut parts = Vec::new();
    let mut full = false;
    for result in results {
        match result {
            Ok(totals) => parts.push(totals),
            Err(ParseError::NamesFull) => full = true,
            Err(e) => return Err(e),
        }
    }
    if full {
        return Err(ParseError::NamesFull);
    }
    match table.0.into_inner() {
        Some(Ok((regions, _))) => Ok((parts, regions.keys)),
        _ => Err(ParseError::Bad("regions.csv: no table".into())),
    }
}

/// A barrier whose waiters spin for a while before sleeping. Each round's phases
/// last a millisecond or two, and waking a dozen sleeping threads takes a good
/// part of that (std's Barrier: about 1 ms more per run). The spin is bounded,
/// then waiters block, so a thread that lost its core is never starved by them.
struct SpinBarrier {
    threads: usize,
    arrived: AtomicUsize,
    generation: AtomicUsize,
    lock: Mutex<()>,
    wake: Condvar,
}

impl SpinBarrier {
    fn new(threads: usize) -> Self {
        let (arrived, generation) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let (lock, wake) = (Mutex::new(()), Condvar::new());
        SpinBarrier {
            threads,
            arrived,
            generation,
            lock,
            wake,
        }
    }

    fn wait(&self) {
        let generation = self.generation.load(Acquire);
        if self.arrived.fetch_add(1, AcqRel) + 1 == self.threads {
            self.arrived.store(0, Relaxed);
            // Under the lock, so a waiter cannot check and then miss the wake-up.
            let _guard = self.lock.lock().unwrap();
            self.generation.fetch_add(1, Release);
            self.wake.notify_all();
            return;
        }
        for _ in 0..1 << 14 {
            if self.generation.load(Acquire) != generation {
                return;
            }
            std::hint::spin_loop();
        }
        let mut guard = self.lock.lock().unwrap();
        while self.generation.load(Acquire) == generation {
            guard = self.wake.wait(guard).unwrap();
        }
    }
}

/// A value that threads take turns on. The barriers between phases order every
/// access; within a phase, each use states why it is the only writer.
struct Turns<T>(UnsafeCell<T>);

// SAFETY: see the type's comment; T itself is only moved between threads.
unsafe impl<T: Send> Sync for Turns<T> {}

impl<T> Turns<T> {
    fn new(value: T) -> Self {
        Turns(UnsafeCell::new(value))
    }

    /// SAFETY: no other thread may access the value during this phase.
    #[allow(clippy::mut_from_ref)]
    unsafe fn get(&self) -> &mut T {
        unsafe { &mut *self.0.get() }
    }

    /// SAFETY: no thread may write the value during this phase.
    unsafe fn read(&self) -> &T {
        unsafe { &*self.0.get() }
    }
}

/// One partition's names: an open-addressing table of name hashes in buckets of
/// 4, kept at most half full so that a lookup is nearly always one bucket, with a dense id per
/// name, and one row of bits per id, a bit per group.
struct Partition {
    keys: Vec<u64>, // 0 = empty, else the name hash with its low byte set to 0xFF
    ids: Vec<u16>,  // by slot
    seen: Vec<u64>, // WORDS per id
    used: usize,
}

impl Partition {
    /// A table for up to `names` names.
    fn new(names: usize) -> Self {
        Partition {
            keys: vec![0; names * 2],
            ids: vec![0; names * 2],
            seen: vec![0; names * WORDS],
            used: 0,
        }
    }

    /// Marks each (name hash, group) pair, and counts in `totals` the pairs seen
    /// for the first time.
    fn add(&mut self, pairs: &[u64], totals: &mut [Totals; GROUPS]) -> Result<(), ParseError> {
        // Fields as locals: no store through them can change the others.
        let Partition {
            keys,
            ids,
            seen,
            used,
        } = self;
        let (buckets, full) = (keys.len() / 4, keys.len() / 2);
        for &pair in pairs {
            let (key, group) = (pair | 0xFF, (pair & 0xFF) as usize);
            // The top hash bits picked the partition; the bits above the group pick
            // a bucket of 4 slots, compared at once. Slots fill in order and are never
            // emptied, so a bucket with an empty slot ends the search.
            let mut bucket = (pair >> 8) as usize & (buckets - 1);
            // SAFETY: buckets are masked to the table (a multiple of 4 slots), ids are
            // below `used` <= names, and `seen` has WORDS per name, which cover every
            // group. AVX2 is enabled.
            unsafe {
                use std::arch::x86_64::*;
                let wanted = _mm256_set1_epi64x(key as i64);
                let slot = loop {
                    let v = _mm256_loadu_si256(keys.as_ptr().add(bucket * 4).cast());
                    let found =
                        _mm256_movemask_pd(_mm256_castsi256_pd(_mm256_cmpeq_epi64(v, wanted)));
                    if found != 0 {
                        break bucket * 4 + found.trailing_zeros() as usize;
                    }
                    let free = _mm256_cmpeq_epi64(v, _mm256_setzero_si256());
                    let free = _mm256_movemask_pd(_mm256_castsi256_pd(free));
                    if free != 0 {
                        if *used == full {
                            return Err(ParseError::NamesFull);
                        }
                        let slot = bucket * 4 + free.trailing_zeros() as usize;
                        *keys.get_unchecked_mut(slot) = key;
                        *ids.get_unchecked_mut(slot) = *used as u16;
                        *used += 1;
                        break slot;
                    }
                    bucket = (bucket + 1) & (buckets - 1);
                };
                let word =
                    seen.get_unchecked_mut(*ids.get_unchecked(slot) as usize * WORDS + group / 64);
                let bit = 1 << (group % 64);
                totals[group].names_distinct += (*word & bit == 0) as u64;
                *word |= bit;
            }
        }
        Ok(())
    }
}

/// The join table: region id -> group index. Groups are the country codes in
/// sorted order, then `_UNMATCHED` as the last one.
struct Regions {
    /// Dense by region id: group + 1, or 0 for ids the table does not contain.
    group_of: Vec<AtomicU8>,
    /// Group index -> output key.
    keys: Vec<Vec<u8>>,
}

/// Country codes are `[A-Z]{3}` (spec 2.3), so each has an index below 26^3.
const CODES: usize = 26 * 26 * 26;

impl Regions {
    /// Numbers the groups from the codes seen in the first pass (codes sorted, then
    /// `_UNMATCHED`), with an empty table for region ids below `ids`. Also returns
    /// what the second pass stores for each code: its group + 1.
    fn number(seen: &[AtomicBool], ids: usize) -> Result<(Self, Vec<u8>), String> {
        let mut codes = vec![0; CODES];
        let mut keys = Vec::new();
        for code in (0..CODES).filter(|&c| seen[c].load(Relaxed)) {
            if keys.len() + 2 > u8::MAX as usize {
                return Err("regions.csv: too many countries".into());
            }
            codes[code] = keys.len() as u8 + 1;
            keys.push(
                [code / 676, code / 26 % 26, code % 26]
                    .map(|letter| b'A' + letter as u8)
                    .to_vec(),
            );
        }
        keys.push(UNMATCHED.to_vec());
        // Zeroed by calloc, so the pages fault in on the threads that fill them.
        // One more entry than the largest id, for every id above it.
        let mut zeros = std::mem::ManuallyDrop::new(vec![0u8; ids + 1]);
        // SAFETY: AtomicU8 has the size, alignment and bit validity of u8.
        let group_of = unsafe {
            Vec::from_raw_parts(
                zeros.as_mut_ptr().cast::<AtomicU8>(),
                zeros.len(),
                zeros.capacity(),
            )
        };
        Ok((Regions { group_of, keys }, codes))
    }

    /// The group of a region id: its stored value minus 1, so 0 (not in the
    /// table) wraps to UNMATCHED_GROUP.
    #[inline(always)]
    fn group(&self, region_id: u64) -> u8 {
        let k = (region_id as usize).min(self.group_of.len() - 1);
        // SAFETY: k is below the length (which is at least 1).
        unsafe { self.group_of.get_unchecked(k) }
            .load(Relaxed)
            .wrapping_sub(1)
    }
}

/// Calls `visit(region_id, country code index)` for every `region_id "," [A-Z]{3}
/// LF` line of `chunk`; returns the largest region id + 1 (0 for none).
fn scan_regions(chunk: &[u8], visit: impl Fn(usize, usize)) -> Result<usize, String> {
    let (mut i, mut ids) = (0, 0);
    while i < chunk.len() {
        if let Some((id, code, len)) = region_line(&chunk[i..]) {
            visit(id, code);
            ids = ids.max(id + 1);
            i += len;
            continue;
        }
        let comma = i + chunk[i..]
            .iter()
            .position(|&b| b == b',')
            .ok_or("regions.csv: a line without a comma")?;
        let id = parse_digits(&chunk[i..comma]).ok_or("regions.csv: bad region_id")? as usize;
        let code = chunk
            .get(comma + 1..comma + 4)
            .filter(|c| c.iter().all(u8::is_ascii_uppercase));
        let code = code
            .filter(|_| chunk.get(comma + 4) == Some(&b'\n'))
            .ok_or("regions.csv: bad country")?;
        visit(
            id,
            code.iter()
                .fold(0, |acc, &b| acc * 26 + (b - b'A') as usize),
        );
        ids = ids.max(id + 1);
        i = comma + 5;
    }
    Ok(ids)
}

/// The common `region_id "," [A-Z]{3} LF` line, with at most 7 digits, from two
/// loads: the region id, the code's index and the line's length.
#[inline(always)]
fn region_line(line: &[u8]) -> Option<(usize, usize, usize)> {
    let bytes = line.first_chunk::<16>()?;
    let word = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    let x = word ^ 0x2C2C_2C2C_2C2C_2C2C; // the first comma is the first zero byte
    let k = ((x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080).trailing_zeros()
        / 8) as usize;
    let digits = (word ^ ZEROS) << (8 * (8 - k) & 63);
    let bad = (digits.wrapping_add(0x7676_7676_7676_7676) | digits) & 0x8080_8080_8080_8080;
    let code = &bytes[k + 1..k + 4];
    let ok = (1..8).contains(&k) && bad == 0 && bytes[k + 4] == b'\n';
    (ok && code.iter().all(u8::is_ascii_uppercase)).then(|| {
        let index = code
            .iter()
            .fold(0, |acc, &b| acc * 26 + (b - b'A') as usize);
        (eight_digits(digits) as usize, index, k + 5)
    })
}

/// Splits `data` into about `parts` slices, each ending just after a newline.
fn split_at_lines(data: &[u8], parts: usize) -> Vec<&[u8]> {
    let mut chunks = Vec::with_capacity(parts);
    let mut start = 0;
    for p in 1..=parts {
        let mut end = (data.len() * p / parts).max(start);
        while end < data.len() && (end == 0 || data[end - 1] != b'\n') {
            end += 1;
        }
        if end > start {
            chunks.push(&data[start..end]);
        }
        start = end;
    }
    chunks
}

#[derive(Clone)]
struct Totals {
    count: u64,
    id_sum: u64,
    amount_sum: i64,
    amount_min: i64,
    amount_max: i64,
    names_distinct: u64,
    name_bytes: u64,
    note_bytes: u64,
}

impl Default for Totals {
    fn default() -> Self {
        Totals {
            count: 0,
            id_sum: 0,
            amount_sum: 0,
            amount_min: i64::MAX,
            amount_max: i64::MIN,
            names_distinct: 0,
            name_bytes: 0,
            note_bytes: 0,
        }
    }
}

impl Totals {
    fn merge(&mut self, other: &Totals) {
        self.count += other.count;
        self.id_sum += other.id_sum;
        self.amount_sum += other.amount_sum;
        self.amount_min = self.amount_min.min(other.amount_min);
        self.amount_max = self.amount_max.max(other.amount_max);
        self.names_distinct += other.names_distinct;
        self.name_bytes += other.name_bytes;
        self.note_bytes += other.note_bytes;
    }
}

enum ParseError {
    /// A partition's name table passed 7/8 full; retry with bigger ones.
    NamesFull,
    /// A run did not end where the next one starts (only possible for a guessed cut).
    Misaligned,
    Bad(String),
}

/// Returns the offset of the first data record, after the BOM and header.
fn skip_header(text: &[u8]) -> Result<usize, String> {
    let start = if text.starts_with(BOM) { BOM.len() } else { 0 };
    let rest = &text[start..];
    let end = rest
        .strip_prefix(RECORDS_HEADER)
        .ok_or("unexpected records.csv header")?;
    let terminator = match end {
        [b'\r', b'\n', ..] => 2,
        [b'\n', ..] => 1,
        [] => 0,
        _ => return Err("unexpected records.csv header".into()),
    };
    Ok(start + RECORDS_HEADER.len() + terminator)
}

/// Where run `k` of the `parts` runs of `text[from..]` starts (`parts`: the end):
/// at a guessed record start (see the top of the file), found by looking at a few
/// bytes after the nominal cut.
fn run_start(text: &[u8], from: usize, k: usize, parts: usize, guessed: bool) -> usize {
    if k == parts {
        return text.len();
    }
    if k == 0 || !guessed {
        return from; // unguessed: every run but the last is empty
    }
    // Monotonic in k: a search that passes the next nominal point finds the same cut.
    let mut i = from + (text.len() - from) * k / parts;
    while i + 1 < text.len()
        && !(text[i] == b'\n' && (text[i + 1].is_ascii_digit() || text[i + 1] == BOM[0]))
    {
        i += 1;
    }
    (i + 1).min(text.len())
}

/// Parses the records in `text[start..end]` into `totals`, and files each
/// record's (name hash, group) pair under its partition. Returns where the last
/// record ended, which is `end` when `start` and `end` are record starts.
fn parse_run(
    text: &[u8],
    start: usize,
    end: usize,
    regions: &Regions,
    totals: &mut [Totals; GROUPS],
    pairs: &mut [Vec<u64>; PARTITIONS],
) -> Result<usize, ParseError> {
    let mut i = start;
    while i < end {
        let r = match fast_record(text, i) {
            Some(r) => r,
            None => slow_record(text, i)?,
        };
        let group = regions.group(r.region_id);
        let total = &mut totals[group as usize];
        total.count += 1;
        total.id_sum += r.id;
        total.amount_sum += r.cents;
        total.amount_min = total.amount_min.min(r.cents);
        total.amount_max = total.amount_max.max(r.cents);
        total.name_bytes += r.name_bytes;
        total.note_bytes += r.note_bytes;
        pairs[(r.hash >> (64 - PARTITION_BITS)) as usize].push(r.hash & !0xFF | group as u64);
        i = r.next;
    }
    Ok(i.min(text.len()))
}

/// One parsed record: its values, its name's hash, and where the next one starts.
struct Record {
    id: u64,
    region_id: u64,
    cents: i64,
    name_bytes: u64,
    note_bytes: u64,
    hash: u64,
    next: usize,
}

/// The common record, `id,"name","note",region_id,amount` and a terminator,
/// with no unquoted text: its separators from the bitmaps of the 64 bytes at `i`
/// (rarely 128) and of 32 bytes after the note, with every check folded into one
/// flag. None sends the record to `slow_record`, which handles every case.
#[inline(always)]
fn fast_record(text: &[u8], i: usize) -> Option<Record> {
    // The header comes first, so a record start is at least 8 bytes in.
    if i < 8 || text.len() - i < 256 {
        return None;
    }
    // SAFETY (for every access below): `i - 8..i + 256` is inside the text, and
    // every load ends by offset 198 (a comma at 128 or less, a bit index below 64
    // in the tail's masks, and a few bytes).
    let p = unsafe { text.as_ptr().add(i) };
    let at = |k: usize| unsafe { *p.add(k) };
    let load = |k: usize| unsafe { p.add(k).sub(8).cast::<u64>().read_unaligned() }; // ends at k
    let (quotes, commas, _) = masks(unsafe { &*p.cast() });
    let inside = prefix_xor(quotes);
    let mut out = commas & !inside;
    let id_end = out.trailing_zeros() as usize;
    out &= out.wrapping_sub(1);
    let name_end = out.trailing_zeros() as usize;
    out &= out.wrapping_sub(1);
    // The name: quoted (its first and last bytes are quotes), or empty.
    let name_quoted = (quotes >> (id_end + 1)) & (quotes >> (name_end - 1)) & 1 == 1;
    if name_end >= 64
        || !name_quoted && name_end != id_end + 1
        || name_end < id_end + 3 && name_quoted
    {
        return None;
    }
    let name_quotes = (quotes & !(u64::MAX << name_end)).count_ones() as usize;
    let name_bytes = name_end - id_end - 1 - name_quoted as usize - name_quotes / 2;
    let (name_s, name_e) = (
        id_end + 1 + name_quoted as usize,
        name_end - name_quoted as usize,
    );
    // SAFETY: `name_s <= name_e`, inside the 64 bytes at `i`, which has 8 bytes before it.
    let hash = unsafe { hash_words(text, i + name_s, i + name_e) };
    let (note_end, quotes_to_note) = if out != 0 {
        let e = out.trailing_zeros() as usize;
        (e, (quotes & !(u64::MAX << e)).count_ones())
    } else {
        // The note runs past the first 64 bytes: the next 64, carrying the quote state.
        let (quotes2, commas2, _) = masks(unsafe { &*p.add(64).cast() });
        let out2 = commas2 & !(prefix_xor(quotes2) ^ ((inside as i64) >> 63) as u64);
        let e = out2.trailing_zeros() as usize; // 64 if none
        let before = quotes.count_ones() + (quotes2 & !(u64::MAX << (e & 63))).count_ones();
        (64 + e, before)
    };
    let note_quoted = (at(name_end + 1) == b'"') & (at(note_end - 1) == b'"');
    if note_end >= 128 || !note_quoted && note_end != name_end + 1 {
        return None;
    }
    let note_quotes = quotes_to_note as usize - name_quotes;
    // The tail, `region_id,amount`: its first comma and dot, from its 32 bytes.
    let (tail_commas, dots) = {
        use std::arch::x86_64::*;
        let v = unsafe { _mm256_loadu_si256(p.add(note_end + 1).cast()) };
        let eq = |byte: u8| movemask(unsafe { _mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8)) });
        (eq(b','), eq(b'.'))
    };
    let region_len = tail_commas.trailing_zeros() as usize;
    let dot = dots.trailing_zeros() as usize;
    let id_len = id_end - if load(3) >> 40 == 0xBF_BBEF { 3 } else { 0 }; // a BOM: not data
    let negative = at(note_end + 2 + region_len) == b'-';
    // `-?[0-9]{1,5}\.[0-9]{2}`, then CRLF or LF.
    let amount_end = note_end + 4 + dot;
    let whole = dot.wrapping_sub(region_len + 1 + negative as usize);
    let terminator = unsafe { p.add(amount_end).cast::<u16>().read_unaligned() };
    let crlf = terminator == 0x0A0D;
    // (`|`, not `||`: CRLF or LF is a coin toss, so no branch on it.)
    if ((id_len.wrapping_sub(1) | region_len.wrapping_sub(1)) >= 8)
        | (whole.wrapping_sub(1) >= 5)
        | !crlf & (terminator as u8 != b'\n')
    {
        return None;
    }
    let words = [
        load(id_end),
        load(note_end + 1 + region_len),
        load(amount_end),
    ];
    let ([id, region_id, cents], digits_ok) = numbers(words, [id_len, region_len, whole + 2]);
    digits_ok.then(|| Record {
        id,
        region_id,
        cents: if negative {
            -(cents as i64)
        } else {
            cents as i64
        },
        name_bytes: name_bytes as u64,
        note_bytes: (note_end - name_end - 1 - note_quoted as usize - note_quotes / 2) as u64,
        hash,
        next: i + amount_end + 1 + crlf as usize,
    })
}

/// Any record starting at `i`, parsed by the general separator scan. Kept out of
/// line, so that it does not crowd the registers of the fast path's loop.
#[cold]
#[inline(never)]
fn slow_record(text: &[u8], i: usize) -> Result<Record, ParseError> {
    let bad = |at: usize, what: &str| ParseError::Bad(format!("records.csv byte {at}: {what}"));
    let mut scan = Scanner::new(text, i);
    let id_start = if text[i..].starts_with(BOM) {
        i + BOM.len()
    } else {
        i
    }; // not data
    let (id_end, q0) = scan.next();
    let (name_end, q1) = scan.next();
    let (note_end, q2) = scan.next();
    let (region_end, _) = scan.next();
    let (amount_end, _) = scan.next();
    let commas = [id_end, name_end, note_end, region_end]
        .iter()
        .all(|&e| text.get(e) == Some(&b','));
    if !commas || amount_end < text.len() && text[amount_end] != b'\n' {
        return Err(bad(i, "expected five fields and a record terminator"));
    }
    let id = digits(text, id_start, id_end).ok_or_else(|| bad(id_start, "bad id"))?;
    let (name, name_bytes) = text_value(text, id_end + 1, name_end, q1 - q0)
        .ok_or_else(|| bad(id_end + 1, "bad name"))?;
    let (_, note_bytes) = text_value(text, name_end + 1, note_end, q2 - q1)
        .ok_or_else(|| bad(name_end + 1, "bad note"))?;
    let region_id =
        digits(text, note_end + 1, region_end).ok_or_else(|| bad(note_end + 1, "bad region_id"))?;
    let cents = amount(text, region_end + 1, amount_end)
        .ok_or_else(|| bad(region_end + 1, "bad amount"))?;
    Ok(Record {
        id,
        region_id,
        cents,
        name_bytes,
        note_bytes,
        hash: hash(text, name.0, name.1),
        next: amount_end + 1,
    })
}

/// Walks the field separators of a run of records (every `,` and LF outside
/// quotes) in order, 64 bytes at a time.
struct Scanner<'a> {
    text: &'a [u8],
    block: usize,       // offset of the current block
    separators: u64,    // its separators not returned yet
    quotes: u64,        // its quote bits
    quotes_before: u64, // quotes between the start of the run and the block
    inside: u64,        // all ones if the block ended inside quotes
}

impl<'a> Scanner<'a> {
    /// Starts at `start`, which must be outside quotes: a record start.
    fn new(text: &'a [u8], start: usize) -> Self {
        let mut scanner = Scanner {
            text,
            block: start,
            separators: 0,
            quotes: 0,
            quotes_before: 0,
            inside: 0,
        };
        scanner.load(start);
        scanner
    }

    fn load(&mut self, at: usize) {
        let rest = &self.text[at.min(self.text.len())..];
        let (quotes, commas, lfs) = match rest.first_chunk::<64>() {
            Some(block) => masks(block),
            None => {
                let mut padded = [0; 64];
                padded[..rest.len()].copy_from_slice(rest);
                masks(&padded)
            }
        };
        let inside = prefix_xor(quotes) ^ self.inside;
        self.inside = ((inside as i64) >> 63) as u64;
        self.quotes_before += self.quotes.count_ones() as u64;
        (self.block, self.quotes, self.separators) = (at, quotes, (commas | lfs) & !inside);
    }

    /// The offset of the next separator, and the number of quotes before it. Past
    /// the last one: the length of the text (the final record may lack a terminator).
    #[inline(always)]
    fn next(&mut self) -> (usize, u64) {
        while self.separators == 0 {
            if self.block + 64 >= self.text.len() {
                return (
                    self.text.len(),
                    self.quotes_before + self.quotes.count_ones() as u64,
                );
            }
            self.load(self.block + 64);
        }
        let k = self.separators.trailing_zeros();
        self.separators &= self.separators - 1;
        (
            self.block + k as usize,
            self.quotes_before + (self.quotes & ((1 << k) - 1)).count_ones() as u64,
        )
    }
}

/// Bitmaps of a 64-byte block: bit k is set if byte k is `"`, `,` or LF.
#[inline(always)]
fn masks(block: &[u8; 64]) -> (u64, u64, u64) {
    use std::arch::x86_64::*;
    // SAFETY: AVX2 is enabled for this build (see the compile_error), and the two unaligned
    // 32-byte loads are inside the 64-byte array.
    unsafe {
        let (lo, hi) = (
            _mm256_loadu_si256(block.as_ptr().cast()),
            _mm256_loadu_si256(block.as_ptr().add(32).cast()),
        );
        let eq = |v, byte: u8| _mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8));
        let bits = |byte| movemask(eq(lo, byte)) | movemask(eq(hi, byte)) << 32;
        (bits(b'"'), bits(b','), bits(b'\n'))
    }
}

/// `vpmovmskb`, kept opaque to the optimizer. LLVM rewrites the intrinsic as a
/// vector of booleans, and on CPUs without AVX-512 (the scoring box) it then
/// assembled our 64-bit masks one bit at a time: a quarter of the run time.
#[inline(always)]
fn movemask(v: std::arch::x86_64::__m256i) -> u64 {
    // A u64: writing the 32-bit register clears the top half, so no zero-extension.
    let mask: u64;
    // SAFETY: AVX2 is enabled for this build; the instruction reads one register.
    unsafe {
        std::arch::asm!("vpmovmskb {0:e}, {1}", out(reg) mask, in(ymm_reg) v, options(pure, nomem, nostack))
    };
    mask
}

/// Bit k of the result is the XOR of bits 0..=k. Applied to quote bits, it tells
/// for each byte whether it is inside quotes (an opening quote counts as inside, a
/// closing one as outside, and a `""` pair leaves the state unchanged).
fn prefix_xor(bits: u64) -> u64 {
    use std::arch::x86_64::*;
    // A carry-less multiply by all ones XORs every lower bit into each bit.
    // SAFETY: PCLMULQDQ is enabled for this build (see the compile_error).
    unsafe {
        _mm_cvtsi128_si64(_mm_clmulepi64_si128(
            _mm_set_epi64x(0, bits as i64),
            _mm_set1_epi8(-1),
            0,
        )) as u64
    }
}

/// A name or note field `text[s..e]` that holds `quotes` double quotes: the range of
/// its raw value bytes, and its unescaped length. Inner quotes are not checked to
/// pair up: the separator scan only proves that their count is even. An unpaired
/// one (invalid, never generated) would be counted as half of a `""`.
#[inline(always)]
fn text_value(text: &[u8], s: usize, e: usize, quotes: u64) -> Option<((usize, usize), u64)> {
    if quotes == 0 {
        return Some(((s, e), (e - s) as u64));
    }
    let quoted = e - s >= 2 && text[s] == b'"' && text[e - 1] == b'"';
    quoted.then(|| ((s + 1, e - 1), (e - s) as u64 - 1 - quotes / 2))
}

/// `1*DIGIT` in `text[s..e]`. Up to 8 digits: one load and SWAR arithmetic.
#[inline(always)]
fn digits(text: &[u8], s: usize, e: usize) -> Option<u64> {
    let len = e.checked_sub(s)?;
    if (1..=8).contains(&len) && e >= 8 {
        // The 8 bytes ending at the field, minus '0', with the bytes before the
        // field zeroed: they read as leading zeros.
        let word = (u64::from_le_bytes(text[e - 8..e].try_into().unwrap()) ^ 0x3030_3030_3030_3030)
            & u64::MAX << (8 * (8 - len));
        // Every byte must now be 0..=9: adding 0x76 sets the top bit of any above 9.
        return ((word.wrapping_add(0x7676_7676_7676_7676) | word) & 0x8080_8080_8080_8080 == 0)
            .then(|| eight_digits(word));
    }
    parse_digits(text.get(s..e)?)
}

/// Three numbers at once, from the 8 bytes ending at each (the id, the region id,
/// and the amount with its dot, which is squeezed out as in `amount`), and how
/// many digits each has (at most 8). Returns the numbers, and whether every digit
/// was one.
#[inline(always)]
fn numbers(words: [u64; 3], lens: [usize; 3]) -> ([u64; 3], bool) {
    use std::arch::x86_64::*;
    // SAFETY: AVX2 is enabled for this build (see the compile_error).
    unsafe {
        let [a, b, c] = words.map(|w| w as i64);
        let v = _mm256_xor_si256(_mm256_set_epi64x(0, c, b, a), _mm256_set1_epi8(b'0' as i8));
        // Within each 128-bit half: the first two words as they are; the amount's
        // bytes 0..5 one up and its dot (byte 5) dropped.
        let squeeze = _mm256_setr_epi8(
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, //
            -128, 0, 1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
        );
        let v = _mm256_shuffle_epi8(v, squeeze);
        // Keep the top `len` bytes of each word: 8 bytes of a table of 8 zero bytes
        // then 8 of 0xFF, starting at `len`.
        const KEEP: [u8; 16] = [
            0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255,
        ];
        let [x, y, z] = lens.map(|n| u64::from_le_bytes(KEEP[n..n + 8].try_into().unwrap()) as i64);
        let v = _mm256_and_si256(v, _mm256_set_epi64x(0, z, y, x));
        let above_9 = _mm256_subs_epu8(v, _mm256_set1_epi8(9));
        let ok = _mm256_testz_si256(above_9, above_9) == 1;
        // Pairs of digits (x10 + 1), then pairs of those (x100 + 1), packed to
        // 16 bits, then pairs again (x10000 + 1): one number per 64-bit lane.
        let v = _mm256_maddubs_epi16(v, _mm256_set1_epi16(0x010A));
        let v = _mm256_madd_epi16(v, _mm256_set1_epi32(0x0001_0064));
        let v = _mm256_packus_epi32(v, v);
        let v = _mm256_madd_epi16(v, _mm256_set1_epi32(0x0001_2710));
        (
            [
                _mm256_extract_epi32::<0>(v) as u32 as u64,
                _mm256_extract_epi32::<1>(v) as u32 as u64,
                _mm256_extract_epi32::<4>(v) as u32 as u64,
            ],
            ok,
        )
    }
}

/// Eight digit values, most significant in the lowest byte, as a number.
#[inline(always)]
fn eight_digits(x: u64) -> u64 {
    let x = (x * 10 + (x >> 8)) & 0x00FF_00FF_00FF_00FF;
    let x = (x * 100 + (x >> 16)) & 0x0000_FFFF_0000_FFFF;
    (x * 10000 + (x >> 32)) & 0xFFFF_FFFF
}

fn parse_digits(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() || digits.len() > 18 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(
        digits
            .iter()
            .fold(0, |acc, &b| acc * 10 + (b - b'0') as u64),
    )
}

/// `[ "-" ] 1*DIGIT "." 2DIGIT` in `text[s..e]`, before the CR of a CRLF: cents.
#[inline(always)]
fn amount(text: &[u8], s: usize, mut e: usize) -> Option<i64> {
    if e > s && text[e - 1] == b'\r' {
        e -= 1;
    }
    let negative = text.get(s) == Some(&b'-');
    let start = s + negative as usize;
    if e < start + 4 || text[e - 3] != b'.' {
        return None;
    }
    let whole = e - 3 - start;
    let cents = if (1..=5).contains(&whole) && e >= 8 {
        // One SWAR conversion for the whole amount: the 8 bytes ending at the field,
        // minus '0', with the dot (byte 5) squeezed out and the bytes before the
        // field zeroed, read as digits (whole * 100 + cents).
        let word = u64::from_le_bytes(text[e - 8..e].try_into().unwrap()) ^ 0x3030_3030_3030_3030;
        let word = (word & 0x0000_00FF_FFFF_FFFF) << 8 | word & 0xFFFF_0000_0000_0000;
        let word = word & u64::MAX << (8 * (6 - whole));
        // Every byte must now be 0..=9 (see `digits`).
        if (word.wrapping_add(0x7676_7676_7676_7676) | word) & 0x8080_8080_8080_8080 != 0 {
            return None;
        }
        eight_digits(word) as i64
    } else {
        (digits(text, start, e - 3)? * 100 + digits(text, e - 2, e)?) as i64
    };
    Some(if negative { -cents } else { cents })
}

/// Cents as money: sign, integer part, two decimals ("-0.05", "0.00").
fn money(cents: i64) -> String {
    let (sign, abs) = (if cents < 0 { "-" } else { "" }, cents.unsigned_abs());
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// A wyhash-style 64-bit hash of `text[s..e]`: each 8-byte word goes through a
/// full 64x64->128 multiply, folded, so every input bit reaches every output bit.
/// The last (up to) 16 bytes are two words without a branch on their length: the
/// first masked to the field, the second ending at `e` (or 0 for 8 bytes or fewer).
#[inline(always)]
fn hash(text: &[u8], s: usize, e: usize) -> u64 {
    assert!(8 <= e && s <= e);
    if e + 8 > text.len() {
        return padded_hash(&text[s..e]);
    }
    // SAFETY: just checked.
    unsafe { hash_words(text, s, e) }
}

/// `hash`, for a field with 8 bytes of text before its end and 8 after it.
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
    let first = unsafe { std::arch::x86_64::_bzhi_u64(load(at), 8 * rest as u32) };
    let last = load(e - 8) & ((rest > 8) as u64).wrapping_neg();
    mix(h ^ first ^ P1, last ^ P0 ^ len)
}

/// `hash` of a field at the very end of the file, from a padded copy so that
/// its loads stay in bounds.
#[cold]
fn padded_hash(field: &[u8]) -> u64 {
    let mut copy = vec![0; 8];
    copy.extend_from_slice(field);
    copy.extend_from_slice(&[0; 8]);
    hash(&copy, 8, 8 + field.len())
}
