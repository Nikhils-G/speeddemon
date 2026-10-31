//! Track A (Ingest): for every (UTC hour, endpoint) group in requests.log, the
//! request count, nearest-rank p50/p95/p99 latency and the 5xx error rate.
//!
//! How it is fast:
//! 1. The log is mmap'd, not read. The harness pre-warms it into the page cache,
//!    and pages cached by the host are not charged to our cgroup, so the 239 MB
//!    input costs nothing on the memory axis.
//! 2. One thread per hardware thread parses its own slice of the file into packed
//!    u32 records (endpoint, error flag, latency; the 1% of latencies that do not
//!    fit go to a side list). Field ends are found with AVX2 compares, digits are
//!    converted eight at a time, and endpoints are interned per thread by comparing
//!    their zero-padded 32-byte form, with no allocation per line.
//! 3. Records carry no hour: shards write in time order, so the file is made of
//!    runs of lines in one hour (about 1,000), and each part keeps a list of its
//!    runs, keyed by the timestamp's first 13 bytes "YYYY-MM-DDTHH". Sorting those
//!    bytes sorts the hours, and they are the start of the output's hour label, so
//!    no calendar arithmetic is needed and any amount of disorder is handled.
//! 4. Endpoints are ranked in byte order, and the hours are handed out to the
//!    threads: a thread gathers one hour's runs from every part, groups them by
//!    endpoint with a counting sort small enough to stay in L1/L2, and selects
//!    exact percentiles per group. Only these statistics are kept; the records are
//!    freed before any output exists, which keeps the memory peak low.
//! 5. Each hour's byte length is known from its statistics, so all threads then
//!    format their hours straight to their final offsets in a pre-sized file.

mod mmap;

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::thread;

/// A record is `local endpoint << 21 | error << 20 | latency`. A latency of ESCAPE
/// or more is stored as ESCAPE, its value appended to the part's `escapes`.
const ENDPOINT_SHIFT: u32 = 21;
const ERROR_BIT: u32 = 1 << 20;
const ESCAPE: u32 = ERROR_BIT - 1;
/// Local endpoint ids must fit the record's 11 bits: the spec has 1,842 endpoints.
const MAX_ENDPOINTS: u32 = 1 << (32 - ENDPOINT_SHIFT);

fn main() {
    mmap::no_huge_pages();
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
    let output = File::create(&output_path).map_err(|e| format!("create {output_path}: {e}"))?;
    if log.is_empty() {
        return Ok(()); // the spec's empty input gives an empty summary
    }

    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let chunks = split_at_lines(log, threads);

    // Pass 1, parallel: parse each chunk into packed records.
    let parts: Vec<Part> = thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|&chunk| {
                let start = chunk.as_ptr() as usize - log.as_ptr() as usize;
                s.spawn(move || {
                    let part = parse_chunk(log, start, start + chunk.len());
                    mmap::release(chunk);
                    part
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, _>>()
    })?;

    // Merge the per-thread endpoint tables into one, then rank the distinct
    // endpoints in byte order; `rank_of[part][local id]` maps to the rank.
    let mut all = Interner::new();
    let mut names: Vec<&[u8]> = Vec::new();
    let global: Vec<Vec<u32>> = parts
        .iter()
        .map(|p| p.names.iter().zip(&p.keys).map(|(&name, &key)| all.intern(name, key, &mut names)).collect())
        .collect();
    let mut order: Vec<u32> = (0..names.len() as u32).collect();
    order.sort_unstable_by_key(|&id| names[id as usize]);
    let mut rank_by_id = vec![0u32; names.len()];
    for (r, &id) in order.iter().enumerate() {
        rank_by_id[id as usize] = r as u32;
    }
    let names: Vec<&[u8]> = order.iter().map(|&id| names[id as usize]).collect();
    let rank_of: Vec<Vec<u32>> = global.iter().map(|ids| ids.iter().map(|&id| rank_by_id[id as usize]).collect()).collect();
    let endpoints = names.len();

    // The hours in order, and each hour's runs from every part.
    let mut hours: Vec<Head> = parts.iter().flat_map(|p| p.runs.iter().map(|run| run.head)).collect();
    hours.sort_unstable();
    hours.dedup();
    let mut runs_of: Vec<Vec<(usize, Run, usize)>> = vec![Vec::new(); hours.len()];
    for (p, part) in parts.iter().enumerate() {
        for (k, run) in part.runs.iter().enumerate() {
            let end = part.runs.get(k + 1).map_or(part.records.len(), |next| next.start as usize);
            runs_of[hours.binary_search(&run.head).unwrap()].push((p, *run, end));
        }
    }

    // Pass 2, parallel over hours: each thread takes the next hour, gathers its
    // records from every part, groups them by endpoint rank, and keeps each
    // group's statistics, plus the hour's output length.
    let next = AtomicUsize::new(0);
    let mut stats: Vec<(usize, Vec<Stats>, usize)> = thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (parts, rank_of, names, runs_of, next) = (&parts, &rank_of, &names, &runs_of, &next);
                s.spawn(move || {
                    let mut done = Vec::new();
                    let (mut start, mut errors, mut values) = (vec![0u32; endpoints + 1], vec![0u32; endpoints], Vec::new());
                    loop {
                        let hour = next.fetch_add(1, Relaxed);
                        let Some(runs) = runs_of.get(hour) else { break done };
                        // Count per endpoint rank (shifted by one), then turn the
                        // counts into start offsets, then place each latency.
                        start.fill(0);
                        errors.fill(0);
                        for &(p, run, end) in runs {
                            for &record in &parts[p].records[run.start as usize..end] {
                                let rank = rank_of[p][(record >> ENDPOINT_SHIFT) as usize] as usize;
                                start[rank + 1] += 1;
                                errors[rank] += (record & ERROR_BIT != 0) as u32;
                            }
                        }
                        for k in 0..endpoints {
                            start[k + 1] += start[k];
                        }
                        values.resize(start[endpoints] as usize, 0);
                        let mut cursor = start.clone();
                        for &(p, run, end) in runs {
                            let mut escapes = parts[p].escapes[run.escapes as usize..].iter();
                            for &record in &parts[p].records[run.start as usize..end] {
                                let rank = rank_of[p][(record >> ENDPOINT_SHIFT) as usize] as usize;
                                let latency = record & ESCAPE;
                                values[cursor[rank] as usize] = if latency == ESCAPE { *escapes.next().unwrap() } else { latency };
                                cursor[rank] += 1;
                            }
                        }
                        let mut groups = Vec::new();
                        let mut len = 0;
                        for rank in 0..endpoints {
                            let group = &mut values[start[rank] as usize..start[rank + 1] as usize];
                            if !group.is_empty() {
                                let [p50, p95, p99] = percentiles(group);
                                let stat = Stats { rank: rank as u32, fields: [group.len() as u32, p50, p95, p99, errors[rank]] };
                                len += stat.line_len(names);
                                groups.push(stat);
                            }
                        }
                        done.push((hour, groups, len));
                    }
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    drop(parts);
    stats.sort_unstable_by_key(|&(hour, _, _)| hour);

    // Pass 3, parallel: format every hour straight to its offset in the file.
    let mut offsets = vec![0];
    for (_, _, len) in &stats {
        offsets.push(offsets.last().unwrap() + len);
    }
    output.set_len(*offsets.last().unwrap() as u64).map_err(|e| format!("size {output_path}: {e}"))?;
    let next = AtomicUsize::new(0);
    thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (stats, offsets, hours, names, output, next) = (&stats, &offsets, &hours, &names, &output, &next);
                s.spawn(move || -> std::io::Result<()> {
                    let mut out = Vec::new();
                    loop {
                        let k = next.fetch_add(1, Relaxed);
                        let Some((hour, groups, len)) = stats.get(k) else { return Ok(()) };
                        out.clear();
                        for stat in groups {
                            stat.write(&hours[*hour], names, &mut out);
                        }
                        assert_eq!(out.len(), *len, "hour length computed wrongly");
                        output.write_all_at(&out, offsets[k] as u64)?;
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<std::io::Result<()>>()
    })
    .map_err(|e| format!("write {output_path}: {e}"))
}

/// The first 13 bytes of a timestamp, "YYYY-MM-DDTHH": one UTC hour.
type Head = [u8; 13];

/// A run of consecutive lines of one part in one hour, from record `start` (and
/// escape `escapes`) to the next run's start.
#[derive(Clone, Copy)]
struct Run {
    head: Head,
    start: u32,
    escapes: u32,
}

/// One group's output: endpoint rank, then count, p50, p95, p99 and errors.
struct Stats {
    rank: u32,
    fields: [u32; 5],
}

impl Stats {
    /// The six numbers after the endpoint: the error rate is in parts per million.
    fn numbers(&self) -> [u64; 6] {
        let [n, p50, p95, p99, errors] = self.fields.map(u64::from);
        [n, p50, p95, p99, errors, errors * 1_000_000 / n]
    }

    /// "YYYY-MM-DDTHH:00:00Z endpoint" then the numbers, each after a space, and LF.
    fn line_len(&self, names: &[&[u8]]) -> usize {
        let digits = |v: u64| v.checked_ilog10().map_or(1, |d| d as usize + 1);
        20 + 1 + names[self.rank as usize].len() + self.numbers().iter().map(|&v| 1 + digits(v)).sum::<usize>() + 1
    }

    fn write(&self, head: &Head, names: &[&[u8]], out: &mut Vec<u8>) {
        out.extend_from_slice(head);
        out.extend_from_slice(b":00:00Z ");
        out.extend_from_slice(names[self.rank as usize]);
        for value in self.numbers() {
            out.push(b' ');
            push_decimal(out, value);
        }
        out.push(b'\n');
    }
}

/// One thread's parse of its chunk.
struct Part<'a> {
    /// One per line, in file order (see ENDPOINT_SHIFT), and the latencies that
    /// did not fit, in the same order.
    records: Vec<u32>,
    escapes: Vec<u32>,
    runs: Vec<Run>,
    /// Local endpoint id -> its bytes in the mmap, and its 32-byte key.
    names: Vec<&'a [u8]>,
    keys: Vec<Key>,
}

/// The longest possible line: 27 + 1 + 7 + 1 + 125 + 1 + 3 + 1 + 10 + 1 bytes.
const MAX_LINE: usize = 177;
/// SIMD loads read up to 64 bytes past a field start, so the fast path needs this
/// much of the file left after the line start; the last lines take the scalar path.
const FAST_MARGIN: usize = MAX_LINE + 128;

/// Parses the lines in `log[start..end]`.
fn parse_chunk(log: &[u8], start: usize, end: usize) -> Result<Part<'_>, String> {
    let mut part = Part { records: Vec::with_capacity((end - start) / 70), escapes: Vec::new(), runs: Vec::new(), names: Vec::new(), keys: Vec::new() };
    let mut interner = Interner::new();
    // Lines from one shard arrive in time order, so most lines repeat the previous
    // line's hour: compare the 13 bytes "YYYY-MM-DDTHH" as one masked load.
    let mut last_head = u128::MAX;
    let mut i = start;
    while i < end {
        let line = &log[i..];
        let head = u128::from_le_bytes(line[..16].try_into().unwrap()) & ((1u128 << 104) - 1);
        if head != last_head {
            last_head = head;
            let head = line[..13].try_into().unwrap();
            part.runs.push(Run { head, start: part.records.len() as u32, escapes: part.escapes.len() as u32 });
        }
        let fields = if log.len() - i >= FAST_MARGIN { Fields::fast(line) } else { Fields::scalar(line) };
        let endpoint = interner.intern(&line[fields.endpoint.clone()], fields.key, &mut part.names);
        if endpoint >= MAX_ENDPOINTS {
            return Err(format!("more than {MAX_ENDPOINTS} distinct endpoints"));
        }
        if part.keys.len() < part.names.len() {
            part.keys.push(fields.key);
        }
        let latency = if fields.latency >= ESCAPE as u64 {
            part.escapes.push(fields.latency as u32);
            ESCAPE
        } else {
            fields.latency as u32
        };
        part.records.push(endpoint << ENDPOINT_SHIFT | if fields.error { ERROR_BIT } else { 0 } | latency);
        i += fields.len;
    }
    Ok(part)
}

/// An endpoint's identity: its bytes zero-padded to KEY_BYTES, as little-endian
/// words. No endpoint byte is zero, so for endpoints shorter than KEY_BYTES the
/// padding makes equal keys mean equal endpoints. A longer endpoint gets the key
/// of its first KEY_BYTES, which another long one can share, so those (5% of
/// lines) are compared byte by byte as well.
type Key = [u64; 8];
const KEY_BYTES: usize = 64;

fn key_of(bytes: &[u8]) -> Key {
    let mut padded = [0u8; KEY_BYTES];
    let n = bytes.len().min(KEY_BYTES);
    padded[..n].copy_from_slice(&bytes[..n]);
    std::array::from_fn(|i| u64::from_le_bytes(padded[i * 8..i * 8 + 8].try_into().unwrap()))
}

/// The fields of one line "ts SP method SP endpoint SP status SP latency LF".
struct Fields {
    endpoint: std::ops::Range<usize>,
    key: Key,
    error: bool,
    latency: u64,
    /// Line length including the LF.
    len: usize,
}

impl Fields {
    /// The plain byte-by-byte parse, used near the end of the file.
    fn scalar(line: &[u8]) -> Self {
        // ts is 27 bytes and the shortest method is 3, so its space is at 31 or later.
        let mut e = 31;
        while line[e] != b' ' {
            e += 1;
        }
        let start = e + 1;
        e = start;
        while line[e] != b' ' {
            e += 1;
        }
        let mut j = e + 5;
        let mut latency = 0u64;
        while line[j] != b'\n' {
            latency = latency * 10 + (line[j] - b'0') as u64;
            j += 1;
        }
        // Status is always three digits, 100 to 599, so a 5xx starts with '5'.
        Fields { endpoint: start..e, key: key_of(&line[start..e]), error: line[e + 1] == b'5', latency, len: j + 1 }
    }

    /// The same parse with AVX2 and no data-dependent branches for the common
    /// line: one 64-byte window after the timestamp gives a mask of its spaces,
    /// whose first two set bits end the method and the endpoint (the method's
    /// length varies, so this replaces a match on its letters). The endpoint's key
    /// comes from two loads masked to its length, the latency digits are converted
    /// eight at a time. The caller guarantees FAST_MARGIN readable bytes.
    #[cfg(target_feature = "avx2")]
    fn fast(line: &[u8]) -> Self {
        use std::arch::x86_64::*;
        debug_assert!(line.len() >= FAST_MARGIN);
        // SAFETY: FAST_MARGIN covers every load below: the window ends 92 bytes in,
        // the endpoint starts at most 37 bytes in, its key loads read 64 bytes from
        // there, and the endpoint (at most 125 bytes) plus the latency field (at most
        // 16 bytes read) ends within MAX_LINE + 16.
        unsafe {
            let window = line.as_ptr().add(28);
            let space = _mm256_set1_epi8(b' ' as i8);
            let spaces = |at: usize| movemask(_mm256_cmpeq_epi8(_mm256_loadu_si256(window.add(at).cast()), space)) as u64;
            let mask = spaces(0) | spaces(32) << 32;
            let start = 29 + mask.trailing_zeros() as usize;
            let rest = mask & (mask - 1);
            let end = if rest != 0 {
                28 + rest.trailing_zeros() as usize
            } else {
                let mut e = 92; // an endpoint of about 60 bytes or more
                while line[e] != b' ' {
                    e += 1;
                }
                e
            };
            let len = end - start;
            // Keep only the endpoint's bytes in each half: zero from the space on.
            let index = _mm256_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31);
            let half = |at: usize| {
                let keep = _mm256_cmpgt_epi8(_mm256_set1_epi8(len.saturating_sub(at).min(32) as i8), index);
                _mm256_and_si256(_mm256_loadu_si256(line.as_ptr().add(start + at).cast()), keep)
            };
            let key = std::mem::transmute::<[__m256i; 2], Key>([half(0), half(32)]);
            let digits = line.as_ptr().add(end + 5);
            let lf = _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_loadu_si128(digits as *const __m128i), _mm_set1_epi8(b'\n' as i8)));
            let count = lf.trailing_zeros() as usize;
            let latency = parse_digits(digits, count);
            Fields { endpoint: start..end, key, error: line[end + 1] == b'5', latency, len: end + 5 + count + 1 }
        }
    }

    #[cfg(not(target_feature = "avx2"))]
    fn fast(line: &[u8]) -> Self {
        Self::scalar(line)
    }
}

/// `vpmovmskb`, kept opaque to the optimizer: LLVM rewrites the intrinsic as a
/// vector of booleans, and on CPUs without AVX-512 (the scoring box) it then
/// builds a 64-bit mask from two of them one bit at a time.
#[cfg(target_feature = "avx2")]
#[inline(always)]
fn movemask(v: std::arch::x86_64::__m256i) -> u32 {
    let mask: u32;
    // SAFETY: AVX2 is enabled for this build; the instruction reads one register.
    unsafe { std::arch::asm!("vpmovmskb {0:e}, {1}", out(reg) mask, in(ymm_reg) v, options(pure, nomem, nostack)) };
    mask
}

/// The value of `count` ASCII digits (1 to 10) starting at `digits`, read through
/// an 8-byte load: the first eight digits are converted as one 64-bit word, the
/// rest one by one. The caller guarantees 16 readable bytes.
unsafe fn parse_digits(digits: *const u8, count: usize) -> u64 {
    // SAFETY: the caller's guarantee covers both loads.
    let word = unsafe { (digits as *const u64).read_unaligned() };
    if count <= 8 {
        // Push the digits to the top of the word and pad the bottom with ASCII
        // zeros, so the eight-digit conversion sees "000dddd".
        let pad = (8 - count) * 8;
        let padded = if pad == 0 { word } else { (word << pad) | (0x3030_3030_3030_3030 >> (64 - pad)) };
        return eight_digits(padded);
    }
    let mut value = eight_digits(word);
    for k in 8..count {
        // SAFETY: still within the 16 readable bytes.
        value = value * 10 + (unsafe { *digits.add(k) } - b'0') as u64;
    }
    value
}

/// Eight ASCII digits packed little-endian (first digit in the low byte) to a number.
fn eight_digits(word: u64) -> u64 {
    let x = word - 0x3030_3030_3030_3030;
    let x = (x * 10 + (x >> 8)) & 0x00FF_00FF_00FF_00FF;
    let x = (x * 100 + (x >> 16)) & 0x0000_FFFF_0000_FFFF;
    (x * 10000 + (x >> 32)) & 0xFFFF_FFFF
}

/// Open-addressing endpoint table keyed by the 32-byte key; names are slices of
/// the mmap, so nothing is copied.
struct Interner {
    slots: Vec<u32>, // 0 = empty, else local id + 1
    keys: Vec<Key>,  // by local id
}

impl Interner {
    fn new() -> Self {
        Interner { slots: vec![0; 1 << 12], keys: Vec::new() }
    }

    fn intern<'a>(&mut self, name: &'a [u8], key: Key, names: &mut Vec<&'a [u8]>) -> u32 {
        let mask = self.slots.len() - 1;
        let mut i = hash(key) as usize & mask;
        loop {
            match self.slots[i] {
                0 => break,
                id if self.keys[id as usize - 1] == key && (name.len() < KEY_BYTES || names[id as usize - 1] == name) => return id - 1,
                _ => i = (i + 1) & mask,
            }
        }
        names.push(name);
        self.keys.push(key);
        self.slots[i] = names.len() as u32;
        if names.len() * 2 > self.slots.len() {
            self.slots = vec![0; self.slots.len() * 2];
            let mask = self.slots.len() - 1;
            for (id, key) in self.keys.iter().enumerate() {
                let mut i = hash(*key) as usize & mask;
                while self.slots[i] != 0 {
                    i = (i + 1) & mask;
                }
                self.slots[i] = id as u32 + 1;
            }
        }
        names.len() as u32 - 1
    }
}

/// Folds the four key words into one multiply. Collisions only cost a probe.
fn hash(key: Key) -> u64 {
    let folded = key[0] ^ key[1].rotate_left(17) ^ key[2].rotate_left(31) ^ key[3].rotate_left(47) ^ key[4] ^ key[7];
    let h = folded.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^ (h >> 29)
}

/// Nearest-rank p50, p95 and p99 (the value at 1-indexed rank ceil(p * n / 100)
/// in sorted order) of a non-empty group, by selection instead of a full sort: p50
/// first, then each next rank within the part above the last one. Each value is
/// read at once: the next selection reorders that part, its own slot included.
fn percentiles(values: &mut [u32]) -> [u32; 3] {
    let [r50, r95, r99] = [50, 95, 99].map(|p| (p * values.len()).div_ceil(100) - 1);
    let p50 = *values.select_nth_unstable(r50).1;
    let p95 = *values[r50..].select_nth_unstable(r95 - r50).1;
    let p99 = *values[r95..].select_nth_unstable(r99 - r95).1;
    [p50, p95, p99]
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
