//! Track C (Join): parse the hostile records.csv, join each record to its country
//! through regions.csv, and write per-country aggregates.
//!
//! How it is fast, and why it is still exact:
//! 1. Both files are mmap'd (pre-warmed page cache: no memory charge, no copy).
//! 2. Parallel parsing needs to know, at an arbitrary byte, whether it is inside
//!    quotes. The grammar (spec 2.1) only allows `"` as a quoted field's delimiter
//!    or inside a `""` pair, never in an unquoted field, so "inside quotes" is just
//!    the parity of the number of `"` bytes before that point. Pass 1 counts quotes
//!    per slice; a prefix XOR gives every slice its starting state, and each slice
//!    then starts at its first record terminator outside quotes.
//! 3. Distinct names per country use one table shared by all threads, lock-free.
//!    A name's identity is its raw inner bytes (inside quotes, `""` pairs map one to
//!    one to the unescaped value, and an unquoted value cannot contain `"`), reduced
//!    to a 64-bit hash: with the spec's pool of 100,000 names the chance that two
//!    distinct names share one is about 3e-10. Each name gets a dense id and a row of
//!    one bit per country; the thread whose `fetch_or` sets a bit counts that
//!    (country, name) pair, so there is no merge step at all.

mod mmap;

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};
use std::thread;

const RECORDS_HEADER: &[u8] = b"id,name,note,region_id,amount";
const REGIONS_HEADER: &[u8] = b"region_id,country\n";
const BOM: &[u8] = b"\xEF\xBB\xBF";
const UNMATCHED: &[u8] = b"_UNMATCHED";
/// Name-table slots for the first attempt: 2.6x the spec's 100,000-name pool.
const FIRST_SLOTS: usize = 1 << 18;

fn main() {
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
    let regions = Regions::parse(open("regions.csv")?)?;
    let records = open("records.csv")?;
    let threads = thread::available_parallelism().map_or(1, |n| n.get());

    // The header is the first record; everything after it is data.
    let body_start = skip_header(records)?;
    let starts = record_starts(records, body_start, threads);

    // Parse and aggregate every slice of whole records in parallel. If the input
    // has far more distinct names than the spec's pool, grow the table and redo it.
    let mut slots = FIRST_SLOTS;
    let totals = loop {
        let names = Names::new(slots, regions.keys.len());
        let parts: Result<Vec<Vec<Totals>>, ParseError> = thread::scope(|s| {
            let handles: Vec<_> = starts
                .windows(2)
                .map(|w| {
                    let (regions, names) = (&regions, &names);
                    s.spawn(move || parse_records(records, w[0], w[1], regions, names))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        match parts {
            Ok(parts) => break parts,
            Err(ParseError::NamesFull) => slots *= 4,
            Err(ParseError::Bad(message)) => return Err(message),
        }
    };

    let mut out = Vec::new();
    for g in regions.order() {
        let mut total = Totals::default();
        for part in &totals {
            total.merge(&part[g]);
        }
        if total.count == 0 {
            continue;
        }
        out.extend_from_slice(&regions.keys[g]);
        for field in [
            Field::Int(total.count),
            Field::Int(total.id_sum),
            Field::Money(total.amount_sum),
            Field::Money(total.amount_min),
            Field::Money(total.amount_max),
            Field::Int(total.names_distinct),
            Field::Int(total.name_bytes),
            Field::Int(total.note_bytes),
        ] {
            out.push(b' ');
            field.write(&mut out);
        }
        out.push(b'\n');
    }
    let output_path = format!("{out_dir}/aggregates.txt");
    File::create(&output_path)
        .and_then(|mut f| f.write_all(&out))
        .map_err(|e| format!("write {output_path}: {e}"))
}

/// The join table: region id -> group index. Groups are the country codes in
/// first-seen order, then `_UNMATCHED` as the last one.
struct Regions {
    /// Dense by region id; NO_GROUP for ids the table does not contain.
    group_of: Vec<u16>,
    /// Group index -> output key.
    keys: Vec<Vec<u8>>,
}

const NO_GROUP: u16 = u16::MAX;

impl Regions {
    fn parse(text: &[u8]) -> Result<Self, String> {
        let body = text.strip_prefix(REGIONS_HEADER).ok_or("unexpected regions.csv header")?;
        let mut index: HashMap<&[u8], u16> = HashMap::new();
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut group_of = Vec::new();
        for line in body.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let comma = line.iter().position(|&b| b == b',').ok_or("regions.csv: a line without a comma")?;
            let id = parse_digits(&line[..comma]).ok_or("regions.csv: bad region_id")? as usize;
            let country = &line[comma + 1..];
            let group = *index.entry(country).or_insert_with(|| {
                keys.push(country.to_vec());
                (keys.len() - 1) as u16
            });
            if id >= group_of.len() {
                group_of.resize(id + 1, NO_GROUP);
            }
            group_of[id] = group;
        }
        if keys.len() >= NO_GROUP as usize {
            return Err("regions.csv: too many countries".into());
        }
        keys.push(UNMATCHED.to_vec());
        Ok(Regions { group_of, keys })
    }

    fn prefetch(&self, region_id: u64) {
        if let Some(entry) = self.group_of.get(region_id as usize) {
            prefetch(entry);
        }
    }

    fn group(&self, region_id: u64) -> usize {
        match self.group_of.get(region_id as usize) {
            Some(&g) if g != NO_GROUP => g as usize,
            _ => self.keys.len() - 1,
        }
    }

    /// Group indices in output order: bytewise by key.
    fn order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.keys.len()).collect();
        order.sort_by(|&a, &b| self.keys[a].cmp(&self.keys[b]));
        order
    }
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
    /// The name table passed half full; retry with a bigger one.
    NamesFull,
    Bad(String),
}

/// Lock-free name table shared by all threads: 64-bit name hash -> dense id, plus
/// one bit per (name id, group).
struct Names {
    /// Slot i is `table[2i]` (the hash, 0 = empty) and `table[2i + 1]` (id + 1 once
    /// published, FULL if the table ran out): both in one cache line.
    table: &'static [AtomicU64],
    next_id: AtomicU32,
    max_ids: u32,
    seen: &'static [AtomicU64], // `words` per name id
    words: usize,
}

const FULL: u64 = u64::MAX;

impl Names {
    fn new(slots: usize, groups: usize) -> Self {
        let words = groups.div_ceil(64);
        let max_ids = slots / 2;
        Names {
            table: zeroed_atomics(slots * 2),
            next_id: AtomicU32::new(0),
            max_ids: max_ids as u32,
            seen: zeroed_atomics(max_ids * words),
            words,
        }
    }

    /// The dense id of the name with this hash, inserting it if it is new.
    fn home(&self, hash: u64) -> usize {
        (hash >> 32) as usize & (self.table.len() / 2 - 1)
    }

    fn id(&self, hash: u64) -> Result<usize, ParseError> {
        let hash = hash.max(1);
        let mask = self.table.len() / 2 - 1;
        let mut i = self.home(hash);
        loop {
            let current = self.table[2 * i].load(Acquire);
            if current == 0 {
                match self.table[2 * i].compare_exchange(0, hash, AcqRel, Acquire) {
                    Ok(_) => {
                        let id = self.next_id.fetch_add(1, Relaxed);
                        let full = id >= self.max_ids;
                        self.table[2 * i + 1].store(if full { FULL } else { id as u64 + 1 }, Release);
                        return if full { Err(ParseError::NamesFull) } else { Ok(id as usize) };
                    }
                    Err(other) if other == hash => return self.published(i),
                    Err(_) => {}
                }
            } else if current == hash {
                return self.published(i);
            }
            i = (i + 1) & mask;
        }
    }

    /// Waits the few nanoseconds until the inserting thread publishes slot i's id.
    fn published(&self, i: usize) -> Result<usize, ParseError> {
        loop {
            match self.table[2 * i + 1].load(Acquire) {
                0 => std::hint::spin_loop(),
                FULL => return Err(ParseError::NamesFull),
                id => return Ok(id as usize - 1),
            }
        }
    }

    fn prefetch_slot(&self, hash: u64) {
        prefetch(&self.table[2 * self.home(hash.max(1))]);
    }

    fn prefetch_row(&self, id: usize, group: usize) {
        prefetch(&self.seen[id * self.words + group / 64]);
    }

    /// Marks (name, group); true for exactly one caller per pair, the first.
    fn first_in_group(&self, id: usize, group: usize) -> bool {
        let word = &self.seen[id * self.words + group / 64];
        let bit = 1 << (group % 64);
        word.load(Relaxed) & bit == 0 && word.fetch_or(bit, Relaxed) & bit == 0
    }
}

/// A zero-initialised atomic array, populated up front (see `mmap::zeroed`).
fn zeroed_atomics<A>(len: usize) -> &'static [A] {
    let ptr = mmap::zeroed(len * size_of::<A>()).unwrap_or_else(|e| {
        eprintln!("track-c: mmap: {e}");
        std::process::exit(1);
    });
    // SAFETY: the mapping is page-aligned, zeroed, at least `len` atomics long, and
    // all-zero bytes are a valid value for an integer atomic.
    unsafe { std::slice::from_raw_parts(ptr as *const A, len) }
}

/// Returns the offset of the first data record, after the BOM and header.
fn skip_header(text: &[u8]) -> Result<usize, String> {
    let start = if text.starts_with(BOM) { BOM.len() } else { 0 };
    let rest = &text[start..];
    let end = rest.strip_prefix(RECORDS_HEADER).ok_or("unexpected records.csv header")?;
    let terminator = match end {
        [b'\r', b'\n', ..] => 2,
        [b'\n', ..] => 1,
        [] => 0,
        _ => return Err("unexpected records.csv header".into()),
    };
    Ok(start + RECORDS_HEADER.len() + terminator)
}

/// Splits `text[from..]` into about `parts` runs of whole records and returns their
/// start offsets, ending with `text.len()`.
fn record_starts(text: &[u8], from: usize, parts: usize) -> Vec<usize> {
    let bounds: Vec<usize> = (0..=parts).map(|p| from + (text.len() - from) * p / parts).collect();
    // Pass 1, parallel: quote parity of each raw slice.
    let odd: Vec<bool> = thread::scope(|s| {
        let handles: Vec<_> = bounds
            .windows(2)
            .map(|w| s.spawn(move || text[w[0]..w[1]].iter().filter(|&&b| b == b'"').count() % 2 == 1))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut starts = vec![from];
    let mut in_quotes = false;
    for p in 1..parts {
        in_quotes ^= odd[p - 1];
        // Walk to the first LF outside quotes; the record after it starts here.
        let (mut i, mut quoted) = (bounds[p], in_quotes);
        while i < text.len() && (quoted || text[i] != b'\n') {
            quoted ^= text[i] == b'"';
            i += 1;
        }
        starts.push((i + 1).min(text.len()).max(*starts.last().unwrap()));
    }
    starts.push(text.len());
    starts
}

/// Records are handled in batches of this many, so that their cache misses overlap.
const BATCH: usize = 16;

/// One parsed record, waiting for its table lookups.
#[derive(Clone, Copy, Default)]
struct Record {
    id: u64,
    region_id: u64,
    cents: i64,
    name_bytes: u64,
    note_bytes: u64,
    hash: u64,
    group: usize,
    name: usize,
}

fn parse_records(text: &[u8], mut i: usize, end: usize, regions: &Regions, names: &Names) -> Result<Vec<Totals>, ParseError> {
    let mut totals = vec![Totals::default(); regions.keys.len()];
    let bad = |at: usize, what: &str| ParseError::Bad(format!("records.csv byte {at}: {what}"));
    let mut batch = [Record::default(); BATCH];
    while i < end {
        // Each record needs three random reads: its region's group, its name's slot,
        // then the (name, group) bit. Done one record at a time, each is a stall.
        // So: parse a batch and prefetch the first two for all of it, then resolve
        // them and prefetch the bits, then set the bits. The misses now overlap.
        let mut n = 0;
        while n < BATCH && i < end {
            if text[i..].starts_with(BOM) {
                i += BOM.len(); // a BOM right after a terminator is not data
            }
            let (id, next) = digits_until_comma(text, i).ok_or_else(|| bad(i, "bad id"))?;
            let (name, name_bytes, next) = text_field(text, next).ok_or_else(|| bad(next, "bad name"))?;
            let (_, note_bytes, next) = text_field(text, next).ok_or_else(|| bad(next, "bad note"))?;
            let (region_id, next) = digits_until_comma(text, next).ok_or_else(|| bad(next, "bad region_id"))?;
            let (cents, next) = amount(text, next).ok_or_else(|| bad(next, "bad amount"))?;
            i = match &text[next..] {
                [b'\r', b'\n', ..] => next + 2,
                [b'\n', ..] | [] => next + 1,
                _ => return Err(bad(next, "expected a record terminator")),
            };
            let hash = hash(name);
            names.prefetch_slot(hash);
            regions.prefetch(region_id);
            batch[n] = Record { id, region_id, cents, name_bytes, note_bytes, hash, group: 0, name: 0 };
            n += 1;
        }
        for r in &mut batch[..n] {
            r.group = regions.group(r.region_id);
            let total = &mut totals[r.group];
            total.count += 1;
            total.id_sum += r.id;
            total.amount_sum += r.cents;
            total.amount_min = total.amount_min.min(r.cents);
            total.amount_max = total.amount_max.max(r.cents);
            total.name_bytes += r.name_bytes;
            total.note_bytes += r.note_bytes;
            r.name = names.id(r.hash)?;
            names.prefetch_row(r.name, r.group);
        }
        for r in &batch[..n] {
            if names.first_in_group(r.name, r.group) {
                totals[r.group].names_distinct += 1;
            }
        }
    }
    Ok(totals)
}

fn prefetch<T>(item: &T) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: prefetching is only a hint and never faults.
    unsafe {
        std::arch::x86_64::_mm_prefetch(item as *const T as *const i8, std::arch::x86_64::_MM_HINT_T0)
    };
}

/// `1*DIGIT ","` at `i`: the value and the offset after the comma.
fn digits_until_comma(text: &[u8], i: usize) -> Option<(u64, usize)> {
    let len = text[i..].iter().position(|&b| b == b',')?;
    Some((parse_digits(&text[i..i + len])?, i + len + 1))
}

fn parse_digits(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() || digits.len() > 18 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(digits.iter().fold(0, |acc, &b| acc * 10 + (b - b'0') as u64))
}

/// A name or note field ending in ",": its raw inner bytes, its unescaped byte
/// length, and the offset after the comma.
fn text_field(text: &[u8], i: usize) -> Option<(&[u8], u64, usize)> {
    if text.get(i) != Some(&b'"') {
        let len = text[i..].iter().position(|&b| matches!(b, b',' | b'"' | b'\r' | b'\n'))?;
        return (text[i + len] == b',').then_some((&text[i..i + len], len as u64, i + len + 1));
    }
    let (start, mut j, mut escapes) = (i + 1, i + 1, 0u64);
    loop {
        j += text[j..].iter().position(|&b| b == b'"')?;
        if text.get(j + 1) == Some(&b'"') {
            escapes += 1; // `""` is one literal quote
            j += 2;
        } else {
            break;
        }
    }
    // The closing quote must be followed by the field's comma.
    (text.get(j + 1) == Some(&b',')).then_some((&text[start..j], (j - start) as u64 - escapes, j + 2))
}

/// `[ "-" ] 1*DIGIT "." 2DIGIT` at `i`: the value in cents and the offset after it.
fn amount(text: &[u8], i: usize) -> Option<(i64, usize)> {
    let negative = text.get(i) == Some(&b'-');
    let start = i + negative as usize;
    let dot = start + text[start..].iter().position(|&b| b == b'.')?;
    let whole = parse_digits(&text[start..dot])?;
    let fraction = parse_digits(text.get(dot + 1..dot + 3)?)?;
    let cents = (whole * 100 + fraction) as i64;
    Some((if negative { -cents } else { cents }, dot + 3))
}

enum Field {
    Int(u64),
    Money(i64),
}

impl Field {
    fn write(&self, out: &mut Vec<u8>) {
        match *self {
            Field::Int(v) => push_decimal(out, v),
            Field::Money(cents) => {
                if cents < 0 {
                    out.push(b'-');
                }
                let abs = cents.unsigned_abs();
                push_decimal(out, abs / 100);
                out.extend_from_slice(&[b'.', b'0' + (abs % 100 / 10) as u8, b'0' + (abs % 10) as u8]);
            }
        }
    }
}

/// A wyhash-style 64-bit hash: each 8-byte word goes through a full 64x64->128
/// multiply, folded, so every input bit reaches every output bit.
fn hash(bytes: &[u8]) -> u64 {
    const P0: u64 = 0xa076_1d64_78bd_642f;
    const P1: u64 = 0xe703_7ed1_a0b4_28db;
    let mix = |a: u64, b: u64| {
        let r = a as u128 * b as u128;
        r as u64 ^ (r >> 64) as u64
    };
    let mut h = P0 ^ bytes.len() as u64;
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        h = mix(h ^ u64::from_le_bytes(word.try_into().unwrap()), P1);
    }
    let mut tail = [0u8; 8];
    tail[..words.remainder().len()].copy_from_slice(words.remainder());
    mix(h ^ u64::from_le_bytes(tail) ^ P1, P0 ^ bytes.len() as u64)
}

fn push_decimal(out: &mut Vec<u8>, mut value: u64) {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
}
