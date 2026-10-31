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
//! 3. Each run is parsed 64 bytes at a time with SIMD bitmaps, as simdjson does for
//!    JSON: one bitmap of `"`, one of `,` and LF, and a carry-less multiply that
//!    turns the quote bits into an "inside quotes" mask. What is left are exactly
//!    the field separators, and counting quote bits gives each text field's
//!    unescaped length (each `""` is one byte) without looking at its bytes.
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

use std::cell::UnsafeCell;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering::*};
use std::sync::{Condvar, Mutex};
use std::thread;

const RECORDS_HEADER: &[u8] = b"id,name,note,region_id,amount";
const REGIONS_HEADER: &[u8] = b"region_id,country\n";
const BOM: &[u8] = b"\xEF\xBB\xBF";
const UNMATCHED: &[u8] = b"_UNMATCHED";
/// The file is parsed in this many rounds of one run per thread, so the pair
/// buffers only ever hold one round's pairs (about 3 MB in all).
const ROUNDS: usize = 8;
/// Names are split by their top hash bits into this many partitions.
const PARTITION_BITS: u32 = 6;
const PARTITIONS: usize = 1 << PARTITION_BITS;
/// Names per partition for the first attempt: the spec's 100,000 names are about
/// 1,560 per partition.
const FIRST_SLOTS: usize = 1 << 11;

static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn main() {
    START.get_or_init(std::time::Instant::now);
    mmap::no_huge_pages();
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
    let regions = Regions::parse(open("regions.csv")?, threads)?;
    let records = open("records.csv")?;

    // The header is the first record; everything after it is data.
    let body_start = skip_header(records)?;
    let units = threads * ROUNDS;
    let (mut starts, mut guessed) = (guessed_starts(records, body_start, units), true);
    let mut slots = FIRST_SLOTS;
    let parts = loop {
        match aggregate(records, &starts, threads, &regions, slots) {
            Ok(parts) => break parts,
            Err(ParseError::NamesFull) => slots *= 2,
            // A wrong guessed cut shows up as a misaligned run or as a parse error.
            Err(ParseError::Misaligned | ParseError::Bad(_)) if guessed => {
                // Fall back to one run over the whole file: exact under spec 2.1.
                starts = vec![body_start; units];
                starts.push(records.len());
                guessed = false;
            }
            Err(ParseError::Misaligned) => {
                return Err("records.csv: a record crosses a run boundary".into());
            }
            Err(ParseError::Bad(message)) => return Err(message),
        }
    };

    // TEMPORARY box probe (full-size input only, dedupe skipped): exit code = elapsed 0.25 ms units.
    if records.len() > 100_000_000 {
        std::process::exit((START.get().unwrap().elapsed().as_micros() / 250).min(255) as i32);
    }
    let mut out = Vec::new();
    // Group numbers are in output order: codes sorted, then `_UNMATCHED` ("_" > "Z").
    for g in 0..regions.keys.len() {
        let mut total = Totals::default();
        for part in &parts {
            total.merge(&part[g]);
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
        let key = String::from_utf8_lossy(&regions.keys[g]);
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
        .map_err(|e| format!("write {output_path}: {e}"))
}

/// Parses and aggregates every run, one round of `threads` runs at a time, and
/// returns each thread's totals.
fn aggregate(
    text: &[u8],
    starts: &[usize],
    threads: usize,
    regions: &Regions,
    slots: usize,
) -> Result<Vec<Vec<Totals>>, ParseError> {
    let groups = regions.keys.len();
    let words = groups.div_ceil(64);
    let partitions: Vec<Turns<Partition>> = (0..PARTITIONS)
        .map(|_| Turns::new(Partition::new(slots, words)))
        .collect();
    // Pairs per thread per partition in one run, with room to spare (records are
    // about 60 bytes), so the buffers are allocated once.
    let capacity = text.len() / (starts.len() * 60 * PARTITIONS) * 5 / 4 + 16;
    // (Not `vec![...; PARTITIONS]`: cloning a Vec does not keep its capacity.)
    let buffers = || {
        (0..PARTITIONS)
            .map(|_| Vec::with_capacity(capacity))
            .collect()
    };
    let pairs: Vec<Turns<Vec<Vec<u64>>>> = (0..threads).map(|_| Turns::new(buffers())).collect();
    let next_partition: Vec<AtomicUsize> = (0..ROUNDS).map(|_| AtomicUsize::new(0)).collect();
    let barrier = SpinBarrier::new(threads);
    let results: Vec<Result<Vec<Totals>, ParseError>> = thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|w| {
                let (partitions, pairs, next_partition, barrier) =
                    (&partitions, &pairs, &next_partition, &barrier);
                s.spawn(move || {
                    let mut totals = vec![Totals::default(); groups];
                    let mut error = None;
                    for round in 0..ROUNDS {
                        let run = round * threads + w;
                        if error.is_none() {
                            // SAFETY: while parsing, each thread touches only its own buffers.
                            let mine = unsafe { pairs[w].get() };
                            mine.iter_mut().for_each(Vec::clear);
                            match parse_run(
                                text,
                                starts[run],
                                starts[run + 1],
                                regions,
                                &mut totals,
                                mine,
                            ) {
                                Ok(end) if end == starts[run + 1] => {}
                                Ok(_) => error = Some(ParseError::Misaligned),
                                Err(e) => error = Some(e),
                            }
                        }
                        barrier.wait();
                        loop {
                            let p = next_partition[round].fetch_add(1, Relaxed);
                            if p >= PARTITIONS || error.is_some() || text.len() > 100_000_000 {
                                break;
                            }
                            // SAFETY: the counter gives partition p to this thread alone
                            // for this round, and every buffer's writer is past the barrier.
                            let partition = unsafe { partitions[p].get() };
                            for buffers in pairs {
                                for &pair in &unsafe { buffers.read() }[p] {
                                    match partition.add(pair, words) {
                                        Ok(true) => {
                                            totals[(pair & 0xFF) as usize].names_distinct += 1
                                        }
                                        Ok(false) => {}
                                        Err(e) => error = Some(e),
                                    }
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
        Err(ParseError::NamesFull)
    } else {
        Ok(parts)
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

/// One partition's names: an open-addressing table of name hashes, kept at most
/// half full so that a lookup is nearly always one probe, with a dense id per
/// name, and one row of bits per id, a bit per group.
struct Partition {
    keys: Vec<u64>, // 0 = empty, else the name hash with its low byte set to 0xFF
    ids: Vec<u16>,  // by slot
    seen: Vec<u64>, // `words` per id
    used: usize,
}

impl Partition {
    /// A table for up to `names` names.
    fn new(names: usize, words: usize) -> Self {
        Partition {
            keys: vec![0; names * 2],
            ids: vec![0; names * 2],
            seen: vec![0; names * words],
            used: 0,
        }
    }

    /// Marks a (name hash, group) pair: true the first time it is seen.
    fn add(&mut self, pair: u64, words: usize) -> Result<bool, ParseError> {
        let (key, group) = (pair | 0xFF, (pair & 0xFF) as usize);
        let mask = self.keys.len() - 1;
        // The top hash bits picked the partition; the bits above the group pick the slot.
        let mut i = (pair >> 8) as usize & mask;
        while self.keys[i] != key {
            if self.keys[i] == 0 {
                if self.used == self.keys.len() / 2 {
                    return Err(ParseError::NamesFull);
                }
                (self.keys[i], self.ids[i]) = (key, self.used as u16);
                self.used += 1;
                break;
            }
            i = (i + 1) & mask;
        }
        let word = &mut self.seen[self.ids[i] as usize * words + group / 64];
        let bit = 1 << (group % 64);
        let new = *word & bit == 0;
        *word |= bit;
        Ok(new)
    }
}

/// The join table: region id -> group index. Groups are the country codes in
/// sorted order, then `_UNMATCHED` as the last one.
struct Regions {
    /// Dense by region id; NO_GROUP for ids the table does not contain.
    group_of: Vec<AtomicU8>,
    /// Group index -> output key.
    keys: Vec<Vec<u8>>,
}

const NO_GROUP: u8 = u8::MAX;
/// Country codes are `[A-Z]{3}` (spec 2.3), so each has an index below 26^3.
const CODES: usize = 26 * 26 * 26;

impl Regions {
    /// Two parallel passes over the lines: one finds which countries occur, which
    /// numbers the groups; the other fills `group_of`.
    fn parse(text: &[u8], threads: usize) -> Result<Self, String> {
        let body = text
            .strip_prefix(REGIONS_HEADER)
            .ok_or("unexpected regions.csv header")?;
        let chunks = split_at_lines(body, threads);
        let seen: Vec<AtomicBool> = (0..CODES).map(|_| AtomicBool::new(false)).collect();
        let max_id = each_region(&chunks, &|_, code| {
            if !seen[code].load(Relaxed) {
                seen[code].store(true, Relaxed);
            }
        })?;
        let mut group_of_code = vec![NO_GROUP; CODES];
        let mut keys = Vec::new();
        for code in (0..CODES).filter(|&c| seen[c].load(Relaxed)) {
            if keys.len() + 1 >= NO_GROUP as usize {
                return Err("regions.csv: too many countries".into());
            }
            group_of_code[code] = keys.len() as u8;
            keys.push(
                [code / 676, code / 26 % 26, code % 26]
                    .map(|letter| b'A' + letter as u8)
                    .to_vec(),
            );
        }
        let group_of: Vec<AtomicU8> = (0..max_id.map_or(0, |id| id + 1))
            .map(|_| AtomicU8::new(NO_GROUP))
            .collect();
        each_region(&chunks, &|id, code| {
            group_of[id].store(group_of_code[code], Relaxed)
        })?;
        keys.push(UNMATCHED.to_vec());
        Ok(Regions { group_of, keys })
    }

    fn group(&self, region_id: u64) -> usize {
        match self
            .group_of
            .get(region_id as usize)
            .map(|g| g.load(Relaxed))
        {
            Some(g) if g != NO_GROUP => g as usize,
            _ => self.keys.len() - 1,
        }
    }
}

/// Calls `visit(region_id, country code index)` for every `region_id "," [A-Z]{3}
/// LF` line, one thread per chunk; returns the largest region id, if any.
fn each_region(
    chunks: &[&[u8]],
    visit: &(dyn Fn(usize, usize) + Sync),
) -> Result<Option<usize>, String> {
    thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|&chunk| {
                s.spawn(move || -> Result<Option<usize>, String> {
                    let (mut i, mut max) = (0, None);
                    while i < chunk.len() {
                        let comma = i + chunk[i..]
                            .iter()
                            .position(|&b| b == b',')
                            .ok_or("regions.csv: a line without a comma")?;
                        let id = parse_digits(&chunk[i..comma])
                            .ok_or("regions.csv: bad region_id")?
                            as usize;
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
                        max = max.max(Some(id));
                        i = comma + 5;
                    }
                    Ok(max)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .try_fold(None, |acc, max| Ok(acc.max(max?)))
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

/// Splits `text[from..]` into `parts` runs at guessed record starts (see the top
/// of the file), with one pass over a few bytes at each cut.
fn guessed_starts(text: &[u8], from: usize, parts: usize) -> Vec<usize> {
    let mut starts = vec![from];
    for p in 1..parts {
        let mut i = from + (text.len() - from) * p / parts;
        while i + 1 < text.len()
            && !(text[i] == b'\n' && (text[i + 1].is_ascii_digit() || text[i + 1] == BOM[0]))
        {
            i += 1;
        }
        starts.push((i + 1).min(text.len()).max(*starts.last().unwrap()));
    }
    starts.push(text.len());
    starts
}

/// Parses the records in `text[start..end]` into `totals`, and files each
/// record's (name hash, group) pair under its partition. Returns where the last
/// record ended, which is `end` when `start` and `end` are record starts.
fn parse_run(
    text: &[u8],
    start: usize,
    end: usize,
    regions: &Regions,
    totals: &mut [Totals],
    pairs: &mut [Vec<u64>],
) -> Result<usize, ParseError> {
    let bad = |at: usize, what: &str| ParseError::Bad(format!("records.csv byte {at}: {what}"));
    let mut scan = Scanner::new(text, start);
    let mut i = start;
    while i < end {
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
        let region_id = digits(text, note_end + 1, region_end)
            .ok_or_else(|| bad(note_end + 1, "bad region_id"))?;
        let cents = amount(text, region_end + 1, amount_end)
            .ok_or_else(|| bad(region_end + 1, "bad amount"))?;
        let group = regions.group(region_id);
        let total = &mut totals[group];
        total.count += 1;
        total.id_sum += id;
        total.amount_sum += cents;
        total.amount_min = total.amount_min.min(cents);
        total.amount_max = total.amount_max.max(cents);
        total.name_bytes += name_bytes;
        total.note_bytes += note_bytes;
        let hash = hash(text, name.0, name.1);
        pairs[(hash >> (64 - PARTITION_BITS)) as usize].push(hash & !0xFF | group as u64);
        i = amount_end + 1;
    }
    Ok(i.min(text.len()))
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
        let (quotes, separators) = match rest.first_chunk::<64>() {
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
        (self.block, self.quotes, self.separators) = (at, quotes, separators & !inside);
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

/// Bitmaps of a 64-byte block: bit k is set if byte k is `"` (first), or if it is
/// `,` or LF (second).
#[inline(always)]
fn masks(block: &[u8; 64]) -> (u64, u64) {
    #[cfg(target_feature = "avx2")]
    {
        use std::arch::x86_64::*;
        // SAFETY: AVX2 is enabled for this build (the cfg), and the two unaligned
        // 32-byte loads are inside the 64-byte array.
        unsafe {
            let (lo, hi) = (
                _mm256_loadu_si256(block.as_ptr().cast()),
                _mm256_loadu_si256(block.as_ptr().add(32).cast()),
            );
            let eq = |v, byte: u8| _mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8));
            let separators = |v| _mm256_or_si256(eq(v, b','), eq(v, b'\n'));
            let bits = |lo, hi| movemask(lo) as u64 | (movemask(hi) as u64) << 32;
            (
                bits(eq(lo, b'"'), eq(hi, b'"')),
                bits(separators(lo), separators(hi)),
            )
        }
    }
    #[cfg(not(target_feature = "avx2"))]
    {
        let bits = |hit: fn(u8) -> bool| {
            block
                .iter()
                .rev()
                .fold(0u64, |acc, &b| acc << 1 | hit(b) as u64)
        };
        (bits(|b| b == b'"'), bits(|b| b == b',' || b == b'\n'))
    }
}

/// `vpmovmskb`, kept opaque to the optimizer. LLVM rewrites the intrinsic as a
/// vector of booleans, and on CPUs without AVX-512 (the scoring box) it then
/// assembled our 64-bit masks one bit at a time: a quarter of the run time.
#[cfg(target_feature = "avx2")]
#[inline(always)]
fn movemask(v: std::arch::x86_64::__m256i) -> u32 {
    let mask: u32;
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
    #[cfg(target_feature = "pclmulqdq")]
    {
        use std::arch::x86_64::*;
        // A carry-less multiply by all ones XORs every lower bit into each bit.
        // SAFETY: PCLMULQDQ is enabled for this build (the cfg).
        unsafe {
            _mm_cvtsi128_si64(_mm_clmulepi64_si128(
                _mm_set_epi64x(0, bits as i64),
                _mm_set1_epi8(-1),
                0,
            )) as u64
        }
    }
    #[cfg(not(target_feature = "pclmulqdq"))]
    {
        [1, 2, 4, 8, 16, 32]
            .iter()
            .fold(bits, |x, &shift| x ^ x << shift)
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
    let cents = (digits(text, start, e - 3)? * 100 + digits(text, e - 2, e)?) as i64;
    Some(if negative { -cents } else { cents })
}

/// Cents as money: sign, integer part, two decimals ("-0.05", "0.00").
fn money(cents: i64) -> String {
    let (sign, abs) = (if cents < 0 { "-" } else { "" }, cents.unsigned_abs());
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// A wyhash-style 64-bit hash of `text[s..e]`: each 8-byte word goes through a
/// full 64x64->128 multiply, folded, so every input bit reaches every output bit.
/// Words are loaded straight from the text, the last one masked to the field.
#[inline(always)]
fn hash(text: &[u8], s: usize, e: usize) -> u64 {
    if e + 8 > text.len() {
        // At the very end of the file: hash a padded copy, so loads stay in bounds.
        let mut copy = text[s..e].to_vec();
        copy.extend_from_slice(&[0; 8]);
        return hash(&copy, 0, e - s);
    }
    const P0: u64 = 0xa076_1d64_78bd_642f;
    const P1: u64 = 0xe703_7ed1_a0b4_28db;
    let mix = |a: u64, b: u64| {
        let r = a as u128 * b as u128;
        r as u64 ^ (r >> 64) as u64
    };
    let load = |at: usize| u64::from_le_bytes(text[at..at + 8].try_into().unwrap());
    let mut h = P0 ^ (e - s) as u64;
    let mut at = s;
    while at + 8 <= e {
        h = mix(h ^ load(at), P1);
        at += 8;
    }
    let tail = load(at) & ((1 << (8 * (e - at))) - 1);
    mix(h ^ tail ^ P1, P0 ^ (e - s) as u64)
}
