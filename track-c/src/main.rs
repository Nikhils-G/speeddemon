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
//!    group) pair under one of 4 partitions per thread picked by the hash. After
//!    each run it parses, a thread hands its buffers of pairs to the partitions'
//!    inboxes, and marks the pairs waiting for the partitions it owns in their
//!    small tables of names with a bit per group, which stay in its cache.
//! 5. No thread waits for a given other one until the end: runs are claimed one at
//!    a time, so a thread slowed down (another process sharing its core) parses
//!    fewer of them, and its partitions' pairs wait in their inboxes. (With a
//!    barrier after each round, one core shared with a busy loop made the run
//!    take twice as long.)

mod mmap;

#[cfg(not(all(target_feature = "avx2", target_feature = "pclmulqdq")))]
compile_error!("track-c needs AVX2 and PCLMULQDQ: build with RUSTFLAGS=\"-C target-cpu=native\"");

use std::arch::x86_64::*;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::*};
use std::thread;

const BOM: &[u8] = b"\xEF\xBB\xBF";
/// Each thread parses this many runs, on average: they are claimed as threads go,
/// so a thread slowed down (another process on its core) parses fewer of them.
const RUNS_PER_THREAD: usize = 32;
/// Runs are dropped from the mapping in blocks of this many.
const BLOCK: usize = 4;
/// Names are split by their top hash bits into this many partitions per thread;
/// each thread normally takes the same ones' pairs, so their tables stay in its cache.
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

fn main() {
    mmap::setup();
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

/// Builds the join table from regions.csv, then parses and aggregates every run,
/// and writes the output. One set of threads does all of it, this one included. Returns only on failure: on success
/// the thread that writes the output exits the process.
fn run(data_dir: &str, out_dir: &str) -> Result<std::convert::Infallible, String> {
    let open = |name: &str| -> Result<&'static [u8], String> {
        let path = format!("{data_dir}/{name}");
        let file = std::fs::File::open(&path).map_err(|e| format!("open {path}: {e}"))?;
        mmap::map(&file).map_err(|e| format!("mmap {path}: {e}"))
    };
    let lines = open("regions.csv")?;
    let lines = lines
        .strip_prefix(b"region_id,country\n")
        .ok_or("bad regions.csv header")?;
    let text = open("records.csv")?;
    // The header is the first record (after a BOM); everything after it is data.
    let header = text.strip_prefix(BOM).unwrap_or(text);
    let header = header.strip_prefix(b"id,name,note,region_id,amount".as_slice());
    let body = header.map(|b| b.strip_prefix(b"\r").unwrap_or(b));
    let body = body
        .and_then(|b| b.strip_prefix(b"\n"))
        .ok_or("bad records.csv header")?;
    let body_start = text.len() - body.len();
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    // regions.csv lists its ids in ascending order (spec 2.3), so the last line
    // has the largest. One entry more, for every id above it.
    let last_line = lines.trim_ascii_end().rsplit(|&b| b == b'\n').next();
    let ids = (digits(last_line.unwrap_or_default(), &mut 0).0 as usize).min(1 << 30) + 2;
    // The generator's pool has 100,000 names (and 4 more per block of edge cases).
    // A partition's table may fill to 5/8: room for its share, plus 8 standard
    // deviations, a power of two. More names than that is an error.
    let parts = threads * PARTITIONS_PER_THREAD;
    let names = 101_000 / parts;
    let slots = ((names + 8 * names.isqrt()) * 8 / 5).next_power_of_two();
    // Pairs per partition in one run, with room to spare (records are about 60
    // bytes), so the buffers are allocated once, by their own thread.
    let runs = threads * RUNS_PER_THREAD;
    let capacity = text.len() / (runs * 60 * parts) * 5 / 4 + 16;
    // A wrong guessed cut (impossible under spec 2.2) shows up as a misaligned run
    // or a bad record. Then the file is parsed again as one run: exact under 2.1.
    for guessed in [true, false] {
        let regions = Regions::new(ids);
        let (next_chunk, chunks_done) = (AtomicUsize::new(0), AtomicUsize::new(0));
        // Each thread's tables (its partitions, w * 4 .. w * 4 + 4), and its inbox:
        // buffers of pairs filed under them that they have not taken yet, 4 at a time
        // (one per partition, in order), and empty ones to file more in. Buffers are
        // handed over, not copied (copying them: +1.2 ms).
        let tables: Vec<Mutex<Vec<Partition>>> = (0..threads).map(|_| Default::default()).collect();
        type Inbox = Mutex<(Vec<Vec<u64>>, Vec<Vec<u64>>)>;
        let inboxes: Vec<Inbox> = (0..threads).map(|_| Default::default()).collect();
        // Per block of runs, how many are parsed; the threads done parsing.
        let parsed: Vec<_> = (0..runs / BLOCK).map(|_| AtomicUsize::new(0)).collect();
        let (next_run, flushed) = (AtomicUsize::new(0), AtomicUsize::new(0));
        // Each thread's totals, or the reason it stopped.
        let finished = Mutex::new(Vec::new());
        let worker = &|w: usize| {
            let mut totals = [EMPTY; GROUPS];
            // This thread's tables and pair buffers: made here, so they are in its cache.
            *tables[w].lock().unwrap() = (0..PARTITIONS_PER_THREAD)
                .map(|_| Partition::new(slots))
                .collect();
            let mut pairs: Vec<_> = (0..parts).map(|_| Vec::with_capacity(capacity)).collect();
            // Thread t's tables take the pairs in its inbox, and give the buffers back
            // empty.
            let mut taken = Vec::new();
            let mut take = |tables: &mut Vec<Partition>, t: usize, totals: &mut _| {
                std::mem::swap(&mut taken, &mut inboxes[t].lock().unwrap().0);
                let mut result = Ok(());
                for (k, pairs) in taken.iter_mut().enumerate() {
                    result = result.and(tables[k % PARTITIONS_PER_THREAD].add(pairs, totals));
                    pairs.clear();
                }
                inboxes[t].lock().unwrap().1.append(&mut taken);
                result
            };
            let mut error = Ok(());
            while let Some(chunk) =
                Some(next_chunk.fetch_add(1, Relaxed)).filter(|&c| c < REGION_CHUNKS)
            {
                error = error.and(regions.fill(lines, chunk));
                chunks_done.fetch_add(1, Release);
            }
            spin_until(|| chunks_done.load(Acquire) == REGION_CHUNKS);
            // Runs are claimed one at a time, so a thread slowed down parses fewer of
            // them, and no thread waits for another one until the end.
            while let Some(run) = Some(next_run.fetch_add(1, Relaxed)).filter(|&r| r < runs) {
                // Each thread finds its own cuts, so their pages fault in in parallel.
                let cut = |run| run_start(text, body_start, run, runs, guessed);
                if error.is_ok() {
                    let (start, end, group_of) = (cut(run), cut(run + 1), &regions.group_of);
                    error = parse_run(text, start, end, group_of, &mut totals, &mut pairs);
                }
                for (inbox, pairs) in inboxes.iter().zip(pairs.chunks_mut(PARTITIONS_PER_THREAD)) {
                    let (full, empty) = &mut *inbox.lock().unwrap();
                    for pairs in pairs {
                        let fresh = empty.pop().unwrap_or_else(|| Vec::with_capacity(capacity));
                        full.push(std::mem::replace(pairs, fresh));
                    }
                }
                // The last of a block's runs to be parsed drops the block's pages, in
                // parallel, instead of the kernel unmapping all 50,000 at exit on one
                // core. (A call per run: +2 ms, from 4 times the TLB shootdowns.)
                let block = run / BLOCK;
                if parsed[block].fetch_add(1, AcqRel) == BLOCK - 1 {
                    mmap::release(&text[cut(block * BLOCK)..cut(block * BLOCK + BLOCK)]);
                }
                // This thread's tables, unless another thread holds them.
                if let Ok(mut mine) = tables[w].try_lock() {
                    error = error.and(take(&mut mine, w, &mut totals));
                }
            }
            // Tables take their inboxes, this thread's first, while other threads
            // still parse, and once more after the last one is done.
            flushed.fetch_add(1, Release);
            while flushed.load(Acquire) < threads {
                for t in std::iter::once(w).chain(0..threads) {
                    // (No tables yet: their thread has not made them.)
                    if let Ok(mut theirs) = tables[t].try_lock()
                        && !theirs.is_empty()
                    {
                        error = error.and(take(&mut theirs, t, &mut totals));
                    }
                }
            }
            for t in std::iter::once(w).chain(0..threads) {
                error = error.and(take(&mut tables[t].lock().unwrap(), t, &mut totals));
            }
            // The last thread to finish writes the output and exits, without waiting
            // for the others to exit and without dropping anything.
            let mut finished = finished.lock().unwrap();
            finished.push(error.map(|_| totals));
            if finished.len() == threads {
                let parts: Result<Vec<_>, _> = finished.drain(..).collect();
                if parts.is_ok() || !guessed {
                    exit_with(parts.and_then(|parts| write_output(out_dir, &parts, &regions)));
                }
            }
        };
        thread::scope(|s| {
            for w in 1..threads {
                s.spawn(move || worker(w));
            }
            worker(0);
        });
    }
    unreachable!("the last worker exits the process")
}

/// Writes each group's line, summing the threads' totals.
fn write_output(dir: &str, parts: &[[Totals; GROUPS]], regions: &Regions) -> Result<(), String> {
    // Keys sorted bytewise: the codes, then `_UNMATCHED` ("_" > "Z").
    let codes = regions.keys.lock().unwrap();
    let mut keys: Vec<_> = codes.iter().map(|c| c.to_vec()).zip(0..).collect();
    keys.sort();
    keys.push((b"_UNMATCHED".to_vec(), GROUPS - 1));
    let mut out = String::new();
    for (key, group) in keys {
        let [count, ids, sum, min, max, names, bytes] = parts.iter().fold(EMPTY, |t, p| {
            // The columns add up, except the min and the max.
            let mut sums: Totals = std::array::from_fn(|k| t[k] + p[group][k]);
            (sums[3], sums[4]) = (t[3].min(p[group][3]), t[4].max(p[group][4]));
            sums
        });
        if count > 0 {
            let key = String::from_utf8_lossy(&key);
            let (sum, min, max) = (money(sum), money(min), money(max));
            let (name_bytes, note_bytes) = (bytes >> 32, bytes & 0xFFFF_FFFF);
            _ = writeln!(
                out,
                "{key} {count} {ids} {sum} {min} {max} {names} {name_bytes} {note_bytes}"
            );
        }
    }
    let output_path = format!("{dir}/aggregates.txt");
    std::fs::write(&output_path, out).map_err(|e| format!("write {output_path}: {e}"))
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
        let seen = vec![0; slots / 8 * 5 * WORDS];
        Partition {
            keys: vec![0; slots],
            ids: vec![0; slots],
            seen,
            used: 0,
        }
    }

    /// Marks each (name hash, group) pair, and counts in `totals` the pairs seen
    /// for the first time.
    fn add(&mut self, pairs: &[u64], totals: &mut [Totals; GROUPS]) -> Result<(), String> {
        // Fields as locals: no store through them can change the others.
        let (keys, ids) = (&mut self.keys, &mut self.ids);
        let (seen, used) = (&mut self.seen, &mut self.used);
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
                            return Err("more distinct names than the tables were sized for".into());
                        }
                        let slot = bucket * 4 + free.trailing_zeros() as usize;
                        *keys.get_unchecked_mut(slot) = key;
                        *ids.get_unchecked_mut(slot) = *used as u16;
                        *used += 1;
                        break slot;
                    }
                    bucket = (bucket + 1) & (buckets - 1);
                };
                let id = *ids.get_unchecked(slot) as usize;
                let word = seen.get_unchecked_mut(id * WORDS + group / 64);
                let bit = 1 << (group % 64);
                totals[group][5] += (*word & bit == 0) as i64; // names_distinct
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
    /// By country code index (26^3 of them): its group + 1, or 0 while not seen.
    codes: Vec<AtomicU8>,
    /// The codes in group order; held while a code gets the next group number.
    keys: Mutex<Vec<[u8; 3]>>,
}

impl Regions {
    /// An empty table for region ids below `ids`.
    fn new(ids: usize) -> Self {
        // SAFETY: zero is a valid AtomicU8. Zeroed by calloc, so the pages fault
        // in on the threads that fill them.
        let group_of = unsafe { Box::new_zeroed_slice(ids).assume_init() };
        let codes = (0..26 * 26 * 26).map(|_| AtomicU8::new(0)).collect();
        Regions {
            group_of,
            codes,
            keys: Mutex::new(Vec::new()),
        }
    }

    /// Enters the `region_id,CCC` LF lines that start in the `k`th of
    /// `REGION_CHUNKS` equal parts of `lines`.
    fn fill(&self, lines: &[u8], k: usize) -> Result<(), String> {
        // Every LF in regions.csv is followed by a digit or the end, so `run_start`
        // cuts it into whole lines.
        let mut at = run_start(lines, 0, k, REGION_CHUNKS, true);
        let end = run_start(lines, 0, k + 1, REGION_CHUNKS, true);
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
            let code = (a - b'A') as usize * 676 + (b - b'A') as usize * 26 + (c - b'A') as usize;
            let group = &self.codes[code];
            if group.load(Relaxed) == 0 {
                let mut keys = self.keys.lock().unwrap();
                if group.load(Relaxed) == 0 {
                    keys.push([a, b, c]);
                    // Group numbers 1..=255; the last group, 256, is `_UNMATCHED`.
                    let number =
                        u8::try_from(keys.len()).map_err(|_| "regions.csv: too many countries")?;
                    group.store(number, Relaxed);
                }
            }
            slot.store(group.load(Relaxed), Relaxed);
        }
        Ok(())
    }
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
    while i + 1 < text.len() && !(text[i] == b'\n' && matches!(text[i + 1], b'0'..=b'9' | 0xEF)) {
        i += 1;
    }
    (i + 1).min(text.len())
}

/// Parses the records in `text[start..end]` into `totals`, and files each
/// record's (name hash, group) pair under its partition. The last record must end
/// at `end`. (It takes the table, not `&Regions`, whose Mutex would make every
/// record reload the table's address.) Out of line: inlined into the worker, its
/// registers were allocated with the worker's, and it ran 2-4% slower.
#[inline(never)]
fn parse_run(
    text: &[u8],
    start: usize,
    end: usize,
    group_of: &[AtomicU8],
    totals: &mut [Totals; GROUPS],
    pairs: &mut [Vec<u64>],
) -> Result<(), String> {
    let mut i = start;
    // The header comes first, so a record start is at least 8 bytes in.
    let fast_end = end.min(text.len().saturating_sub(256));
    while i < end {
        // A non-temporal prefetch ~16 records ahead: the stream then takes less of
        // L2, so more of the 1 MB region table stays there.
        // SAFETY: a prefetch never faults, and wrapping_add keeps the address defined.
        unsafe { _mm_prefetch::<_MM_HINT_NTA>(text.as_ptr().wrapping_add(i + 1024).cast()) };
        let r = match fast_record(text, i, fast_end) {
            Some(r) => r,
            None => slow_record(text, i)
                .ok_or_else(|| format!("records.csv: bad record at byte {i}"))?,
        };
        let Record(id, region_id, cents, bytes, hash, next) = r;
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
        i = next;
    }
    match i.min(text.len()) == end {
        true => Ok(()),
        false => Err("records.csv: a record crosses a run boundary".into()),
    }
}

/// One parsed record: id, region_id, amount in cents, name_bytes << 32 |
/// note_bytes, the name's hash, and where the next record starts.
struct Record(u64, u64, i64, u64, u64, usize);

/// The common record, `id,"name","note",region_id,amount` and a terminator,
/// with no unquoted text: its separators from the bitmaps of the 64 bytes at `i`
/// (rarely 128) and of 32 bytes after the note, with every check folded into one
/// flag. None sends the record to `slow_record`, which handles every case.
/// Only for records that start at least 8 bytes in and before `fast_end`, which
/// is at least 256 bytes before the end of the text.
#[inline(always)]
fn fast_record(text: &[u8], i: usize, fast_end: usize) -> Option<Record> {
    if i < 8 || i >= fast_end {
        return None;
    }
    // SAFETY (for every access below): `i - 8..i + 256` is inside the text, and
    // every load ends by offset 198 (a comma at 128 or less, a bit index below 64
    // in the tail's masks, and a few bytes).
    let p = unsafe { text.as_ptr().add(i) };
    let at = |k: usize| unsafe { *p.add(k) };
    let load = |k: usize| unsafe { p.add(k).sub(8).cast::<u64>().read_unaligned() }; // ends at k
    let (quotes, commas) = masks(unsafe { &*p.cast() });
    let inside = prefix_xor(quotes);
    let mut out = commas & !inside;
    let id_end = out.trailing_zeros() as usize;
    out &= out.wrapping_sub(1);
    let name_end = out.trailing_zeros() as usize;
    out &= out.wrapping_sub(1);
    // The name: quoted (its first and last bytes are quotes), or empty.
    let name_quoted = (quotes >> (id_end + 1)) & (quotes >> (name_end - 1)) & 1 == 1;
    let name_ok = if name_quoted {
        name_end >= id_end + 3
    } else {
        name_end == id_end + 1
    };
    if name_end >= 64 || !name_ok {
        return None;
    }
    let name_quotes = (quotes & !(u64::MAX << name_end)).count_ones() as usize;
    let name_bytes = name_end - id_end - 1 - name_quoted as usize - name_quotes / 2;
    let name_start = id_end + 1 + name_quoted as usize;
    // SAFETY: the name is inside the 64 bytes at `i`, which has 8 bytes before it.
    let hash = unsafe { hash_words(text, i + name_start, i + name_end - name_quoted as usize) };
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
    let note_quoted = (at(name_end + 1) == b'"') & (at(note_end - 1) == b'"');
    if note_end >= 128 || !note_quoted && note_end != name_end + 1 {
        return None;
    }
    let note_quotes = quotes_to_note as usize - name_quotes;
    let note_bytes = note_end - name_end - 1 - note_quoted as usize - note_quotes / 2;
    // The tail, `region_id,amount`: its first comma and dot, from its 32 bytes.
    let tail = unsafe { _mm256_loadu_si256(p.add(note_end + 1).cast()) };
    let tail_eq =
        |byte: u8| movemask(unsafe { _mm256_cmpeq_epi8(tail, _mm256_set1_epi8(byte as i8)) });
    let region_len = tail_eq(b',').trailing_zeros() as usize;
    let dot = tail_eq(b'.').trailing_zeros() as usize;
    let id_len = id_end - if load(3) >> 40 == 0xBF_BBEF { 3 } else { 0 }; // a BOM: not data
    let negative = at(note_end + 2 + region_len) == b'-';
    // `-?[0-9]{1,5}\.[0-9]{2}`, then CRLF or LF.
    let (region_end, amount_end) = (note_end + 1 + region_len, note_end + 4 + dot);
    let whole = dot.wrapping_sub(region_len + 1 + negative as usize);
    let terminator = unsafe { p.add(amount_end).cast::<u16>().read_unaligned() };
    let crlf = terminator == 0x0A0D;
    // (`|`, not `||`: CRLF or LF is a coin toss, so no branch on it.)
    let lens_ok =
        ((id_len.wrapping_sub(1) | region_len.wrapping_sub(1)) < 8) & (whole.wrapping_sub(1) < 5);
    if !lens_ok | !crlf & (terminator as u8 != b'\n') {
        return None;
    }
    let words = [load(id_end), load(region_end), load(amount_end)];
    let ([id, region_id, c], digits_ok) = numbers(words, [id_len, region_len, whole + 2]);
    let cents = if negative { -(c as i64) } else { c as i64 };
    let (bytes, next) = (
        (name_bytes << 32 | note_bytes) as u64,
        i + amount_end + 1 + crlf as usize,
    );
    digits_ok.then_some(Record(id, region_id, cents, bytes, hash, next))
}

/// Any record starting at `i`, parsed byte by byte as spec 2.1 defines it; None
/// if it is invalid. Kept out of line, so that it does not crowd the registers of
/// the fast path's loop.
#[cold]
#[inline(never)]
fn slow_record(text: &[u8], i: usize) -> Option<Record> {
    let mut at = i + text[i..].starts_with(BOM) as usize * BOM.len(); // a BOM is not data
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
    let cents = if negative { -cents } else { cents };
    // A name at the very end of the file is hashed from a padded copy, so that the
    // hash's loads stay in bounds.
    let hash = match name_end + 8 <= text.len() {
        // SAFETY: just checked; the header puts 8 bytes before any field's end.
        true => unsafe { hash_words(text, name_start, name_end) },
        false => {
            let copy = [&[0; 8], &text[name_start..name_end], &[0; 8]].concat();
            unsafe { hash_words(&copy, 8, copy.len() - 8) }
        }
    };
    Some(Record(
        id,
        region_id,
        cents,
        name_bytes << 32 | note_bytes,
        hash,
        next,
    ))
}

/// The digits at `*at`, which it moves past: their value and their count.
fn digits(text: &[u8], at: &mut usize) -> (u64, usize) {
    let (start, mut value) = (*at, 0u64);
    while let Some(&digit @ b'0'..=b'9') = text.get(*at) {
        value = value.wrapping_mul(10).wrapping_add((digit - b'0') as u64);
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
        *at += rest
            .iter()
            .position(|b| b"\",\r\n".contains(b))
            .unwrap_or(rest.len());
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

/// Bitmaps of a 64-byte block: bit k is set if byte k is `"`, and if it is `,`.
#[inline(always)]
fn masks(block: &[u8; 64]) -> (u64, u64) {
    // SAFETY: AVX2 is enabled for this build (see the compile_error), and the two unaligned
    // 32-byte loads are inside the 64-byte array.
    unsafe {
        let lo = _mm256_loadu_si256(block.as_ptr().cast());
        let hi = _mm256_loadu_si256(block.as_ptr().add(32).cast());
        let eq = |v, byte: u8| movemask(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8)));
        let bits = |byte| eq(lo, byte) | eq(hi, byte) << 32;
        (bits(b'"'), bits(b','))
    }
}

/// `vpmovmskb`, kept opaque to the optimizer. LLVM rewrites the intrinsic as a
/// vector of booleans, and on CPUs without AVX-512 (the scoring box) it then
/// assembled our 64-bit masks one bit at a time: a quarter of the run time.
#[inline(always)]
fn movemask(v: __m256i) -> u64 {
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
    // A carry-less multiply by all ones XORs every lower bit into each bit.
    // SAFETY: PCLMULQDQ is enabled for this build (see the compile_error).
    unsafe {
        let product = _mm_clmulepi64_si128(_mm_set_epi64x(0, bits as i64), _mm_set1_epi8(-1), 0);
        _mm_cvtsi128_si64(product) as u64
    }
}

/// Three numbers at once, from the 8 bytes ending at each (the id, the region id,
/// and the amount with its dot, which is squeezed out), and how many digits each
/// has (1 to 8). Returns the numbers, and whether every digit was one.
#[inline(always)]
fn numbers(words: [u64; 3], lens: [usize; 3]) -> ([u64; 3], bool) {
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
        let a = _mm256_extract_epi32::<0>(v) as u32 as u64;
        let b = _mm256_extract_epi32::<1>(v) as u32 as u64;
        ([a, b, _mm256_extract_epi32::<4>(v) as u32 as u64], ok)
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
    let first = unsafe { _bzhi_u64(load(at), 8 * rest as u32) };
    let last = load(e - 8) & ((rest > 8) as u64).wrapping_neg();
    mix(h ^ first ^ P1, last ^ P0 ^ len)
}
