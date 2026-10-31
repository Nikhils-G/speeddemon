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
//!    check is a predictable branch to a byte-by-byte parser of spec 2.1, which
//!    handles any other record (and rejects invalid ones).
//! 4. Distinct names per country: a name's identity is its raw inner bytes (inside
//!    quotes, `""` maps one to one to the unescaped value, and an unquoted value
//!    cannot contain `"`), reduced to a 56-bit hash; with the spec's 100,000 names
//!    the chance that two share one is about 7e-8. Parsing only files a (hash,
//!    group) pair under one of 4 partitions per thread picked by the hash. Between
//!    parsing rounds, each thread marks the pairs of the partitions it owns (the
//!    same ones every round) in their small tables of names with a bit per group,
//!    which stay in its cache. No two threads ever write the same memory, so there
//!    are no atomics or cache lines bouncing between cores, and the rounds keep the
//!    pair buffers small.

mod mmap;

#[cfg(not(all(target_feature = "avx2", target_feature = "pclmulqdq")))]
compile_error!("track-c needs AVX2 and PCLMULQDQ: build with RUSTFLAGS=\"-C target-cpu=native\"");

use std::fmt::Write as _;
use std::fs::File;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::*};
use std::sync::{Mutex, RwLock};
use std::thread;

const RECORDS_HEADER: &[u8] = b"id,name,note,region_id,amount";
const REGIONS_HEADER: &[u8] = b"region_id,country\n";
const BOM: &[u8] = b"\xEF\xBB\xBF";
/// The file is parsed in this many rounds of one run per thread, so the pair
/// buffers only ever hold one round's pairs (about 3 MB in all).
const ROUNDS: usize = 8;
/// Names are split by their top hash bits into this many partitions per thread;
/// each thread owns the same ones in every round, so their tables stay in its cache.
const PARTITIONS_PER_THREAD: usize = 4;
/// Group indices are bytes, so that tables indexed by a group need no bounds
/// checks: countries in the order they are first seen, `_UNMATCHED` the last one.
const GROUPS: usize = 256;
const UNMATCHED_GROUP: usize = GROUPS - 1;
/// Words of group bits per name.
const WORDS: usize = GROUPS / 64;
/// regions.csv is entered in this many parts.
const REGION_CHUNKS: usize = 64;
/// Country codes are `[A-Z]{3}` (spec 2.3), so each has an index below 26^3.
const CODES: usize = 26 * 26 * 26;

/// A group's columns: count, id_sum, amount_sum, amount_min, amount_max,
/// names_distinct, and name_bytes << 32 | note_bytes (amounts in cents; every
/// byte column is below 2^31, spec section 3).
type Totals = [i64; 7];
const EMPTY: Totals = [0, 0, 0, i64::MAX, i64::MIN, 0, 0];
const NAMES_DISTINCT: usize = 5;

fn main() {
    mmap::no_huge_pages();
    mmap::big_heap_steps();
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    let Err(message) = run(&data_dir, &out_dir);
    exit_with(Err(message))
}

fn exit_with(result: Result<(), String>) -> ! {
    if let Err(message) = &result {
        eprintln!("track-c: {message}");
    }
    // Exit without dropping the tables, one free at a time.
    std::process::exit(result.is_err() as i32)
}

/// Returns only on failure: on success the thread that writes the output exits
/// the process.
fn run(data_dir: &str, out_dir: &str) -> Result<std::convert::Infallible, String> {
    let open = |name: &str| -> Result<&'static [u8], String> {
        let path = format!("{data_dir}/{name}");
        let file = File::open(&path).map_err(|e| format!("open {path}: {e}"))?;
        mmap::map(&file).map_err(|e| format!("mmap {path}: {e}"))
    };
    let lines = open("regions.csv")?
        .strip_prefix(REGIONS_HEADER)
        .ok_or("unexpected regions.csv header")?;
    let records = open("records.csv")?;
    let body_start = skip_header(records).ok_or("unexpected records.csv header")?;
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    // regions.csv lists its ids in ascending order (spec 2.3), so the last line
    // has the largest. One entry more, for every id above it.
    let last_line = lines.trim_ascii_end().rsplit(|&b| b == b'\n').next();
    let ids = (digits(last_line.unwrap_or_default(), &mut 0).0 as usize).min(1 << 30) + 2;
    // Slots for the spec's 100,000 names at most 5/8 full, a power of two.
    let first_slots = (160_000 / (threads * PARTITIONS_PER_THREAD)).next_power_of_two();
    let (mut guessed, mut slots) = (true, first_slots.max(4));
    loop {
        match aggregate(
            records, body_start, guessed, threads, lines, slots, ids, out_dir,
        ) {
            ParseError::NamesFull => slots *= 2,
            // A wrong guessed cut shows up as a misaligned run or as a parse error.
            // Fall back to one run over the whole file: exact under spec 2.1.
            ParseError::Misaligned | ParseError::Bad(_) if guessed => guessed = false,
            ParseError::Misaligned => return Err("a record crosses a run boundary".into()),
            ParseError::Bad(message) => return Err(message),
        }
    }
}

/// Writes the output from each thread's totals and each country code's group + 1
/// (0: not in regions.csv).
fn write_output(
    out_dir: &str,
    parts: &[[Totals; GROUPS]],
    codes: &[AtomicU8],
) -> Result<(), String> {
    // Keys sorted bytewise: the codes, then `_UNMATCHED` ("_" > "Z").
    let mut keys: Vec<(String, usize)> = (0..CODES)
        .filter(|&code| codes[code].load(Relaxed) != 0)
        .map(|code| {
            let letters = [code / 676, code / 26 % 26, code % 26].map(|c| (b'A' + c as u8) as char);
            let group = codes[code].load(Relaxed) as usize - 1;
            (letters.iter().collect(), group)
        })
        .collect();
    keys.push(("_UNMATCHED".into(), UNMATCHED_GROUP));
    let mut out = String::new();
    for (key, group) in keys {
        let mut t = EMPTY;
        for part in parts {
            for (k, v) in part[group].iter().enumerate() {
                t[k] = match k {
                    3 => t[k].min(*v),
                    4 => t[k].max(*v),
                    _ => t[k] + v,
                };
            }
        }
        let [count, ids, sum, min, max, names, bytes] = t;
        if count > 0 {
            let (sum, min, max) = (money(sum), money(min), money(max));
            let (name_bytes, note_bytes) = (bytes >> 32, bytes & 0xFFFF_FFFF);
            _ = writeln!(
                out,
                "{key} {count} {ids} {sum} {min} {max} {names} {name_bytes} {note_bytes}"
            );
        }
    }
    let output_path = format!("{out_dir}/aggregates.txt");
    std::fs::write(&output_path, out).map_err(|e| format!("write {output_path}: {e}"))
}

/// Builds the join table from regions.csv, then parses and aggregates every run,
/// one round of `threads` runs at a time, and writes the output. One set of
/// threads does all of it, this one included. Returns only if this attempt failed.
fn aggregate(
    text: &[u8],
    body_start: usize,
    guessed: bool,
    threads: usize,
    lines: &[u8],
    slots: usize,
    ids: usize,
    out_dir: &str,
) -> ParseError {
    let regions = Regions {
        // SAFETY: zero is a valid AtomicU8. Zeroed by calloc, so the pages fault
        // in on the threads that fill them.
        group_of: unsafe { Box::new_zeroed_slice(ids).assume_init() },
        codes: (0..CODES).map(|_| AtomicU8::new(0)).collect(),
        numbered: Mutex::new(0),
    };
    // regions.csv is entered in chunks that threads claim as they start, so the
    // ones that start first do more of it.
    let (next_chunk, chunks_done) = (AtomicUsize::new(0), AtomicUsize::new(0));
    // Pairs per thread per partition in one run, with room to spare (records are
    // about 60 bytes), so the buffers are allocated once, by their own thread.
    let units = threads * ROUNDS;
    let parts = threads * PARTITIONS_PER_THREAD;
    let capacity = text.len() / (units * 60 * parts) * 5 / 4 + 16;
    let pairs: Vec<RwLock<Vec<Vec<u64>>>> = (0..threads).map(|_| RwLock::new(Vec::new())).collect();
    let (arrived, generation) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let barrier = SpinBarrier {
        threads,
        arrived,
        generation,
    };
    let finished: Vec<Mutex<Option<[Totals; GROUPS]>>> =
        (0..threads).map(|_| Mutex::new(None)).collect();
    let errors = Mutex::new(Vec::new());
    let worker = &|w: usize| {
        let mut totals = [EMPTY; GROUPS];
        // Allocated here: buffer lengths next to another thread's would share
        // cache lines with it (+1 ms).
        *pairs[w].write().unwrap() = (0..parts).map(|_| Vec::with_capacity(capacity)).collect();
        // This thread's partitions, p = w + k * threads; made here, so they are in its cache.
        let mut partitions: Vec<_> = (0..PARTITIONS_PER_THREAD)
            .map(|_| Partition::new(slots))
            .collect();
        let mut error = None;
        loop {
            let chunk = next_chunk.fetch_add(1, Relaxed);
            if chunk >= REGION_CHUNKS {
                break;
            }
            if let Err(e) = regions.fill(lines, chunk, REGION_CHUNKS) {
                error = Some(ParseError::Bad(e));
            }
            chunks_done.fetch_add(1, Release);
        }
        spin_until(|| chunks_done.load(Acquire) == REGION_CHUNKS);
        for round in 0..ROUNDS {
            let run = round * threads + w;
            if error.is_none() {
                let mut mine = pairs[w].write().unwrap();
                mine.iter_mut().for_each(Vec::clear);
                // Each thread finds its own cuts, so their pages fault in in parallel.
                let start = run_start(text, body_start, run, units, guessed);
                let end = run_start(text, body_start, run + 1, units, guessed);
                match parse_run(text, start, end, &regions.group_of, &mut totals, &mut mine) {
                    // Drop the run's pages now, in parallel, instead of the kernel
                    // unmapping all 50,000 at exit on one core.
                    Ok(last) if last == end => mmap::release(&text[start..end]),
                    Ok(_) => error = Some(ParseError::Misaligned),
                    Err(e) => error = Some(e),
                }
            }
            barrier.wait();
            for (k, partition) in partitions.iter_mut().enumerate() {
                for buffers in &pairs {
                    let mine = &buffers.read().unwrap()[w + k * threads];
                    if let (None, Err(e)) = (&error, partition.add(mine, &mut totals)) {
                        error = Some(e);
                    }
                }
            }
            if round + 1 == ROUNDS && error.is_none() {
                *finished[w].lock().unwrap() = Some(totals);
            }
            barrier.wait();
        }
        // The first thread writes the output and exits, without waiting for the
        // others to exit and without dropping anything.
        if w == 0
            && let Some(parts) = finished
                .iter()
                .map(|t| *t.lock().unwrap())
                .collect::<Option<Vec<_>>>()
        {
            exit_with(write_output(out_dir, &parts, &regions.codes));
        }
        errors.lock().unwrap().extend(error);
    };
    thread::scope(|s| {
        for w in 1..threads {
            s.spawn(move || worker(w));
        }
        worker(0);
    });
    // A bad cut can also make the table overflow, so the cut errors come first.
    let errors = errors.into_inner().unwrap().into_iter();
    errors
        .min_by_key(|e| matches!(e, ParseError::NamesFull))
        .unwrap()
}

/// Spins for a while, then yields the core between checks, until `done()`. The
/// phases between checks last a millisecond or two, and waking a dozen sleeping
/// threads takes a good part of that (std's Barrier: about 1 ms more per run).
/// Yielding after the spin keeps a thread that lost its core from being starved.
fn spin_until(done: impl Fn() -> bool) {
    for spin in 0u32.. {
        if done() {
            return;
        }
        if spin < 1 << 14 {
            std::hint::spin_loop();
        } else {
            thread::yield_now();
        }
    }
}

/// A barrier that waits with `spin_until`.
struct SpinBarrier {
    threads: usize,
    arrived: AtomicUsize,
    generation: AtomicUsize,
}

impl SpinBarrier {
    fn wait(&self) {
        let generation = self.generation.load(Acquire);
        if self.arrived.fetch_add(1, AcqRel) + 1 == self.threads {
            self.arrived.store(0, Relaxed);
            self.generation.fetch_add(1, Release);
        } else {
            spin_until(|| self.generation.load(Acquire) != generation);
        }
    }
}

/// One partition's names: an open-addressing table of name hashes in buckets of
/// 4, at most 5/8 full so that a lookup is nearly always one bucket, with a dense
/// id per name, and one row of bits per id, a bit per group. (Rows by slot instead,
/// in a 7/8 full table: 23% less memory, but 13% of lookups leave their first
/// bucket, +0.8M branch misses.)
struct Partition {
    keys: Vec<u64>, // 0 = empty, else the name hash with its low byte set to 0xFF
    ids: Vec<u16>,  // by slot
    seen: Vec<u64>, // WORDS per id
    used: usize,
}

impl Partition {
    /// A table of `slots` slots (a power of two), for up to 5/8 as many names.
    fn new(slots: usize) -> Self {
        Partition {
            keys: vec![0; slots],
            ids: vec![0; slots],
            seen: vec![0; slots / 8 * 5 * WORDS],
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
        let (buckets, full) = (keys.len() / 4, keys.len() / 8 * 5);
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
                totals[group][NAMES_DISTINCT] += (*word & bit == 0) as i64;
                *word |= bit;
            }
        }
        Ok(())
    }
}

/// The join table: region id -> group.
struct Regions {
    /// Dense by region id: group + 1, or 0 for ids the table does not contain.
    group_of: Box<[AtomicU8]>,
    /// By country code index: its group + 1, or 0 while not seen.
    codes: Vec<AtomicU8>,
    /// Held while a code gets the next group number.
    numbered: Mutex<u8>,
}

impl Regions {
    /// Enters the `region_id,CCC` LF lines that start in the `k`th of `parts`
    /// equal parts of `lines`.
    fn fill(&self, lines: &[u8], k: usize, parts: usize) -> Result<(), String> {
        let cut = |k: usize| match lines.len() * k / parts {
            0 => 0,
            p => {
                p + lines[p - 1..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .unwrap_or(lines.len() - p)
            }
        };
        let (mut at, end) = (cut(k), cut(k + 1));
        while at < end {
            let (region, n) = digits(lines, &mut at);
            let slot = self.group_of.get(region as usize).filter(|_| n > 0);
            let (Some(slot), Some(&[b',', a, b, c, b'\n'])) = (slot, lines.get(at..at + 5)) else {
                return Err(format!("regions.csv: bad line at byte {at}"));
            };
            if ![a, b, c].iter().all(u8::is_ascii_uppercase) {
                return Err(format!("regions.csv: bad country at byte {at}"));
            }
            at += 5;
            let code = &self.codes
                [(a - b'A') as usize * 676 + (b - b'A') as usize * 26 + (c - b'A') as usize];
            if code.load(Relaxed) == 0 {
                let mut numbered = self.numbered.lock().unwrap();
                if code.load(Relaxed) == 0 {
                    *numbered = numbered
                        .checked_add(1)
                        .ok_or("regions.csv: too many countries")?;
                    code.store(*numbered, Relaxed);
                }
            }
            slot.store(code.load(Relaxed), Relaxed);
        }
        Ok(())
    }
}

enum ParseError {
    /// A partition's name table passed 5/8 full; retry with bigger ones.
    NamesFull,
    /// A run did not end where the next one starts (only possible for a guessed cut).
    Misaligned,
    Bad(String),
}

/// The offset of the first data record, after the BOM and header.
fn skip_header(text: &[u8]) -> Option<usize> {
    let rest = text
        .strip_prefix(BOM)
        .unwrap_or(text)
        .strip_prefix(RECORDS_HEADER)?;
    let body = match rest {
        [b'\r', b'\n', body @ ..] | [b'\n', body @ ..] => body,
        [] => rest,
        _ => return None,
    };
    Some(text.len() - body.len())
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
/// record ended, which is `end` when `start` and `end` are record starts. (It
/// takes the table, not `&Regions`, whose Mutex would make every record reload
/// the table's address.)
fn parse_run(
    text: &[u8],
    start: usize,
    end: usize,
    group_of: &[AtomicU8],
    totals: &mut [Totals; GROUPS],
    pairs: &mut [Vec<u64>],
) -> Result<usize, ParseError> {
    let mut i = start;
    // The header comes first, so a record start is at least 8 bytes in.
    let fast_end = end.min(text.len().saturating_sub(256));
    while i < end {
        // A non-temporal prefetch ~16 records ahead: the stream then takes less of
        // L2, so more of the 1 MB region table stays there.
        // SAFETY: a prefetch never faults, and wrapping_add keeps the address defined.
        unsafe {
            use std::arch::x86_64::*;
            _mm_prefetch::<_MM_HINT_NTA>(text.as_ptr().wrapping_add(i + 1024).cast());
        }
        // SAFETY: the record starts at least 8 bytes in, and 256 before the end.
        let fast = if i < fast_end {
            unsafe { fast_record(text, i) }
        } else {
            None
        };
        let r = match fast {
            Some(r) => r,
            None => slow_record(text, i)
                .ok_or_else(|| ParseError::Bad(format!("records.csv: bad record at byte {i}")))?,
        };
        // The stored group + 1, so 0 (not in the table) wraps to UNMATCHED_GROUP.
        let k = (r.region_id as usize).min(group_of.len() - 1);
        // SAFETY: k is below the length (which is at least 2).
        let group = unsafe { group_of.get_unchecked(k) }
            .load(Relaxed)
            .wrapping_sub(1);
        let t = &mut totals[group as usize];
        t[0] += 1;
        t[1] += r.id as i64;
        t[2] += r.cents;
        // Branches, not cmov and a store: a new min or max is rare.
        if r.cents < t[3] {
            t[3] = r.cents;
        }
        if r.cents > t[4] {
            t[4] = r.cents;
        }
        t[6] += r.bytes as i64;
        // The partition from the top hash bits, by multiply-shift: any count.
        let p = ((r.hash >> 32) * pairs.len() as u64 >> 32) as usize;
        pairs[p].push(r.hash & !0xFF | group as u64);
        i = r.next;
    }
    Ok(i.min(text.len()))
}

/// One parsed record: its values, its name's hash, and where the next one starts.
struct Record {
    id: u64,
    region_id: u64,
    cents: i64,
    bytes: u64, // name_bytes << 32 | note_bytes
    hash: u64,
    next: usize,
}

/// The common record, `id,"name","note",region_id,amount` and a terminator,
/// with no unquoted text: its separators from the bitmaps of the 64 bytes at `i`
/// (rarely 128) and of 32 bytes after the note, with every check folded into one
/// flag. None sends the record to `slow_record`, which handles every case.
///
/// SAFETY: `i - 8..i + 256` must be inside the text.
#[inline(always)]
unsafe fn fast_record(text: &[u8], i: usize) -> Option<Record> {
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
        bytes: (name_bytes << 32
            | (note_end - name_end - 1 - note_quoted as usize - note_quotes / 2))
            as u64,
        hash,
        next: i + amount_end + 1 + crlf as usize,
    })
}

/// Any record starting at `i`, parsed byte by byte as spec 2.1 defines it; None
/// if it is invalid. Kept out of line, so that it does not crowd the registers of
/// the fast path's loop.
#[cold]
#[inline(never)]
fn slow_record(text: &[u8], i: usize) -> Option<Record> {
    let mut at = i + if text[i..].starts_with(BOM) {
        BOM.len()
    } else {
        0
    }; // not data
    let expect = |at: &mut usize, byte: u8| (text.get(*at) == Some(&byte)).then(|| *at += 1);
    let (id, n) = digits(text, &mut at);
    expect(&mut at, b',').filter(|_| n > 0)?;
    let (name_start, name_end, name_bytes) = text_field(text, &mut at)?;
    expect(&mut at, b',')?;
    let (_, _, note_bytes) = text_field(text, &mut at)?;
    expect(&mut at, b',')?;
    let (region_id, n) = digits(text, &mut at);
    expect(&mut at, b',').filter(|_| n > 0)?;
    let negative = expect(&mut at, b'-').is_some();
    let (whole, n) = digits(text, &mut at);
    expect(&mut at, b'.').filter(|_| n > 0)?;
    let (fraction, n) = digits(text, &mut at);
    let next = match &text[at..] {
        _ if n != 2 => return None,
        [] => at,
        [b'\n', ..] => at + 1,
        [b'\r', b'\n', ..] => at + 2,
        [b'\r'] => at + 1, // a CR at the very end, as before and as the reference reads it
        _ => return None,
    };
    let cents = whole.wrapping_mul(100).wrapping_add(fraction) as i64;
    Some(Record {
        id,
        region_id,
        cents: if negative { -cents } else { cents },
        bytes: name_bytes << 32 | note_bytes,
        hash: hash(text, name_start, name_end),
        next,
    })
}

/// The digits at `*at`, which it moves past: their value and their count.
fn digits(text: &[u8], at: &mut usize) -> (u64, usize) {
    let (start, mut value) = (*at, 0u64);
    while let Some(d) = text
        .get(*at)
        .map(|b| b.wrapping_sub(b'0'))
        .filter(|&d| d < 10)
    {
        value = value.wrapping_mul(10).wrapping_add(d as u64);
        *at += 1;
    }
    (value, *at - start)
}

/// The name or note at `*at`, which it moves past: the range of its raw value
/// (inside the quotes, if quoted) and its unescaped length.
fn text_field(text: &[u8], at: &mut usize) -> Option<(usize, usize, u64)> {
    let s = *at;
    if text.get(s) != Some(&b'"') {
        let rest = &text[s..];
        let n = rest
            .iter()
            .position(|&b| matches!(b, b'"' | b',' | b'\r' | b'\n'));
        *at += n.unwrap_or(rest.len());
        return Some((s, *at, (*at - s) as u64));
    }
    let (mut e, mut escapes) = (s + 1, 0);
    loop {
        e += text.get(e..)?.iter().position(|&b| b == b'"')?;
        if text.get(e + 1) != Some(&b'"') {
            break;
        }
        (e, escapes) = (e + 2, escapes + 1);
    }
    *at = e + 1;
    Some((s + 1, e, (e - s - 1 - escapes) as u64))
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
