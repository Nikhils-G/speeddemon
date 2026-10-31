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
//! 3. Names are never copied. A name's identity is its raw inner bytes: inside
//!    quotes, `""` pairs map one to one to the unescaped value, and an unquoted
//!    value cannot contain `"`, so equal raw inner bytes mean equal values and vice
//!    versa. Each thread interns names and keeps, per name, a bitset of the
//!    countries it appeared in; the threads' bitsets are OR-ed together at the end.

mod mmap;

use std::fs::File;
use std::io::Write;
use std::thread;

const RECORDS_HEADER: &[u8] = b"id,name,note,region_id,amount";
const REGIONS_HEADER: &[u8] = b"region_id,country\n";
const BOM: &[u8] = b"\xEF\xBB\xBF";
const UNMATCHED: &[u8] = b"_UNMATCHED";

fn main() {
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

    // Parse and aggregate every slice of whole records in parallel.
    let groups = regions.keys.len();
    let parts: Vec<Part> = thread::scope(|s| {
        let handles: Vec<_> = starts
            .windows(2)
            .map(|w| {
                let regions = &regions;
                s.spawn(move || parse_records(records, w[0], w[1], regions))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, _>>()
    })?;

    // Merge the per-thread totals and the per-name country bitsets.
    let words = groups.div_ceil(64);
    let mut totals = vec![Totals::default(); groups];
    let mut interner = Interner::new();
    let mut names: Vec<&[u8]> = Vec::new();
    let mut seen_in: Vec<u64> = Vec::new();
    for part in &parts {
        for (total, add) in totals.iter_mut().zip(&part.totals) {
            total.merge(add);
        }
        for (local, &name) in part.names.iter().enumerate() {
            let id = interner.intern(name, &mut names) as usize;
            if seen_in.len() < names.len() * words {
                seen_in.resize(names.len() * words, 0);
            }
            for w in 0..words {
                seen_in[id * words + w] |= part.seen_in[local * words + w];
            }
        }
    }

    let mut out = Vec::new();
    for g in regions.order() {
        let total = &totals[g];
        if total.count == 0 {
            continue;
        }
        let distinct: u64 = seen_in.chunks_exact(words).map(|bits| (bits[g / 64] >> (g % 64) & 1) as u64).sum();
        out.extend_from_slice(&regions.keys[g]);
        for field in [
            Field::Int(total.count),
            Field::Int(total.id_sum),
            Field::Money(total.amount_sum),
            Field::Money(total.amount_min),
            Field::Money(total.amount_max),
            Field::Int(distinct),
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

/// The join table: region id -> group index, where groups are the country codes
/// plus `_UNMATCHED` as the last one.
struct Regions {
    /// Dense by region id; `None` for ids the table does not contain.
    group_of: Vec<Option<u16>>,
    /// Group index -> output key.
    keys: Vec<Vec<u8>>,
}

impl Regions {
    fn parse(text: &[u8]) -> Result<Self, String> {
        let body = text.strip_prefix(REGIONS_HEADER).ok_or("unexpected regions.csv header")?;
        let mut rows = Vec::new();
        for line in body.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let comma = line.iter().position(|&b| b == b',').ok_or("regions.csv: a line without a comma")?;
            let id = parse_digits(&line[..comma]).ok_or("regions.csv: bad region_id")?;
            rows.push((id as usize, &line[comma + 1..]));
        }
        let mut keys: Vec<Vec<u8>> = rows.iter().map(|&(_, country)| country.to_vec()).collect();
        keys.sort_unstable();
        keys.dedup();
        if keys.len() >= u16::MAX as usize {
            return Err("regions.csv: too many countries".into());
        }
        let mut group_of = vec![None; rows.iter().map(|&(id, _)| id + 1).max().unwrap_or(0)];
        for (id, country) in rows {
            group_of[id] = Some(keys.binary_search_by(|k| k.as_slice().cmp(country)).unwrap() as u16);
        }
        keys.push(UNMATCHED.to_vec());
        Ok(Regions { group_of, keys })
    }

    fn group(&self, region_id: u64) -> usize {
        match self.group_of.get(region_id as usize) {
            Some(&Some(g)) => g as usize,
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
    name_bytes: u64,
    note_bytes: u64,
}

impl Default for Totals {
    fn default() -> Self {
        Totals { count: 0, id_sum: 0, amount_sum: 0, amount_min: i64::MAX, amount_max: i64::MIN, name_bytes: 0, note_bytes: 0 }
    }
}

impl Totals {
    fn merge(&mut self, other: &Totals) {
        self.count += other.count;
        self.id_sum += other.id_sum;
        self.amount_sum += other.amount_sum;
        self.amount_min = self.amount_min.min(other.amount_min);
        self.amount_max = self.amount_max.max(other.amount_max);
        self.name_bytes += other.name_bytes;
        self.note_bytes += other.note_bytes;
    }
}

/// One thread's aggregates over its slice of records.
struct Part {
    totals: Vec<Totals>,
    /// Local name id -> raw inner bytes of the name.
    names: Vec<&'static [u8]>,
    /// `words` u64s per local name id: bit g is set if the name occurs in group g.
    seen_in: Vec<u64>,
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

fn parse_records(text: &'static [u8], mut i: usize, end: usize, regions: &Regions) -> Result<Part, String> {
    let groups = regions.keys.len();
    let words = groups.div_ceil(64);
    let mut part = Part { totals: vec![Totals::default(); groups], names: Vec::new(), seen_in: Vec::new() };
    let mut interner = Interner::new();
    let bad = |at: usize, what: &str| format!("records.csv byte {at}: {what}");
    while i < end {
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

        let g = regions.group(region_id);
        let total = &mut part.totals[g];
        total.count += 1;
        total.id_sum += id;
        total.amount_sum += cents;
        total.amount_min = total.amount_min.min(cents);
        total.amount_max = total.amount_max.max(cents);
        total.name_bytes += name_bytes;
        total.note_bytes += note_bytes;
        let local = interner.intern(name, &mut part.names) as usize;
        if part.seen_in.len() < part.names.len() * words {
            part.seen_in.resize(part.names.len() * words, 0);
        }
        part.seen_in[local * words + g / 64] |= 1 << (g % 64);
    }
    Ok(part)
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
fn text_field(text: &'static [u8], i: usize) -> Option<(&'static [u8], u64, usize)> {
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

/// Open-addressing string table; names are slices of the mmap, so nothing is copied.
struct Interner {
    slots: Vec<u32>, // 0 = empty, else id + 1
}

impl Interner {
    fn new() -> Self {
        Interner { slots: vec![0; 1 << 16] }
    }

    fn intern<'a>(&mut self, name: &'a [u8], names: &mut Vec<&'a [u8]>) -> u32 {
        let mask = self.slots.len() - 1;
        let mut i = hash(name) as usize & mask;
        loop {
            match self.slots[i] {
                0 => break,
                id if names[id as usize - 1] == name => return id - 1,
                _ => i = (i + 1) & mask,
            }
        }
        names.push(name);
        self.slots[i] = names.len() as u32;
        if names.len() * 2 > self.slots.len() {
            self.slots = vec![0; self.slots.len() * 2];
            let mask = self.slots.len() - 1;
            for (id, name) in names.iter().enumerate() {
                let mut i = hash(name) as usize & mask;
                while self.slots[i] != 0 {
                    i = (i + 1) & mask;
                }
                self.slots[i] = id as u32 + 1;
            }
        }
        names.len() as u32 - 1
    }
}

/// A fast multiply-rotate hash over 8-byte words.
fn hash(bytes: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h = bytes.len() as u64;
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        h = (h ^ u64::from_le_bytes(word.try_into().unwrap())).wrapping_mul(K).rotate_left(31);
    }
    let mut tail = [0u8; 8];
    tail[..words.remainder().len()].copy_from_slice(words.remainder());
    h = (h ^ u64::from_le_bytes(tail)).wrapping_mul(K);
    h ^ (h >> 32)
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
