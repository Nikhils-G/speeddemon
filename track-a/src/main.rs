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
//!    converted by multiply-adds, and endpoints are interned per thread by their
//!    zero-padded 64-byte form, with no allocation or capacity check per line.
//! 3. Records carry no hour: shards write in time order, so the file is made of
//!    runs of lines in one hour (about 1,000), and each part keeps a list of its
//!    runs, keyed by the timestamp's first 13 bytes "YYYY-MM-DDTHH". Sorting those
//!    bytes sorts the hours, and they are the start of the output's hour label, so
//!    no calendar arithmetic is needed and any amount of disorder is handled.
//! 4. Endpoints are ranked in byte order, and the threads take the hours in order:
//!    a thread gathers one hour's runs from every part, groups them by endpoint
//!    with a counting sort small enough to stay in L1/L2, gives back the records
//!    it no longer needs, and selects exact percentiles per group.
//! 5. As soon as the previous hour's end offset is known, the hour is formatted
//!    straight into the output file, mapped shared; records shrink as it grows.

mod mmap;

#[cfg(not(target_feature = "avx2"))]
compile_error!("track-a needs AVX2: build with RUSTFLAGS=\"-C target-cpu=native\"");

use std::cell::UnsafeCell;
use std::fs::{File, OpenOptions};
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering::*};
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
    let padded = mmap::map(&input).map_err(|e| format!("mmap {input_path}: {e}"))?;
    let log = &padded[..padded.len() - mmap::PAD];
    let output_path = format!("{out_dir}/summary.txt");
    // Read and write: the output is mapped shared and written through memory.
    let output = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&output_path);
    let output = output.map_err(|e| format!("create {output_path}: {e}"))?;
    if log.is_empty() {
        return Ok(()); // the spec's empty input gives an empty summary
    }

    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let chunks = split_at_lines(log, threads);

    // Pass 1, parallel: parse each chunk into packed records, with one endpoint
    // table shared by all threads.
    let table = &Interner::new();
    let parts: Vec<Part> = thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .map(|&chunk| {
                let start = chunk.as_ptr() as usize - log.as_ptr() as usize;
                s.spawn(move || {
                    let part = parse_chunk(padded, start, start + chunk.len(), table);
                    mmap::release(chunk);
                    part
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Result<_, _>>()
    })?;

    // Rank the distinct endpoints in byte order: `rank[id]`.
    let found: Vec<&[u8]> = (0..table.count.load(Relaxed))
        .map(|id| table.name(id))
        .collect();
    let mut order: Vec<u32> = (0..found.len() as u32).collect();
    order.sort_unstable_by_key(|&id| found[id as usize]);
    let mut rank = vec![0u32; found.len()];
    for (r, &id) in order.iter().enumerate() {
        rank[id as usize] = r as u32;
    }
    let names: Vec<&[u8]> = order.iter().map(|&id| found[id as usize]).collect();
    let endpoints = names.len();

    // The hours in order, and each hour's runs from every part.
    let mut hours: Vec<Head> = parts
        .iter()
        .flat_map(|p| p.runs.iter().map(|run| run.head))
        .collect();
    hours.sort_unstable();
    hours.dedup();
    let mut runs_of: Vec<Vec<(usize, Run, usize)>> = vec![Vec::new(); hours.len()];
    for (p, part) in parts.iter().enumerate() {
        for (k, run) in part.runs.iter().enumerate() {
            let end = part
                .runs
                .get(k + 1)
                .map_or(part.records.len(), |next| next.start as usize);
            runs_of[hours.binary_search(&run.head).unwrap()].push((p, *run, end));
        }
    }
    // How many hours still read each part's records: the hour that brings its
    // part's count to zero gives the rest of the records back. Runs of one part
    // are adjacent in an hour's list.
    let readers: Vec<AtomicUsize> = (0..parts.len()).map(|_| AtomicUsize::new(0)).collect();
    for runs in &runs_of {
        for (k, &(p, _, _)) in runs.iter().enumerate() {
            if k == 0 || runs[k - 1].0 != p {
                readers[p].fetch_add(1, Relaxed);
            }
        }
    }

    // The output is mapped shared at a size no output can exceed (each group's
    // line is at most `longest` bytes) and cut to its length at the end. On tmpfs,
    // where /out is, one write call per hour would serialise on the file's lock;
    // page faults on the mapping do not.
    let longest = 21 + names.iter().map(|n| n.len()).max().unwrap_or(0) + 6 * 11 + 1;
    let bound: usize = runs_of
        .iter()
        .map(|runs| {
            runs.iter()
                .map(|&(_, run, end)| end - run.start as usize)
                .sum::<usize>()
        })
        .map(|n| n.min(endpoints) * longest)
        .sum();
    output
        .set_len(bound as u64)
        .map_err(|e| format!("size {output_path}: {e}"))?;
    let file = mmap::map_mut(&output, bound).map_err(|e| format!("mmap {output_path}: {e}"))?;
    let file = file.as_mut_ptr() as usize; // shared by the threads, each hour its own bytes
    // ends[h] is where hour h's output ends, once known (hours are taken in order).
    let ends: Vec<AtomicUsize> = (0..=hours.len())
        .map(|h| AtomicUsize::new(if h == 0 { 0 } else { usize::MAX }))
        .collect();

    // Pass 2, parallel over hours: each thread takes the next hour, gathers its
    // records from every part, groups them by endpoint rank, computes each group's
    // statistics and the hour's length, then writes the hour once the previous
    // one's end is known.
    let next = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..threads {
            let (parts, rank, names, runs_of, next, readers, hours, ends) = (
                &parts, &rank, &names, &runs_of, &next, &readers, &hours, &ends,
            );
            s.spawn(move || {
                let (mut start, mut counts, mut values) =
                    (vec![0u32; endpoints + 1], vec![0u64; endpoints], Vec::new());
                let (mut cursor, mut groups) = (vec![0u32; endpoints + 1], Vec::new());
                loop {
                    let hour = next.fetch_add(1, Relaxed);
                    let Some(runs) = runs_of.get(hour) else {
                        break;
                    };
                    // Count per endpoint rank, errors in the high half of the same
                    // word (one increment per record), then turn the counts into
                    // start offsets, then place each latency.
                    counts.fill(0);
                    for &(p, run, end) in runs {
                        for &record in &parts[p].records[run.start as usize..end] {
                            // SAFETY: records hold ids of the table, which has a rank each.
                            let rank =
                                unsafe { *rank.get_unchecked((record >> ENDPOINT_SHIFT) as usize) };
                            counts[rank as usize] += 1 | ((record >> 20 & 1) as u64) << 32;
                        }
                    }
                    let mut present = 0;
                    for k in 0..endpoints {
                        start[k + 1] = start[k] + counts[k] as u32;
                        present += (counts[k] != 0) as usize;
                    }
                    values.resize(start[endpoints] as usize, 0);
                    cursor.copy_from_slice(&start);
                    let (values, cursor) = (&mut values[..], &mut cursor[..]);
                    for &(p, run, end) in runs {
                        let part = &parts[p];
                        let mut escapes = part.escapes[run.escapes as usize..].iter();
                        for &record in &part.records[run.start as usize..end] {
                            let latency = record & ESCAPE;
                            let latency = if latency == ESCAPE {
                                *escapes.next().unwrap()
                            } else {
                                latency
                            };
                            // SAFETY: ids have a rank each, and each rank's cursor stays
                            // below the next rank's start, which is at most values.len():
                            // the counts above are this hour's.
                            unsafe {
                                let rank = *rank.get_unchecked((record >> ENDPOINT_SHIFT) as usize)
                                    as usize;
                                let at = cursor.get_unchecked_mut(rank);
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
                            mmap::release(bytes_of(&parts[p].records[run.start as usize..end]));
                        }
                        let last = k == 0 || runs[k - 1].0 != p;
                        if last && readers[p].fetch_sub(1, AcqRel) == 1 {
                            mmap::release(bytes_of(&parts[p].records));
                        }
                    }
                    groups.clear();
                    groups.reserve(present);
                    let mut len = 0;
                    for rank in 0..endpoints {
                        let group = &mut values[start[rank] as usize..start[rank + 1] as usize];
                        if !group.is_empty() {
                            let [p50, p95, p99] = percentiles(group);
                            let stat = Stats {
                                rank: rank as u32,
                                fields: [
                                    group.len() as u32,
                                    p50,
                                    p95,
                                    p99,
                                    (counts[rank] >> 32) as u32,
                                ],
                            };
                            len += stat.line_len(names);
                            groups.push(stat);
                        }
                    }
                    let at = loop {
                        match ends[hour].load(Acquire) {
                            usize::MAX => thread::yield_now(),
                            at => break at,
                        }
                    };
                    ends[hour + 1].store(at + len, Release);
                    // SAFETY: bytes at..at + len of the mapping are this hour's alone,
                    // and at + len <= bound: every line is at most `longest` bytes.
                    let out =
                        unsafe { std::slice::from_raw_parts_mut((file as *mut u8).add(at), len) };
                    let mut written = 0;
                    for stat in &groups {
                        written += stat.write(&hours[hour], names, &mut out[written..]);
                    }
                    assert_eq!(written, len, "hour length");
                }
            });
        }
    });
    let total = ends[hours.len()].load(Relaxed);
    output
        .set_len(total as u64)
        .map_err(|e| format!("size {output_path}: {e}"))?;
    // The output is complete in the page cache. Exit now: dropping the tables
    // above, one free at a time, took a few milliseconds on the scoring box.
    std::process::exit(0)
}

/// The bytes of a u32 slice, to release its memory.
fn bytes_of(words: &[u32]) -> &[u8] {
    // SAFETY: any u32 is four plain bytes.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast(), words.len() * 4) }
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
        20 + 1
            + names[self.rank as usize].len()
            + self.numbers().iter().map(|&v| 1 + digits(v)).sum::<usize>()
            + 1
    }

    /// The line is put together from its end in a stack buffer, so no number's
    /// length is needed first, then copied to the start of `out`; returns its length.
    fn write(&self, head: &Head, names: &[&[u8]], out: &mut [u8]) -> usize {
        let mut line = [0u8; 256]; // 20 + 1 + 125 + 6 * 11 + 1 = 213 at most
        let mut at = line.len() - 1;
        line[at] = b'\n';
        for value in self.numbers().into_iter().rev() {
            at = put_decimal(&mut line, at, value) - 1;
            line[at] = b' ';
        }
        let name = names[self.rank as usize];
        at -= name.len();
        line[at..at + name.len()].copy_from_slice(name);
        at -= 21;
        line[at..at + 13].copy_from_slice(head);
        line[at + 13..at + 21].copy_from_slice(b":00:00Z ");
        out[..line.len() - at].copy_from_slice(&line[at..]);
        line.len() - at
    }
}

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

/// Parses the lines in `log[start..end]`; `log` goes on with mmap::PAD zero bytes.
/// Every line is "ts SP method SP endpoint SP status SP latency LF" (27-byte ts).
fn parse_chunk(
    log: &'static [u8],
    start: usize,
    end: usize,
    table: &Interner,
) -> Result<Part, String> {
    use std::arch::x86_64::*;
    // Sized for the worst case, so the loop writes without capacity checks; only
    // the pages actually written are ever backed by memory.
    let most = (end - start) / MIN_LINE + 1;
    let (mut records, mut escapes) = (
        Vec::<u32>::with_capacity(most),
        Vec::<u32>::with_capacity(most),
    );
    let mut runs = Vec::new();
    // SAFETY: every load stays within the line (at most 177 bytes) and the mmap::PAD
    // zero bytes after it: the space window ends 92 bytes in, the endpoint starts at
    // most 37 bytes in and its key loads read 64 bytes from there, and the latency
    // load reads 16 bytes from the status's end. Records and escapes are written
    // below their capacity: there are at most `most` lines.
    unsafe {
        let space = _mm256_set1_epi8(b' ' as i8);
        let (first_record, first_escape) = (records.as_mut_ptr(), escapes.as_mut_ptr());
        let (mut record, mut escape) = (first_record, first_escape);
        let (mut line, stop) = (log.as_ptr().add(start), log.as_ptr().add(end));
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
            let spaces = |at: usize| {
                movemask(_mm256_cmpeq_epi8(
                    _mm256_loadu_si256(line.add(28 + at).cast()),
                    space,
                )) as u64
            };
            let mask = spaces(0) | spaces(32) << 32;
            let from = 29 + mask.trailing_zeros() as usize;
            let rest = mask & (mask - 1);
            let to = if rest != 0 {
                28 + rest.trailing_zeros() as usize
            } else {
                let mut e = 92; // an endpoint of about 60 bytes or more
                while *line.add(e) != b' ' {
                    e += 1;
                }
                e
            };
            // The key: the endpoint's first 64 bytes, zero from the space on.
            let len = to - from;
            let keep = KEEP.as_ptr().cast::<u8>().add(64 - len.min(64));
            let key = [
                _mm256_and_si256(
                    _mm256_loadu_si256(line.add(from).cast()),
                    _mm256_loadu_si256(keep.cast()),
                ),
                _mm256_and_si256(
                    _mm256_loadu_si256(line.add(from + 32).cast()),
                    _mm256_loadu_si256(keep.add(32).cast()),
                ),
            ];
            let name = std::slice::from_raw_parts(line.add(from), len);
            let endpoint = table.id(&key, name)?;
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
    Ok(Part {
        records,
        escapes,
        runs,
    })
}

/// 64 bytes of 0xFF then 64 zeros: the 64 bytes from `64 - len` keep the first `len`.
static KEEP: [[u8; 64]; 2] = [[0xFF; 64], [0; 64]];

/// An endpoint's identity: its bytes zero-padded to 64. No endpoint byte is zero,
/// so for endpoints shorter than 64 bytes equal keys mean equal endpoints. A longer
/// endpoint gets the key of its first 64 bytes, which another long one can share,
/// so those (5% of lines) are compared byte by byte as well.
type Key = [std::arch::x86_64::__m256i; 2];
const KEY_BYTES: usize = 64;

/// `vpmovmskb`, kept opaque to the optimizer: LLVM rewrites the intrinsic as a
/// vector of booleans, and on CPUs without AVX-512 (the scoring box) it then
/// builds a 64-bit mask from two of them one bit at a time.
#[inline(always)]
fn movemask(v: std::arch::x86_64::__m256i) -> u32 {
    let mask: u32;
    // SAFETY: AVX2 is enabled for this build; the instruction reads one register.
    unsafe {
        std::arch::asm!("vpmovmskb {0:e}, {1}", out(reg) mask, in(ymm_reg) v, options(pure, nomem, nostack))
    };
    mask
}

/// The value of the `count` (1 to 10) ASCII digits at the start of `digits`: they
/// are moved to the end of the vector behind zeros, then combined pairwise by
/// multiply-adds into two eight-digit halves.
#[inline(always)]
fn parse_digits(digits: std::arch::x86_64::__m128i, count: usize) -> u64 {
    use std::arch::x86_64::*;
    // SAFETY: SSSE3 and SSE4.1 come with AVX2.
    unsafe {
        // Byte j takes digit j + count - 16; a negative index makes it zero.
        let index = _mm_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
        let order = _mm_add_epi8(index, _mm_set1_epi8(count as i8 - 16));
        let v = _mm_shuffle_epi8(_mm_sub_epi8(digits, _mm_set1_epi8(b'0' as i8)), order);
        let v = _mm_maddubs_epi16(
            v,
            _mm_setr_epi8(10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1, 10, 1),
        );
        let v = _mm_madd_epi16(v, _mm_setr_epi16(100, 1, 100, 1, 100, 1, 100, 1));
        let v = _mm_packus_epi32(v, v);
        let v = _mm_madd_epi16(v, _mm_setr_epi16(10000, 1, 10000, 1, 0, 0, 0, 0));
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

const SLOTS: usize = 8 * MAX_ENDPOINTS as usize;

impl Interner {
    fn new() -> Self {
        Interner {
            slots: (0..SLOTS).map(|_| AtomicU16::new(0)).collect(),
            keys: (0..MAX_ENDPOINTS)
                .map(|_| UnsafeCell::new(unsafe { std::mem::zeroed() }))
                .collect(),
            names: (0..MAX_ENDPOINTS)
                .map(|_| UnsafeCell::new(&[][..]))
                .collect(),
            count: AtomicUsize::new(0),
        }
    }

    /// The id of a published slot's endpoint if it is this one.
    #[inline(always)]
    fn matches(&self, slot: u16, key: &Key, name: &[u8]) -> Option<u32> {
        let id = slot as usize - 1;
        // SAFETY: a published id's key and name are written and never change.
        let (k, n) = unsafe {
            (
                &*self.keys.get_unchecked(id).get(),
                *self.names.get_unchecked(id).get(),
            )
        };
        (same(k, key) && (name.len() < KEY_BYTES || n == name)).then_some(id as u32)
    }

    /// The endpoint's id, added if new.
    #[inline(always)]
    fn id(&self, key: &Key, name: &'static [u8]) -> Result<u32, String> {
        let mut i = hash(key);
        loop {
            // SAFETY: i < SLOTS.
            let slot = unsafe { self.slots.get_unchecked(i) }.load(Acquire);
            if slot == 0 {
                return self.insert(i, key, name);
            }
            if let Some(id) = self.matches(slot, key, name) {
                return Ok(id);
            }
            i = (i + 1) % SLOTS;
        }
    }

    /// Adds an endpoint not found up to the empty slot `i`. If another thread
    /// fills that slot first with the same endpoint, its id wins and the one taken
    /// here stays unused (a duplicate name that no record refers to).
    #[cold]
    #[inline(never)]
    fn insert(&self, mut i: usize, key: &Key, name: &'static [u8]) -> Result<u32, String> {
        let id = self.count.fetch_add(1, Relaxed);
        if id >= MAX_ENDPOINTS as usize {
            return Err(format!("more than {MAX_ENDPOINTS} distinct endpoints"));
        }
        // A long name is also compared byte by byte later, after the input's pages
        // are released: copy it while they are still in place.
        let name = if name.len() < KEY_BYTES {
            name
        } else {
            Box::leak(name.into())
        };
        // SAFETY: id is this thread's alone until a slot publishes it.
        unsafe { (*self.keys[id].get(), *self.names[id].get()) = (*key, name) };
        loop {
            match self.slots[i].compare_exchange(0, id as u16 + 1, Release, Acquire) {
                Ok(_) => return Ok(id as u32),
                Err(slot) => {
                    if let Some(other) = self.matches(slot, key, name) {
                        return Ok(other);
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
        let (key, name) = unsafe { (&*self.keys[id].get(), *self.names[id].get()) };
        let bytes = unsafe { &*(key as *const Key).cast::<[u8; KEY_BYTES]>() };
        if name.len() < KEY_BYTES {
            &bytes[..name.len()]
        } else {
            name
        }
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
