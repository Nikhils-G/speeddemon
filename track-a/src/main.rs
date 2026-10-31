//! Track A (Ingest): for every (UTC hour, endpoint) group in requests.log, the
//! request count, nearest-rank p50/p95/p99 latency and the 5xx error rate.
//!
//! How it is fast:
//! 1. The log is mmap'd, not read. The harness pre-warms it into the page cache,
//!    and pages cached by the host are not charged to our cgroup, so the 239 MB
//!    input costs nothing on the memory axis.
//! 2. One thread per hardware thread parses its own slice of the file into packed
//!    u64 records (hour, endpoint, error flag, latency). Field ends are found with
//!    AVX2 compares, digits are converted eight at a time, and endpoints are
//!    interned per thread by comparing their zero-padded 32-byte form, with no
//!    allocation per line.
//! 3. Endpoints are then ranked in byte order, so `group = hour * E + rank` is
//!    already the output order. A counting sort by group lays every group's
//!    latencies out contiguously, and each group is sorted on its own (groups are
//!    small), which gives exact percentiles without one big sort.
//! 4. Every pass between parsing and writing is parallel too, including the
//!    prefix sums over the 130,000 groups and the output, written by all threads
//!    at once into a pre-sized file: at this speed, serial glue is what shows.

mod mmap;

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::thread;

/// Hours are stored relative to the first line's hour minus this slack, in 12 bits.
/// The spec fixes a 72-hour span; anything outside +-85 days is reported as an error.
const HOUR_SLACK: u32 = 2048;
const HOUR_BITS: u32 = 12;
/// A record key is `hour << 20 | error << 19 | local endpoint`.
const KEY_BITS: u32 = 20;
const ERROR_BIT: u32 = 1 << 19;
const ENDPOINT_MASK: u32 = ERROR_BIT - 1;

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

    let base = hour_of(log) - HOUR_SLACK;
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let chunks = split_at_lines(log, threads);

    // Pass 1, parallel: parse each chunk into packed records.
    let parts: Vec<Part> = thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|&chunk| {
                let start = chunk.as_ptr() as usize - log.as_ptr() as usize;
                s.spawn(move || parse_chunk(log, start, start + chunk.len(), base))
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
    let hour_lo = parts.iter().map(|p| p.hour_lo).min().unwrap();
    let hour_hi = parts.iter().map(|p| p.hour_hi).max().unwrap();
    let endpoints = names.len();
    let groups = (hour_hi - hour_lo + 1) as usize * endpoints;
    let group_of = |part: usize, key: u32| -> usize {
        let hour = (key >> KEY_BITS) - hour_lo;
        hour as usize * endpoints + rank_of[part][(key & ENDPOINT_MASK) as usize] as usize
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
                        counts[group_of(i, (record >> 32) as u32)] += 1;
                    }
                    counts
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // Turn the histograms into write positions: group by group, part by part.
    // Only the running total over groups is serial; the rest works on disjoint
    // group ranges in parallel.
    let ranges: Vec<std::ops::Range<usize>> = (0..threads).map(|t| groups * t / threads..groups * (t + 1) / threads).collect();
    let mut group_start = vec![0u32; groups + 1];
    thread::scope(|s| {
        let slots = &slots;
        let mut rest: &mut [u32] = &mut group_start;
        for range in &ranges {
            let (mine, tail) = rest.split_at_mut(range.len());
            rest = tail;
            s.spawn(move || {
                for (total, g) in mine.iter_mut().zip(range.clone()) {
                    *total = slots.iter().map(|part| part[g]).sum();
                }
            });
        }
    });
    let mut next = 0u32;
    for total in group_start.iter_mut() {
        let count = *total;
        *total = next;
        next += count;
    }
    let shared_slots: Vec<SharedSlice> = slots.iter_mut().map(|part| SharedSlice(part.as_mut_ptr())).collect();
    thread::scope(|s| {
        let (group_start, shared_slots) = (&group_start, &shared_slots);
        for range in &ranges {
            s.spawn(move || {
                for g in range.clone() {
                    let mut offset = group_start[g];
                    for part in shared_slots {
                        // SAFETY: each thread owns a disjoint range of groups, and
                        // every part's array has `groups` entries.
                        let slot = unsafe { &mut *part.0.add(g) };
                        let count = *slot;
                        *slot = offset;
                        offset += count;
                    }
                }
            });
        }
    });

    // Pass 3, parallel: scatter each latency to its slot, counting 5xx on the way.
    // Both arrays are written by every thread at random: populated up front, so
    // the threads do not all fault the same pages in (see `mmap::populate`).
    let mut latencies = vec![0u32; next as usize];
    mmap::populate(&mut latencies);
    let mut errors: Vec<AtomicU32> = (0..groups).map(|_| AtomicU32::new(0)).collect();
    mmap::populate(&mut errors);
    let errors = &errors;
    let target = SharedSlice(latencies.as_mut_ptr());
    thread::scope(|s| {
        for (i, (part, part_slots)) in parts.iter().zip(slots.iter_mut()).enumerate() {
            let target = &target;
            s.spawn(move || {
                for &record in &part.records {
                    let key = (record >> 32) as u32;
                    let g = group_of(i, key);
                    let slot = &mut part_slots[g];
                    // SAFETY: the prefix sums above gave every (group, part) pair its
                    // own disjoint range of `latencies`, so no two threads write the
                    // same index.
                    unsafe { *target.0.add(*slot as usize) = record as u32 };
                    *slot += 1;
                    if key & ERROR_BIT != 0 {
                        errors[g].fetch_add(1, Relaxed);
                    }
                }
            });
        }
    });
    drop(slots);
    drop(parts);

    // Pass 4, parallel: sort each group and format its line. The groups are split
    // into contiguous ranges of roughly equal latency counts, so each buffer is a
    // contiguous piece of the output.
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
            let (group_start, names, hour_labels) = (&group_start, &names, &hour_labels);
            handles.push(s.spawn(move || {
                let mut out = Vec::with_capacity(len * 4);
                let offset = group_start[g0] as usize;
                for g in g0..g1 {
                    let values = &mut mine[group_start[g] as usize - offset..group_start[g + 1] as usize - offset];
                    if values.is_empty() {
                        continue;
                    }
                    values.sort_unstable();
                    let (n, errs) = (values.len() as u64, errors[g].load(Relaxed) as u64);
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

    // Every thread writes its piece at its offset into the pre-sized file.
    let total: usize = buffers.iter().map(Vec::len).sum();
    output.set_len(total as u64).map_err(|e| format!("size {output_path}: {e}"))?;
    let mut offsets = Vec::with_capacity(buffers.len());
    let mut at = 0u64;
    for buffer in &buffers {
        offsets.push(at);
        at += buffer.len() as u64;
    }
    thread::scope(|s| {
        let handles: Vec<_> = buffers
            .iter()
            .zip(&offsets)
            .map(|(buffer, &offset)| {
                let output = &output;
                s.spawn(move || output.write_all_at(buffer, offset))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<std::io::Result<()>>()
    })
    .map_err(|e| format!("write {output_path}: {e}"))
}

struct SharedSlice(*mut u32);
// SAFETY: only used for the disjoint writes documented at each use.
unsafe impl Sync for SharedSlice {}

/// One thread's parse of its chunk.
struct Part<'a> {
    /// `key << 32 | latency` (see KEY_BITS), hour relative to `base`.
    records: Vec<u64>,
    /// Local endpoint id -> its bytes in the mmap, and its 32-byte key.
    names: Vec<&'a [u8]>,
    keys: Vec<Key>,
    hour_lo: u32,
    hour_hi: u32,
}

/// The longest possible line: 27 + 1 + 7 + 1 + 125 + 1 + 3 + 1 + 10 + 1 bytes.
const MAX_LINE: usize = 177;
/// SIMD loads read up to 32 bytes past a field start, so the fast path needs this
/// much of the file left after the line start; the last lines take the scalar path.
const FAST_MARGIN: usize = MAX_LINE + 64;

/// Parses the lines in `log[start..end]`.
fn parse_chunk(log: &[u8], start: usize, end: usize, base: u32) -> Result<Part<'_>, String> {
    let mut part = Part { records: Vec::with_capacity((end - start) / 70), names: Vec::new(), keys: Vec::new(), hour_lo: u32::MAX, hour_hi: 0 };
    let mut interner = Interner::new();
    // Lines from one shard arrive in time order, so most lines repeat the previous
    // line's hour: compare the 13 bytes "YYYY-MM-DDTHH" instead of reparsing them.
    let mut last_head = 0u128;
    let mut hour = 0u32;
    let mut i = start;
    while i < end {
        let line = &log[i..];
        let head = u128::from_le_bytes(line[..16].try_into().unwrap()) & ((1u128 << 104) - 1);
        if head != last_head {
            last_head = head;
            let absolute = hour_of(line);
            if absolute < base || absolute - base >= 1 << HOUR_BITS {
                return Err(format!("timestamp {} is too far from the first line's", String::from_utf8_lossy(&line[..27])));
            }
            hour = absolute - base;
            part.hour_lo = part.hour_lo.min(hour);
            part.hour_hi = part.hour_hi.max(hour);
        }
        let fields = if log.len() - i >= FAST_MARGIN { Fields::fast(line) } else { Fields::scalar(line) };
        let endpoint = interner.intern(&line[fields.endpoint.clone()], fields.key, &mut part.names);
        if endpoint >= ERROR_BIT {
            return Err("more endpoints than the record layout holds".into());
        }
        if part.keys.len() < part.names.len() {
            part.keys.push(fields.key);
        }
        let key = hour << KEY_BITS | if fields.error { ERROR_BIT } else { 0 } | endpoint;
        part.records.push((key as u64) << 32 | fields.latency);
        i += fields.len;
    }
    Ok(part)
}

/// An endpoint's identity: its bytes zero-padded to 32, as four little-endian
/// words. No endpoint byte is zero, so for endpoints shorter than 32 bytes the
/// padding makes equal keys mean equal endpoints. An endpoint of 32 bytes or more
/// gets the key of its first 32 bytes, which a longer endpoint can share, so
/// those are compared byte by byte as well.
type Key = [u64; 4];

fn key_of(bytes: &[u8]) -> Key {
    let mut padded = [0u8; 32];
    let n = bytes.len().min(32);
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

    /// The same parse with AVX2: the endpoint's closing space and the latency's LF
    /// are found 32 and 16 bytes at a time, the endpoint's key comes straight from
    /// the register it was loaded into, and the latency digits are converted eight
    /// at a time. The caller guarantees FAST_MARGIN readable bytes from `line`.
    #[cfg(target_feature = "avx2")]
    fn fast(line: &[u8]) -> Self {
        use std::arch::x86_64::*;
        debug_assert!(line.len() >= FAST_MARGIN);
        // The method is one of seven fixed words; its first letters give its length.
        let method = match (line[28], line[29]) {
            (b'G', _) | (b'P', b'U') => 3, // GET, PUT
            (b'P', b'O') | (b'H', _) => 4, // POST, HEAD
            (b'P', b'A') => 5,             // PATCH
            (b'D', _) => 6,                // DELETE
            _ => 7,                        // OPTIONS
        };
        if line[28 + method] != b' ' {
            return Self::scalar(line); // not one of the seven: let the scalar parse decide
        }
        let start = 29 + method;
        // SAFETY: FAST_MARGIN covers every load below: the endpoint starts at most 37
        // bytes in, is at most 125 bytes, and the latency field at most 11 more.
        unsafe {
            let ptr = line.as_ptr().add(start);
            let head = _mm256_loadu_si256(ptr as *const __m256i);
            let spaces = _mm256_movemask_epi8(_mm256_cmpeq_epi8(head, _mm256_set1_epi8(b' ' as i8))) as u32;
            let (len, key) = if spaces != 0 {
                let len = spaces.trailing_zeros() as usize;
                // Keep only the endpoint's bytes: zero everything from the space on.
                let index = _mm256_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31);
                let keep = _mm256_cmpgt_epi8(_mm256_set1_epi8(len as i8), index);
                (len, std::mem::transmute::<__m256i, Key>(_mm256_and_si256(head, keep)))
            } else {
                let mut e = start + 32;
                while line[e] != b' ' {
                    e += 1;
                }
                (e - start, std::mem::transmute::<__m256i, Key>(head))
            };
            let end = start + len;
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
                id if self.keys[id as usize - 1] == key && (name.len() < 32 || names[id as usize - 1] == name) => return id - 1,
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
    let folded = key[0] ^ key[1].rotate_left(17) ^ key[2].rotate_left(31) ^ key[3].rotate_left(47);
    let h = folded.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^ (h >> 29)
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
