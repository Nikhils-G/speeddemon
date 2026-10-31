//! Track A (Ingest): for every (UTC hour, endpoint) group in requests.log, the
//! request count, nearest-rank p50/p95/p99 latency and the 5xx error rate.
//!
//! How it is fast:
//! 1. The log is mmap'd, not read. The harness pre-warms it into the page cache,
//!    and pages cached by the host are not charged to our cgroup, so the 239 MB
//!    input costs nothing on the memory axis.
//! 2. One thread per hardware thread parses its own slice of the file into packed
//!    u64 records (hour, endpoint, latency). Endpoints are interned per thread,
//!    with no allocation per line.
//! 3. Endpoints are then ranked in byte order, so `group = hour * E + rank` is
//!    already the output order. A counting sort by group lays every group's
//!    latencies out contiguously, and each group is sorted on its own (groups are
//!    small), which gives exact percentiles without one big sort.

mod mmap;

use std::fs::File;
use std::io::Write;
use std::thread;

/// Hours are stored relative to the first line's hour minus this slack, in 12 bits.
/// The spec fixes a 72-hour span; anything outside +-85 days is reported as an error.
const HOUR_SLACK: u32 = 2048;
const HOUR_BITS: u32 = 12;
const ENDPOINT_BITS: u32 = 20;

fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    if let Err(message) = run(&data_dir, &out_dir) {
        eprintln!("track-a: {message}");
        std::process::exit(1);
    }
}

fn run(data_dir: &str, out_dir: &str) -> Result<(), String> {
    let input_path = format!("{data_dir}/requests.log");
    let input = File::open(&input_path).map_err(|e| format!("open {input_path}: {e}"))?;
    let log = mmap::map(&input).map_err(|e| format!("mmap {input_path}: {e}"))?;
    let output_path = format!("{out_dir}/summary.txt");
    let mut output = File::create(&output_path).map_err(|e| format!("create {output_path}: {e}"))?;
    if log.is_empty() {
        return Ok(()); // the spec's empty input gives an empty summary
    }

    let base = hour_of(log) - HOUR_SLACK;
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let chunks = split_at_lines(log, threads);

    // Pass 1, parallel: parse each chunk into packed records.
    let parts: Vec<Part> = thread::scope(|s| {
        let handles: Vec<_> = chunks.iter().map(|&chunk| s.spawn(move || parse_chunk(chunk, base))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, _>>()
    })?;

    // Rank every distinct endpoint in byte order; `rank_of[part][local id]` maps to it.
    let mut names: Vec<&[u8]> = parts.iter().flat_map(|p| p.endpoints.iter().copied()).collect();
    names.sort_unstable();
    names.dedup();
    let rank_of: Vec<Vec<u32>> = parts
        .iter()
        .map(|p| p.endpoints.iter().map(|name| names.binary_search(name).unwrap() as u32).collect())
        .collect();
    let hour_lo = parts.iter().map(|p| p.hour_lo).min().unwrap();
    let hour_hi = parts.iter().map(|p| p.hour_hi).max().unwrap();
    let endpoints = names.len();
    let groups = (hour_hi - hour_lo + 1) as usize * endpoints;
    let group_of = |part: usize, key: u64| -> usize {
        let hour = (key >> ENDPOINT_BITS) as u32 - hour_lo;
        let local = (key & ((1 << ENDPOINT_BITS) - 1)) as usize;
        hour as usize * endpoints + rank_of[part][local] as usize
    };

    // Pass 2, parallel: per-part group histograms.
    let mut slots: Vec<Vec<u32>> = thread::scope(|s| {
        let handles: Vec<_> = parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                s.spawn(move || {
                    let mut counts = vec![0u32; groups];
                    for &record in &part.records {
                        counts[group_of(i, record >> 32)] += 1;
                    }
                    counts
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Turn the histograms into write positions: group by group, part by part.
    let mut group_start = vec![0u32; groups + 1];
    let mut next = 0u32;
    for g in 0..groups {
        group_start[g] = next;
        for part_slots in slots.iter_mut() {
            let count = part_slots[g];
            part_slots[g] = next;
            next += count;
        }
    }
    group_start[groups] = next;

    // Pass 3, parallel: scatter each latency to its slot.
    let mut latencies = vec![0u32; next as usize];
    let target = SharedSlice(latencies.as_mut_ptr());
    thread::scope(|s| {
        for (i, (part, part_slots)) in parts.iter().zip(slots.iter_mut()).enumerate() {
            let target = &target;
            s.spawn(move || {
                for &record in &part.records {
                    let slot = &mut part_slots[group_of(i, record >> 32)];
                    // SAFETY: the prefix sum above gives every (group, part) pair its own
                    // disjoint range of `latencies`, so no two threads write the same index.
                    unsafe { *target.0.add(*slot as usize) = record as u32 };
                    *slot += 1;
                }
            });
        }
    });
    drop(slots);

    let mut errors = vec![0u32; groups];
    for (i, part) in parts.iter().enumerate() {
        for &key in &part.errors {
            errors[group_of(i, key as u64)] += 1;
        }
    }
    drop(parts);

    // Pass 4, parallel: sort each group, format its line. Split the groups into
    // contiguous ranges of roughly equal latency counts so the output stays in order.
    let hour_labels: Vec<[u8; 20]> = (hour_lo..=hour_hi).map(|h| hour_label(base + h)).collect();
    let mut bounds = vec![0usize];
    for t in 1..threads {
        let want = (next as u64 * t as u64 / threads as u64) as u32;
        bounds.push(group_start.partition_point(|&s| s < want).min(groups).max(*bounds.last().unwrap()));
    }
    bounds.push(groups);
    let buffers: Vec<Vec<u8>> = thread::scope(|s| {
        let mut rest: &mut [u32] = &mut latencies;
        let mut handles = Vec::new();
        for w in bounds.windows(2) {
            let (g0, g1) = (w[0], w[1]);
            let len = (group_start[g1] - group_start[g0]) as usize;
            let (mine, tail) = rest.split_at_mut(len);
            rest = tail;
            let (group_start, errors, names, hour_labels) = (&group_start, &errors, &names, &hour_labels);
            handles.push(s.spawn(move || {
                let mut out = Vec::with_capacity(len * 4);
                let offset = group_start[g0] as usize;
                for g in g0..g1 {
                    let values = &mut mine[group_start[g] as usize - offset..group_start[g + 1] as usize - offset];
                    if values.is_empty() {
                        continue;
                    }
                    values.sort_unstable();
                    let (n, errs) = (values.len() as u64, errors[g] as u64);
                    out.extend_from_slice(&hour_labels[g / endpoints]);
                    out.push(b' ');
                    out.extend_from_slice(names[g % endpoints]);
                    for value in [n, rank(values, 50), rank(values, 95), rank(values, 99), errs, errs * 1_000_000 / n] {
                        out.push(b' ');
                        push_decimal(&mut out, value);
                    }
                    out.push(b'\n');
                }
                out
            }));
        }
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for buffer in &buffers {
        output.write_all(buffer).map_err(|e| format!("write {output_path}: {e}"))?;
    }
    Ok(())
}

struct SharedSlice(*mut u32);
// SAFETY: only used for the disjoint scatter writes in pass 3.
unsafe impl Sync for SharedSlice {}

/// One thread's parse of its chunk.
struct Part<'a> {
    /// `hour << 52 | local endpoint << 32 | latency`, hour relative to `base`.
    records: Vec<u64>,
    /// `hour << 20 | local endpoint` of every 5xx line.
    errors: Vec<u32>,
    /// Local endpoint id -> its bytes in the mmap.
    endpoints: Vec<&'a [u8]>,
    hour_lo: u32,
    hour_hi: u32,
}

fn parse_chunk(chunk: &[u8], base: u32) -> Result<Part<'_>, String> {
    let mut part = Part { records: Vec::with_capacity(chunk.len() / 70), errors: Vec::new(), endpoints: Vec::new(), hour_lo: u32::MAX, hour_hi: 0 };
    let mut interner = Interner::new();
    // Lines from one shard arrive in time order, so most lines repeat the previous
    // line's hour: compare the 13 bytes "YYYY-MM-DDTHH" instead of reparsing them.
    let mut last_prefix = [0u8; 13];
    let mut hour = 0u32;
    let mut i = 0;
    while i < chunk.len() {
        let line = &chunk[i..];
        if line[..13] != last_prefix {
            last_prefix.copy_from_slice(&line[..13]);
            let absolute = hour_of(line);
            if absolute < base || absolute - base >= 1 << HOUR_BITS {
                return Err(format!("timestamp {} is too far from the first line's", String::from_utf8_lossy(&line[..27])));
            }
            hour = absolute - base;
            part.hour_lo = part.hour_lo.min(hour);
            part.hour_hi = part.hour_hi.max(hour);
        }
        // "ts SP method SP endpoint SP status SP latency LF": ts is 27 bytes and the
        // shortest method is 3, so the method's space is at 31 or later.
        let mut e = 31;
        while line[e] != b' ' {
            e += 1;
        }
        let start = e + 1;
        e = start;
        while line[e] != b' ' {
            e += 1;
        }
        let endpoint = interner.intern(&line[start..e], &mut part.endpoints);
        if endpoint >= 1 << ENDPOINT_BITS {
            return Err("more endpoints than the record layout holds".into());
        }
        let key = hour << ENDPOINT_BITS | endpoint;
        // Status is always three digits, 100 to 599, so a 5xx starts with '5'.
        if line[e + 1] == b'5' {
            part.errors.push(key);
        }
        let mut j = e + 5;
        let mut latency = 0u64;
        while line[j] != b'\n' {
            latency = latency * 10 + (line[j] - b'0') as u64;
            j += 1;
        }
        part.records.push((key as u64) << 32 | latency);
        i += j + 1;
    }
    Ok(part)
}

/// Open-addressing endpoint table; names are slices of the mmap, so nothing is copied.
struct Interner {
    slots: Vec<u32>, // 0 = empty, else local id + 1
}

impl Interner {
    fn new() -> Self {
        Interner { slots: vec![0; 1 << 13] }
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
            self.grow(names);
        }
        names.len() as u32 - 1
    }

    fn grow(&mut self, names: &[&[u8]]) {
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

/// Hours since 1970-01-01T00Z of a line starting "YYYY-MM-DDTHH".
fn hour_of(line: &[u8]) -> u32 {
    let num = |range: std::ops::Range<usize>| line[range].iter().fold(0i64, |acc, &b| acc * 10 + (b - b'0') as i64);
    (days_from_civil(num(0..4), num(5..7), num(8..10)) * 24 + num(11..13)) as u32
}

/// "YYYY-MM-DDTHH:00:00Z" for hours since 1970-01-01T00Z.
fn hour_label(hours: u32) -> [u8; 20] {
    let (y, m, d) = civil_from_days(hours as i64 / 24);
    let mut label = *b"0000-00-00T00:00:00Z";
    for (pos, value, width) in [(0, y, 4), (5, m, 2), (8, d, 2), (11, hours as i64 % 24, 2)] {
        let mut v = value;
        for k in (pos..pos + width).rev() {
            label[k] = b'0' + (v % 10) as u8;
            v /= 10;
        }
    }
    label
}

// Howard Hinnant's civil calendar algorithms (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

/// Nearest rank: the value at 1-indexed rank ceil(p * n / 100) of sorted values.
fn rank(sorted: &[u32], p: usize) -> u64 {
    sorted[(p * sorted.len() + 99) / 100 - 1] as u64
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
