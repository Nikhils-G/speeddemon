//! Track A (Ingest): for every (UTC hour, endpoint) group in requests.log, the
//! request count, nearest-rank p50/p95/p99 latency and the 5xx error rate.
//!
//! How it is fast:
//! 1. The log is mmap'd, not read. The harness pre-warms it into the page cache,
//!    and pages cached by the host are not charged to our cgroup, so the 239 MB
//!    input costs nothing on the memory axis.
//! 2. One thread per hardware thread parses its own slice of the file into packed
//!    u32 records (endpoint, error flag, latency; the 1% of latencies that do not
//!    fit go to a side list). Field ends come from one AVX2 space mask, digits are
//!    converted by multiply-adds, and endpoints are interned in one table shared by
//!    the threads, by their zero-padded 64-byte form, with no allocation or
//!    capacity check per line.
//! 3. Records carry no hour: shards write in time order, so the file is made of
//!    runs of lines in one hour (about 1,000), and each part keeps a list of its
//!    runs, keyed by the timestamp's first 13 bytes "YYYY-MM-DDTHH". Sorting those
//!    bytes sorts the hours, and they are the start of the output's hour label, so
//!    no calendar arithmetic is needed and any amount of disorder is handled.
//! 4. Endpoints are sorted in byte order, and the threads take the hours in order:
//!    a thread gathers one hour's runs from every part, groups them by endpoint
//!    with a counting sort small enough to stay in L1/L2, gives back the records
//!    it no longer needs, and selects exact percentiles per group.
//! 5. Each hour is formatted straight into the output file, mapped shared, as soon
//!    as the hours before it have their lengths; a thread takes further hours
//!    meanwhile. Records shrink as the output grows.

mod mmap;

#[cfg(not(target_feature = "avx2"))]
compile_error!("track-a needs AVX2: build with RUSTFLAGS=\"-C target-cpu=native\"");

use std::arch::x86_64::*;
use std::cell::UnsafeCell;
use std::fs::File;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering::*};
use std::thread;

/// A record is `endpoint id << 21 | error << 20 | latency`. A latency of ESCAPE or
/// more is stored as ESCAPE, its value appended to the part's `escapes`.
const ENDPOINT_SHIFT: u32 = 21;
const ESCAPE: u32 = (1 << 20) - 1;
/// Endpoint ids must fit the record's 11 bits: the spec has 1,842 endpoints.
const MAX_ENDPOINTS: usize = 1 << (32 - ENDPOINT_SHIFT);
/// An hour's start or length that is not known yet.
const UNKNOWN: usize = usize::MAX;

fn main() {
    mmap::no_huge_pages();
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    let input = File::open(format!("{data_dir}/requests.log")).expect("open the log");
    let padded = mmap::map(&input);
    let log = &padded[..padded.len() - mmap::PAD];
    // Read and write: the output is mapped shared and written through memory.
    let mut options = File::options();
    options.read(true).write(true).create(true).truncate(true);
    let output = options.open(format!("{out_dir}/summary.txt"));
    let output = output.expect("create the summary");
    if log.is_empty() {
        return; // the spec's empty input gives an empty summary
    }

    // Pass 1, parallel: parse each chunk into packed records, with one endpoint
    // table shared by all threads.
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let table = &Interner::new();
    let chunks = split_at_lines(log, threads);
    let parts: Vec<Part> = thread::scope(|s| {
        let spawn = |&chunk| s.spawn(move || parse_chunk(chunk, table));
        let handles: Vec<_> = chunks.iter().map(spawn).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // The endpoint ids in byte order of their names.
    let endpoints = table.count.load(Relaxed);
    let names: Vec<&[u8]> = (0..endpoints).map(|id| table.name(id)).collect();
    // First by the first 16 bytes as one number (zero-padded: a prefix sorts first),
    // so few comparisons need the bytes themselves.
    let prefixes: Vec<u128> = names.iter().map(|name| number(name)).collect();
    let mut order: Vec<usize> = (0..endpoints).collect();
    order.sort_unstable_by_key(|&id| (prefixes[id], names[id]));

    // The hours in order, and each hour's runs from every part.
    let all_runs = parts.iter().flat_map(|p| &p.runs);
    let mut hours: Vec<Head> = all_runs.map(|r| r.head).collect();
    hours.sort_unstable_by_key(|head| number(head));
    hours.dedup();
    // Also how many hours still read each part's records: the hour that brings
    // its part's count to zero gives the rest of the records back. Runs of one
    // part are adjacent in an hour's list.
    let mut runs_of: Vec<Vec<(usize, Run, usize)>> = vec![Vec::new(); hours.len()];
    let readers: Vec<AtomicUsize> = parts.iter().map(|_| AtomicUsize::new(0)).collect();
    for (p, part) in parts.iter().enumerate() {
        for (k, run) in part.runs.iter().enumerate() {
            let next = part.runs.get(k + 1);
            let end = next.map_or(part.records.len(), |r| r.start as usize);
            let hour = hours.binary_search_by_key(&number(&run.head), |h| number(h));
            let runs = &mut runs_of[hour.unwrap()];
            if runs.last().is_none_or(|r| r.0 != p) {
                readers[p].fetch_add(1, Relaxed);
            }
            runs.push((p, *run, end));
        }
    }

    // The output is mapped shared at a size no output can exceed (each group's
    // line is at most `longest` bytes) and cut to its length at the end. On tmpfs,
    // where /out is, one write call per hour would serialise on the file's lock;
    // page faults on the mapping do not.
    let longest = 21 + names.iter().map(|n| n.len()).max().unwrap() + 6 * 11 + 1;
    let lines: usize = parts.iter().map(|p| p.records.len()).sum();
    let bound = (hours.len() * endpoints).min(lines) * longest;
    output.set_len(bound as u64).expect("size the summary");
    let file = mmap::map_mut(&output, bound) as usize; // each hour its own bytes
    // ends[h] is where hour h's output starts once known: it is filled in from
    // the hours' lengths, in order, up to `known`.
    let unknown = |_| AtomicUsize::new(UNKNOWN);
    let ends: Vec<AtomicUsize> = (0..=hours.len()).map(unknown).collect();
    let lens: Vec<AtomicUsize> = (0..hours.len()).map(unknown).collect();
    ends[0].store(0, Relaxed);
    let (known, next) = (AtomicUsize::new(0), AtomicUsize::new(0));

    // Pass 2, parallel over hours: each thread takes the next hour, gathers its
    // records from every part, groups them by endpoint id, computes each group's
    // statistics and the hour's length, then writes each of its hours once the
    // previous one's end is known, taking further hours meanwhile.
    // Writes the pending hours whose start is known; with `wait`, all of them.
    let flush = |pending: &mut Vec<(usize, Vec<Stats>)>, wait: bool| {
        while !pending.is_empty() {
            let mut h = known.load(Acquire);
            while h < hours.len() && lens[h].load(Acquire) != UNKNOWN {
                let end = ends[h].load(Acquire) + lens[h].load(Acquire);
                ends[h + 1].store(end, Release);
                h += 1;
            }
            known.fetch_max(h, AcqRel);
            pending.retain(|(hour, groups)| {
                let at = ends[*hour].load(Acquire);
                if at == UNKNOWN {
                    return true;
                }
                let len = lens[*hour].load(Relaxed);
                // SAFETY: bytes at..at + len of the mapping are this hour's
                // alone, and at + len <= bound: every line is at most
                // `longest` bytes.
                let out = unsafe { std::slice::from_raw_parts_mut((file + at) as _, len) };
                let mut written = 0;
                for stat in groups {
                    let name = names[stat[0] as usize];
                    written += write(&hours[*hour], name, stat, &mut out[written..]);
                }
                assert_eq!(written, len, "hour length");
                false
            });
            if !wait {
                break;
            }
            thread::yield_now();
        }
    };
    thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let (mut start, mut counts) = (vec![0u32; endpoints + 1], vec![0u64; endpoints]);
                let (mut values, mut pending) = (Vec::new(), Vec::new());
                loop {
                    let hour = next.fetch_add(1, Relaxed);
                    let Some(runs) = runs_of.get(hour) else {
                        break flush(&mut pending, true);
                    };
                    // Count per endpoint id, errors in the high half of the same
                    // word (one increment per record), then set start[i + 1] to the
                    // start of id i's group and place each latency at it: start[i]
                    // then runs from id i's first value to its end. The groups are
                    // then taken in byte order of their names.
                    counts.fill(0);
                    for &(p, run, end) in runs {
                        for &record in &parts[p].records[run.start as usize..end] {
                            let id = (record >> ENDPOINT_SHIFT) as usize;
                            counts[id] += 1 | ((record >> 20 & 1) as u64) << 32;
                        }
                    }
                    let mut sum = 0;
                    for k in 0..endpoints {
                        start[k + 1] = sum;
                        sum += counts[k] as u32;
                    }
                    values.resize(sum as usize, 0);
                    let (values, start) = (&mut values[..], &mut start[..]);
                    for &(p, run, end) in runs {
                        let part = &parts[p];
                        let mut escapes = part.escapes[run.escapes as usize..].iter();
                        for &record in &part.records[run.start as usize..end] {
                            let latency = match record & ESCAPE {
                                ESCAPE => *escapes.next().unwrap(),
                                latency => latency,
                            };
                            // SAFETY: ids are below `endpoints`, and each id's cursor stays
                            // below the next id's start, which is at most values.len():
                            // the counts above are this hour's.
                            unsafe {
                                let id = (record >> ENDPOINT_SHIFT) as usize;
                                let at = start.get_unchecked_mut(id + 1);
                                *values.get_unchecked_mut(*at as usize) = latency;
                                *at += 1;
                            }
                        }
                    }
                    // The records are no longer needed once placed.
                    for (k, &(p, run, end)) in runs.iter().enumerate() {
                        // Long runs (shard 0's hourly uploads: 142 runs, 93% of the
                        // records) at once, the rest with their part.
                        if end - run.start as usize >= 4096 {
                            mmap::release(&parts[p].records[run.start as usize..end]);
                        }
                        let last = k == 0 || runs[k - 1].0 != p;
                        if last && readers[p].fetch_sub(1, AcqRel) == 1 {
                            mmap::release(&parts[p].records);
                        }
                    }
                    let (mut groups, mut len) = (Vec::with_capacity(endpoints), 0);
                    for &id in &order {
                        let group = &mut values[start[id] as usize..start[id + 1] as usize];
                        if !group.is_empty() {
                            let [p50, p95, p99] = percentiles(group);
                            let errors = (counts[id] >> 32) as u32;
                            let stat = [id as u32, group.len() as u32, p50, p95, p99, errors];
                            len += line_len(names[id], &stat);
                            groups.push(stat);
                        }
                    }
                    lens[hour].store(len, Release);
                    pending.push((hour, groups));
                    flush(&mut pending, false);
                }
            });
        }
    });
    let total = ends[hours.len()].load(Relaxed);
    output.set_len(total as u64).expect("size the summary");
    // The output is complete in the page cache. Exit now: dropping the tables
    // above, one free at a time, took a few milliseconds on the scoring box.
    std::process::exit(0)
}

/// The first 13 bytes of a timestamp, "YYYY-MM-DDTHH": one UTC hour.
type Head = [u8; 13];

/// Up to the first 16 bytes, zero-padded, as a number that sorts like them.
fn number(bytes: &[u8]) -> u128 {
    let mut padded = [0; 16];
    let n = bytes.len().min(16);
    padded[..n].copy_from_slice(&bytes[..n]);
    u128::from_be_bytes(padded)
}

/// A run of consecutive lines of one part in one hour, from record `start` (and
/// escape `escapes`) to the next run's start.
#[derive(Clone, Copy)]
struct Run {
    head: Head,
    start: u32,
    escapes: u32,
}

/// One group's output: endpoint id, count, p50, p95, p99 and errors.
type Stats = [u32; 6];

/// The six numbers after the endpoint: the error rate is in parts per million.
fn numbers(stat: &Stats) -> [u64; 6] {
    let [_, n, p50, p95, p99, errors] = stat.map(u64::from);
    [n, p50, p95, p99, errors, errors * 1_000_000 / n]
}

/// "YYYY-MM-DDTHH:00:00Z endpoint" then the numbers, each after a space, and LF.
fn line_len(name: &[u8], stat: &Stats) -> usize {
    let digits = |v: u64| v.checked_ilog10().map_or(1, |d| d as usize + 1);
    21 + name.len() + numbers(stat).iter().map(|&v| 1 + digits(v)).sum::<usize>() + 1
}

/// The line is put together from its end in a stack buffer, so no number's
/// length is needed first, then copied to the start of `out`; returns its length.
fn write(head: &Head, name: &[u8], stat: &Stats, out: &mut [u8]) -> usize {
    let mut line = [0u8; 256]; // 21 + 125 + 6 * 11 + 1 = 213 at most
    let mut at = line.len() - 1;
    line[at] = b'\n';
    for value in numbers(stat).into_iter().rev() {
        at = put_decimal(&mut line, at, value) - 1;
        line[at] = b' ';
    }
    at -= 21 + name.len();
    line[at..at + 13].copy_from_slice(head);
    line[at + 13..at + 21].copy_from_slice(b":00:00Z ");
    line[at + 21..at + 21 + name.len()].copy_from_slice(name);
    out[..line.len() - at].copy_from_slice(&line[at..]);
    line.len() - at
}

/// Writes `value` in decimal to end just before `line[end]`, two digits at a
/// time, and returns where it starts.
fn put_decimal(line: &mut [u8], mut end: usize, mut value: u64) -> usize {
    while value >= 100 {
        let pair = (value % 100) as usize * 2;
        value /= 100;
        end -= 2;
        line[end..end + 2].copy_from_slice(&PAIRS[pair..pair + 2]);
    }
    if value >= 10 {
        end -= 2;
        line[end..end + 2].copy_from_slice(&PAIRS[value as usize * 2..value as usize * 2 + 2]);
    } else {
        end -= 1;
        line[end] = b'0' + value as u8;
    }
    end
}

/// "00", "01", ..., "99".
static PAIRS: [u8; 200] = {
    let mut pairs = [0; 200];
    let mut k = 0;
    while k < 100 {
        pairs[2 * k] = b'0' + (k / 10) as u8;
        pairs[2 * k + 1] = b'0' + (k % 10) as u8;
        k += 1;
    }
    pairs
};

/// One thread's parse of its chunk.
struct Part {
    /// One per line, in file order (see ENDPOINT_SHIFT), and the latencies that
    /// did not fit, in the same order.
    records: Vec<u32>,
    escapes: Vec<u32>,
    runs: Vec<Run>,
}

/// The shortest possible line: timestamp, "GET", "/", status, one digit, spaces, LF.
const MIN_LINE: usize = 27 + 1 + 3 + 1 + 1 + 1 + 3 + 1 + 1 + 1;

/// Parses the lines of `chunk`, a slice of the mapped log, which goes on with
/// mmap::PAD zero bytes, then gives the chunk's pages back. Every line is
/// "ts SP method SP endpoint SP status SP latency LF" (27-byte ts).
fn parse_chunk(chunk: &'static [u8], table: &Interner) -> Part {
    // Sized for the worst case, so the loop writes without capacity checks; only
    // the pages actually written are ever backed by memory.
    let most = chunk.len() / MIN_LINE + 1;
    let (mut records, mut escapes) = (Vec::with_capacity(most), Vec::with_capacity(most));
    let mut runs = Vec::new();
    // SAFETY: every load stays within the line (at most 177 bytes) and the mmap::PAD
    // zero bytes after the log: the space windows end at most 188 bytes in, the endpoint
    // starts at most 37 bytes in and its key loads read 64 bytes from there, and the
    // latency load reads 16 bytes from the status's end. Records and escapes are
    // written below their capacity: there are at most `most` lines.
    unsafe {
        let space = _mm256_set1_epi8(b' ' as i8);
        let (first_record, first_escape): (*mut u32, *mut u32) =
            (records.as_mut_ptr(), escapes.as_mut_ptr());
        let (mut record, mut escape) = (first_record, first_escape);
        let (mut line, stop) = (chunk.as_ptr(), chunk.as_ptr_range().end);
        // Lines from one shard arrive in time order, so most lines repeat the
        // previous line's hour. Its bytes "MM-DDTHH" decide: every timestamp lies in
        // one 72-hour span (the spec), so lines that share them share the year.
        let mut last = u64::MAX;
        while line < stop {
            let hour = line.add(5).cast::<u64>().read_unaligned();
            if hour != last {
                last = hour;
                runs.push(Run {
                    head: *line.cast::<Head>(),
                    start: record.offset_from(first_record) as u32,
                    escapes: escape.offset_from(first_escape) as u32,
                });
            }
            // One 64-byte window after the timestamp: its first two spaces end the
            // method and the endpoint (no branch on the method or the length).
            let load = |at: usize| _mm256_loadu_si256(line.add(at).cast());
            let spaces = |at: usize| movemask(_mm256_cmpeq_epi8(load(28 + at), space)) as u64;
            let mask = spaces(0) | spaces(32) << 32;
            let from = 29 + mask.trailing_zeros() as usize;
            let rest = mask & (mask - 1);
            let to = if rest != 0 {
                28 + rest.trailing_zeros() as usize
            } else {
                let mut at = 64; // an endpoint of about 60 bytes or more: on by 32
                while spaces(at) == 0 {
                    at += 32;
                }
                28 + at + spaces(at).trailing_zeros() as usize
            };
            let name = std::slice::from_raw_parts(line.add(from), to - from);
            let endpoint = table.id(&key_of(name), name);
            // The latency: up to 10 digits from 5 bytes after the endpoint, to the LF.
            let digits = _mm_loadu_si128(line.add(to + 5).cast());
            let count = _mm_movemask_epi8(_mm_cmpeq_epi8(digits, _mm_set1_epi8(b'\n' as i8)))
                .trailing_zeros() as usize;
            let latency = parse_digits(digits, count);
            *escape = latency as u32;
            escape = escape.add((latency >= ESCAPE as u64) as usize);
            let error = ((*line.add(to + 1) == b'5') as u32) << 20;
            *record = endpoint << ENDPOINT_SHIFT | error | latency.min(ESCAPE as u64) as u32;
            record = record.add(1);
            line = line.add(to + 5 + count + 1);
        }
        records.set_len(record.offset_from(first_record) as usize);
        escapes.set_len(escape.offset_from(first_escape) as usize);
    }
    mmap::release(chunk);
    Part {
        records,
        escapes,
        runs,
    }
}

/// The key of the endpoint `name`: its first 64 bytes, zero from its end on.
/// `name` must be followed by 64 readable bytes.
#[inline(always)]
fn key_of(name: &[u8]) -> Key {
    // SAFETY: AVX2 is enabled; the caller guarantees the bytes after `name`.
    unsafe {
        let keep = KEEP.as_ptr().cast::<u8>().add(64 - name.len().min(64));
        let half = |at: usize| {
            let bytes = _mm256_loadu_si256(name.as_ptr().add(at).cast());
            _mm256_and_si256(bytes, _mm256_loadu_si256(keep.add(at).cast()))
        };
        [half(0), half(32)]
    }
}

/// 64 bytes of 0xFF then 64 zeros: the 64 bytes from `64 - len` keep the first `len`.
static KEEP: [[u8; 64]; 2] = [[0xFF; 64], [0; 64]];

/// An endpoint's identity: its bytes zero-padded to 64. No endpoint byte is zero,
/// so for endpoints shorter than 64 bytes equal keys mean equal endpoints. A longer
/// endpoint gets the key of its first 64 bytes, which another long one can share,
/// so those (5% of lines) are compared byte by byte as well.
type Key = [__m256i; 2];
const KEY_BYTES: usize = 64;

/// `vpmovmskb`, kept opaque to the optimizer: LLVM rewrites the intrinsic as a
/// vector of booleans, and on CPUs without AVX-512 (the scoring box) it then
/// builds a 64-bit mask from two of them one bit at a time.
#[inline(always)]
fn movemask(v: __m256i) -> u32 {
    let mask: u32;
    // SAFETY: AVX2 is enabled for this build; the instruction reads one register.
    unsafe {
        std::arch::asm!("vpmovmskb {0:e}, {1}", out(reg) mask, in(ymm_reg) v, options(pure, nomem, nostack))
    };
    mask
}

/// k - 16 at k: the 16 bytes from `count` are the shuffle that right-aligns `count`
/// digits.
static SHIFT: [i8; 32] = [
    -16, -15, -14, -13, -12, -11, -10, -9, -8, -7, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7,
    8, 9, 10, 11, 12, 13, 14, 15,
];

/// The value of the `count` (1 to 10) ASCII digits at the start of `digits`: they
/// are moved to the end of the vector behind zeros, then combined pairwise by
/// multiply-adds into two eight-digit halves.
#[inline(always)]
fn parse_digits(digits: std::arch::x86_64::__m128i, count: usize) -> u64 {
    use std::arch::x86_64::*;
    // SAFETY: SSSE3 and SSE4.1 come with AVX2.
    unsafe {
        // Byte j takes digit j + count - 16; a negative index makes it zero.
        let order = _mm_loadu_si128(SHIFT.as_ptr().add(count).cast());
        let v = _mm_shuffle_epi8(_mm_sub_epi8(digits, _mm_set1_epi8(b'0' as i8)), order);
        let v = _mm_maddubs_epi16(v, _mm_set1_epi16(0x010A)); // pairs: 10 * a + b
        let v = _mm_madd_epi16(v, _mm_set1_epi32(0x0001_0064)); // fours: 100 * a + b
        let v = _mm_packus_epi32(v, v);
        let v = _mm_madd_epi16(v, _mm_set1_epi32(0x0001_2710)); // eights: 10000 * a + b
        let both = _mm_cvtsi128_si64(v) as u64;
        (both & 0xFFFF_FFFF) * 100_000_000 + (both >> 32)
    }
}

/// Open-addressing endpoint table keyed by the 64-byte key, shared by the parsing
/// threads: lookups are plain loads, an insert claims its slot by compare-and-swap.
/// Fixed at SLOTS: ids must fit 11 bits anyway.
struct Interner {
    slots: Box<[AtomicU16]>, // 0 = empty, else id + 1
    keys: Box<[UnsafeCell<Key>]>,
    names: Box<[UnsafeCell<&'static [u8]>]>,
    count: AtomicUsize,
}

// SAFETY: an id's key and name are written once, before a slot publishes the id
// (Release), and read only after a slot holding it is seen (Acquire).
unsafe impl Sync for Interner {}

const SLOTS: usize = 8 * MAX_ENDPOINTS;

impl Interner {
    fn new() -> Self {
        // SAFETY: a zero key is a valid placeholder.
        let zero = || UnsafeCell::new(unsafe { std::mem::zeroed() });
        Interner {
            slots: (0..SLOTS).map(|_| AtomicU16::new(0)).collect(),
            keys: (0..MAX_ENDPOINTS).map(|_| zero()).collect(),
            names: (0..MAX_ENDPOINTS).map(|_| Default::default()).collect(),
            count: AtomicUsize::new(0),
        }
    }

    /// The key of id `id`.
    /// SAFETY: `id` must be published, or parsing over.
    #[inline(always)]
    unsafe fn key(&self, id: usize) -> &Key {
        unsafe { &*self.keys.get_unchecked(id).get() }
    }

    /// The id of a published slot's endpoint if it is this one.
    #[inline(always)]
    fn matches(&self, slot: u16, key: &Key, name: &[u8]) -> Option<u32> {
        let id = slot as usize - 1;
        // SAFETY: a published id's key and name are written and never change.
        let (k, n) = unsafe { (self.key(id), *self.names.get_unchecked(id).get()) };
        (same(k, key) && (name.len() < KEY_BYTES || n == name)).then_some(id as u32)
    }

    /// The endpoint's id, added if new.
    #[inline(always)]
    fn id(&self, key: &Key, name: &'static [u8]) -> u32 {
        let mut i = hash(key);
        loop {
            // SAFETY: i < SLOTS.
            let slot = unsafe { self.slots.get_unchecked(i) }.load(Acquire);
            if slot == 0 {
                return self.insert(i, name); // rebuilds the key: none is spilled here
            }
            if let Some(id) = self.matches(slot, key, name) {
                return id;
            }
            i = (i + 1) % SLOTS;
        }
    }

    /// Adds an endpoint not found up to the empty slot `i`. If another thread
    /// fills that slot first with the same endpoint, its id wins and the one taken
    /// here stays unused (a duplicate name that no record refers to).
    #[cold]
    #[inline(never)]
    fn insert(&self, mut i: usize, name: &'static [u8]) -> u32 {
        let key = &key_of(name);
        let id = self.count.fetch_add(1, Relaxed);
        assert!(id < MAX_ENDPOINTS, "too many distinct endpoints");
        // A long name is also compared byte by byte later, after the input's pages
        // are released: copy it while they are still in place.
        let long = name.len() >= KEY_BYTES;
        let name = if long { Box::leak(name.into()) } else { name };
        // SAFETY: id is this thread's alone until a slot publishes it.
        unsafe { (*self.keys[id].get(), *self.names[id].get()) = (*key, name) };
        loop {
            match self.slots[i].compare_exchange(0, id as u16 + 1, Release, Acquire) {
                Ok(_) => return id as u32,
                Err(slot) => {
                    if let Some(other) = self.matches(slot, key, name) {
                        return other;
                    }
                }
            }
            i = (i + 1) % SLOTS;
        }
    }

    /// The bytes of endpoint `id`. One shorter than KEY_BYTES is read from its key,
    /// so the input's released pages do not fault back in to sort and print it.
    fn name(&self, id: usize) -> &[u8] {
        // SAFETY: called once parsing is over; a Key is 64 plain bytes.
        let (key, name) = unsafe { (self.key(id), *self.names[id].get()) };
        let bytes = unsafe { &*(key as *const Key).cast::<[u8; KEY_BYTES]>() };
        let long = name.len() >= KEY_BYTES;
        if long { name } else { &bytes[..name.len()] }
    }
}

/// Key equality as two vector compares.
#[inline(always)]
fn same(a: &Key, b: &Key) -> bool {
    use std::arch::x86_64::*;
    // SAFETY: AVX2 is enabled for this build.
    unsafe {
        let diff = _mm256_or_si256(_mm256_xor_si256(a[0], b[0]), _mm256_xor_si256(a[1], b[1]));
        _mm256_testz_si256(diff, diff) != 0
    }
}

/// The key folded to 16 bytes, then two CRC steps: the slot to probe first.
/// Collisions only cost a probe.
#[inline(always)]
fn hash(key: &Key) -> usize {
    use std::arch::x86_64::*;
    // SAFETY: AVX2 and SSE4.2 are enabled for this build.
    unsafe {
        let x = _mm256_xor_si256(key[0], key[1]);
        let x = _mm_xor_si128(_mm256_castsi256_si128(x), _mm256_extracti128_si256(x, 1));
        let h = _mm_crc32_u64(
            _mm_crc32_u64(0, _mm_cvtsi128_si64(x) as u64),
            _mm_extract_epi64(x, 1) as u64,
        );
        h as usize % SLOTS
    }
}

/// Nearest-rank p50, p95 and p99 (the value at 1-indexed rank ceil(p * n / 100)
/// in sorted order) of a non-empty group, by selection instead of a full sort: p50
/// first, then each next rank within the part above the last one. Each value is
/// read at once: the next selection reorders that part, its own slot included.
fn percentiles(values: &mut [u32]) -> [u32; 3] {
    let [r50, r95, r99] = [50, 95, 99].map(|p| (p * values.len()).div_ceil(100) - 1);
    // Most groups are small: one insertion-sort pass beats three selections.
    if values.len() <= 32 {
        values.sort_unstable();
        return [r50, r95, r99].map(|r| values[r]);
    }
    let p50 = *values.select_nth_unstable(r50).1;
    let p95 = *values[r50..].select_nth_unstable(r95 - r50).1;
    let p99 = *values[r95..].select_nth_unstable(r99 - r95).1;
    [p50, p95, p99]
}

/// Splits `data` into about `parts` slices, each ending just after a newline.
fn split_at_lines(data: &[u8], parts: usize) -> Vec<&[u8]> {
    let cut = |p: usize| {
        let at = data.len() * p / parts;
        let lf = data[at..].iter().position(|&b| b == b'\n');
        if p == 0 {
            0
        } else {
            lf.map_or(data.len(), |i| at + i + 1)
        }
    };
    let ends: Vec<usize> = (0..=parts).map(cut).collect();
    let chunks = ends.windows(2).filter(|w| w[1] > w[0]);
    chunks.map(|w| &data[w[0]..w[1]]).collect()
}
