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

#[cfg(not(target_feature = "avx2"))]
compile_error!("track-a needs AVX2: build with RUSTFLAGS=\"-C target-cpu=native\"");

use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering::*};
use std::{arch::x86_64::*, cell::UnsafeCell, fs::File, io::Error, os::fd::AsRawFd, ptr::null_mut};
use std::{sync::OnceLock, thread};

/// A record is `endpoint id << 21 | error << 20 | latency`: 11 bits of endpoint id,
/// bit 20 set for a 5xx status, and 20 bits of latency. A latency of ESCAPE or more
/// is stored as ESCAPE, its value appended to the part's escapes (1.2% of lines).
const ENDPOINT_SHIFT: u32 = 21;
const ESCAPE: u32 = (1 << 20) - 1;
/// Endpoint ids must fit the record's 11 bits: the spec has 1,842 endpoints.
const MAX_ENDPOINTS: usize = 1 << (32 - ENDPOINT_SHIFT);
/// Zero bytes mapped after the end of the log, so that fixed-size SIMD loads at
/// any line of it stay inside the mapping.
const PAD: usize = 256;

fn main() {
    // Transparent huge pages off: with THP set to "always", touching one byte of a
    // sparse, lazily zeroed table can fault in a whole 2 MB page, so the memory
    // peak would depend on how fragmented the host is that run.
    // SAFETY: a plain syscall; failure only leaves THP on. 41 is PR_SET_THP_DISABLE.
    unsafe { prctl(41, 1, 0, 0, 0) };
    let dir = |n, default: &str| std::env::args().nth(n).unwrap_or(default.into());
    let input = File::open(dir(1, "/data") + "/requests.log").expect("open the log");
    // Read and write: the output is mapped shared and written through memory.
    let mut options = File::options();
    options.read(true).write(true).create(true).truncate(true);
    let output = options.open(dir(2, "/out") + "/summary.txt");
    let output = output.expect("create the summary");
    let len @ 1.. = input.metadata().expect("stat the log").len() as usize else {
        return; // the spec's empty input gives an empty summary
    };
    // The log followed by PAD zero bytes: the whole range is reserved as zero
    // pages, then the file is mapped over its start (the rest of the file's last
    // page reads as zero). The dataset is mounted read-only, so the bytes cannot
    // change under us.
    let base = map(null_mut(), len + PAD, 1, 0x22, -1); // READ; PRIVATE | ANONYMOUS
    map(base, len, 1, 0x12, input.as_raw_fd()); // READ; PRIVATE | FIXED
    // SAFETY: `len + PAD` bytes are mapped and stay mapped until exit.
    let log: &[u8] = unsafe { std::slice::from_raw_parts(base, len) };

    // Pass 1, parallel: each thread parses one slice of the log, cut at line ends,
    // into packed records, with one endpoint table shared by all threads. Cut p is
    // just after the last LF before byte len * p / threads (0 if there is none; the
    // last cut is the end of the log, which ends with an LF). Cuts never decrease,
    // so the slices hold every line once; two cuts in one line give an empty slice.
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let lf = |p| log[..len * p / threads].iter().rposition(|&b| b == b'\n');
    let cut = |p| lf(p).map_or(0, |i| i + 1);
    // SAFETY: all-zero bytes are an empty table (every field is valid as zeros).
    // Allocated zeroed, its pages are mapped as the parse first touches them.
    let table: &Interner = Box::leak(unsafe { Box::new_zeroed().assume_init() });
    let parts: Vec<Part> = thread::scope(|s| {
        let spawn = |p| s.spawn(move || parse(&log[cut(p)..cut(p + 1)], table));
        let handles: Vec<_> = (0..threads).map(spawn).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    // (id, name) of every endpoint, in byte order of the names: first by the first
    // 16 bytes as one number, so few comparisons need the bytes themselves, then
    // by the full bytes on ties.
    // SAFETY: the parse is over, so nothing writes the table any more.
    let count = table.count.load(Relaxed);
    let names = table.names[..count].iter().map(|n| unsafe { &**n.get() });
    let mut endpoints: Vec<_> = names.enumerate().collect();
    endpoints.sort_by_cached_key(|&(_, name)| (sort_key(name), name));

    // Every part's runs as ((hour key, first record, first escape), part, end
    // record), in order of hour then part, cut into hours.
    let mut runs = Vec::new();
    for (p, (_, _, heads)) in parts.iter().enumerate() {
        runs.extend(heads.windows(2).map(|w| (w[0], p, w[1].1)));
    }
    runs.sort_unstable_by_key(|run| (run.0.0, run.1));
    let hours: Vec<&[_]> = runs.chunk_by(|a, b| a.0.0 == b.0.0).collect();
    // How many hours still read each part's records: the hour that brings its
    // part's count to zero gives the rest of them back.
    let readers: Vec<AtomicUsize> = parts.iter().map(|_| AtomicUsize::new(0)).collect();
    for runs in runs.chunk_by(|a, b| (a.0.0, a.1) == (b.0.0, b.1)) {
        readers[runs[0].1].fetch_add(1, Relaxed);
    }

    // The output is mapped shared at a size no output can exceed, and cut to its
    // length at the end. On tmpfs, where /out is, one write call per hour would
    // serialise on the file's lock; page faults on the mapping do not. A line is
    // the 21-byte hour label and its space, the name, six numbers of at most 10
    // digits after a space each, and LF: 21 + 1 + 6 * 11 + 1 = 89 bytes and the name.
    let longest = 89 + endpoints.iter().map(|e| e.1.len()).max().unwrap();
    let bound = hours.len() * endpoints.len() * longest;
    output.set_len(bound as u64).expect("size the summary");
    // Kept as an address: a raw pointer could not be shared by the threads below,
    // which each write only their own hours' bytes.
    let file = map(null_mut(), bound, 3, 1, output.as_raw_fd()) as usize; // READ | WRITE; SHARED
    // Where an hour's output starts, once the lengths of all earlier hours are known.
    let lens: Vec<OnceLock<usize>> = hours.iter().map(|_| OnceLock::new()).collect();
    let offset = |hour| lens[..hour].iter().try_fold(0, |at, n| Some(at + n.get()?));
    // Writes the pending hours whose start is known.
    let flush = |pending: &mut Vec<(usize, usize, Vec<(&[u8], Stats)>)>| {
        pending.retain(|&(hour, len, ref groups)| {
            let Some(at) = offset(hour) else { return true };
            // SAFETY: bytes at..at + len of the mapping are this hour's alone, and
            // at + len <= bound: every line is at most `longest` bytes.
            let out = unsafe { std::slice::from_raw_parts_mut((file + at) as _, len) };
            let head = (hours[hour][0].0.0 << 24).to_be_bytes(); // its first 13 bytes
            let mut written = 0;
            for &(name, stat) in groups {
                written += write(&head, name, stat, &mut out[written..]);
            }
            assert_eq!(written, len, "hour length");
            false
        })
    };

    // Pass 2, parallel over hours: each thread takes the next hour, gathers its
    // records from every part, groups them by endpoint id, computes each group's
    // statistics and the hour's length, then writes each of its hours once the
    // earlier hours' lengths are known, taking further hours meanwhile.
    let next = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let (mut start, mut counts) = ([0u32; MAX_ENDPOINTS + 1], [0u64; MAX_ENDPOINTS]);
                let (mut values, mut pending) = (Vec::new(), Vec::new());
                loop {
                    let hour = next.fetch_add(1, Relaxed);
                    let Some(&runs) = hours.get(hour) else { break };
                    // Count per endpoint id, errors in the high half of the same
                    // word (one increment per record), then set start[i + 1] to the
                    // start of id i's group and place each latency at it: start[i]
                    // then runs from id i's first value to its end.
                    counts.fill(0);
                    for &((_, from, _), p, to) in runs {
                        for &record in &parts[p].0[from..to] {
                            let id = (record >> ENDPOINT_SHIFT) as usize;
                            counts[id] += 1 | ((record >> 20 & 1) as u64) << 32;
                        }
                    }
                    // The low half of a count word is the record count (`as u32`).
                    let mut sum = 0;
                    for (k, &count) in counts.iter().enumerate() {
                        start[k + 1] = sum;
                        sum += count as u32;
                    }
                    values.resize(sum as usize, 0);
                    for (k, &((_, from, escape), p, to)) in runs.iter().enumerate() {
                        let (records, escapes, _) = &parts[p];
                        let mut escapes = escapes[escape..].iter();
                        for &record in &records[from..to] {
                            let latency = match record & ESCAPE {
                                ESCAPE => *escapes.next().unwrap(),
                                latency => latency,
                            };
                            let at = &mut start[(record >> ENDPOINT_SHIFT) as usize + 1];
                            // SAFETY: each id's cursor stays below the next id's
                            // start, which is at most values.len(): the counts
                            // above are this hour's.
                            unsafe { *values.get_unchecked_mut(*at as usize) = latency };
                            *at += 1;
                        }
                        // The records are no longer needed once placed. Long runs
                        // (shard 0's hourly uploads: 142 runs, 93% of the records) go
                        // at once, the rest with their part: a run under 4096 records
                        // (16 KB) frees at most three whole pages, too few for a
                        // madvise call and its TLB shootdown each. Runs of one part
                        // are adjacent: after its last one, the part has one reader less.
                        if to - from >= 4096 {
                            release(&records[from..to]);
                        }
                        let last = runs.get(k + 1).is_none_or(|run| run.1 != p);
                        if last && readers[p].fetch_sub(1, AcqRel) == 1 {
                            release(records);
                        }
                    }
                    // The groups in byte order of their names.
                    let (mut groups, mut len) = (Vec::with_capacity(endpoints.len()), 0);
                    for &(id, name) in &endpoints {
                        let group = &mut values[start[id] as usize..start[id + 1] as usize];
                        if !group.is_empty() {
                            let [p50, p95, p99] = percentiles(group);
                            let (n, errors) = (group.len() as u32, (counts[id] >> 32) as u32);
                            let rate = (errors as u64 * 1_000_000 / n as u64) as u32;
                            let stat = [n, p50, p95, p99, errors, rate];
                            len += line_len(name, &stat);
                            groups.push((name, stat));
                        }
                    }
                    lens[hour].set(len).unwrap();
                    pending.push((hour, len, groups));
                    flush(&mut pending);
                }
                while !pending.is_empty() {
                    thread::yield_now();
                    flush(&mut pending);
                }
            });
        }
    });
    let total = offset(hours.len()).unwrap();
    output.set_len(total as u64).expect("size the summary");
    // The output is complete in the page cache. Exit now: dropping the tables
    // above, one free at a time, took a few milliseconds on the scoring box.
    std::process::exit(0)
}

/// Up to the first 16 bytes, zero-padded, as a number that sorts like them: big
/// endian, so the first byte is the most significant.
fn sort_key(bytes: &[u8]) -> u128 {
    let padded = bytes.iter().chain(&[0; 16]);
    padded.take(16).fold(0, |key, &b| key << 8 | b as u128)
}

/// The six numbers of a group's line: count, p50, p95, p99, errors and the error
/// rate in parts per million. All fit 32 bits: latencies are unsigned 32-bit
/// numbers (the spec).
type Stats = [u32; 6];

/// "YYYY-MM-DDTHH:00:00Z endpoint" then the numbers, each after a space, and LF.
fn line_len(name: &[u8], stat: &Stats) -> usize {
    let digits = |v: u32| v.checked_ilog10().map_or(1, |d| d as usize + 1);
    21 + name.len() + stat.iter().map(|&v| 1 + digits(v)).sum::<usize>() + 1
}

/// The line is put together from its end in a stack buffer, so no number's
/// length is needed first, then copied to the start of `out`; returns its length.
fn write(head: &[u8; 16], name: &[u8], stat: Stats, out: &mut [u8]) -> usize {
    let mut line = [0u8; 256]; // 21 + 125 + 6 * 11 + 1 = 213 at most
    let mut at = line.len() - 1;
    line[at] = b'\n';
    for mut value in stat.into_iter().rev() {
        // Two digits at a time from the end, then the first digit of an odd-length
        // number (or the only one).
        let end = at;
        while value >= 10 {
            let pair = value as usize % 100 * 2;
            at -= 2;
            line[at..at + 2].copy_from_slice(&PAIRS[pair..pair + 2]);
            value /= 100;
        }
        if value > 0 || at == end {
            at -= 1;
            line[at] = b'0' + value as u8;
        }
        at -= 1;
        line[at] = b' ';
    }
    at -= 21 + name.len();
    line[at..at + 13].copy_from_slice(&head[..13]);
    line[at + 13..at + 21].copy_from_slice(b":00:00Z ");
    line[at + 21..at + 21 + name.len()].copy_from_slice(name);
    out[..line.len() - at].copy_from_slice(&line[at..]);
    line.len() - at
}

/// "00", "01", ..., "99".
static PAIRS: &[u8; 200] = b"000102030405060708091011121314151617181920212223242526272829303132\
    33343536373839404142434445464748495051525354555657585960616263646566676869707172737475767778\
    798081828384858687888990919293949596979899";

/// One thread's parse of its slice: one record per line, in file order (see
/// ENDPOINT_SHIFT); the latencies that did not fit, in the same order; and where
/// each run of lines in one hour starts, as (hour key, first record, first
/// escape), then an end marker.
type Part = (Vec<u32>, Vec<u32>, Vec<(u128, usize, usize)>);

/// Parses the lines of `chunk`, a slice of the mapped log, which goes on with PAD
/// zero bytes, then gives the chunk's pages back. Every line is
/// "ts SP method SP endpoint SP status SP latency LF" (27-byte ts).
fn parse(chunk: &[u8], table: &Interner) -> Part {
    // Sized for the worst case, so the loop writes without capacity checks; only
    // the pages actually written are ever backed by memory. The shortest line is 40
    // bytes: timestamp, "GET", "/", status, one digit, four spaces and LF.
    let most = chunk.len() / 40 + 1;
    let [mut records, mut escapes] = [(); 2].map(|_| Vec::<u32>::with_capacity(most));
    let mut runs = Vec::new();
    // SAFETY: every load stays within the line (at most 177 bytes) and the PAD zero
    // bytes after the log: the space windows end at most 188 bytes in, the endpoint
    // starts at most 37 bytes in and its key loads read 64 bytes from there, and the
    // latency load reads 16 bytes from the status's end, which hold the LF (at most
    // 10 digits). Records and escapes are written below their capacity: there are at
    // most `most` lines, and each line writes one record and one escape slot.
    unsafe {
        let space = _mm256_set1_epi8(b' ' as i8);
        let (mut record, mut escape) = (records.as_mut_ptr(), escapes.as_mut_ptr());
        let (mut line, stop) = (chunk.as_ptr(), chunk.as_ptr_range().end);
        // Lines from one shard arrive in time order, so most lines repeat the
        // previous line's hour. Its bytes "MM-DDTHH" decide: every timestamp lies in
        // one 72-hour span (the spec), so lines that share them share the year.
        let mut last = u64::MAX;
        while line < stop {
            // Bytes 5..13 of the timestamp, "MM-DDTHH", as one word.
            let hour = line.add(5).cast::<u64>().read_unaligned();
            if hour != last {
                last = hour;
                let key = u128::from_be_bytes(*line.cast()) >> 24; // the first 13 bytes
                let at = record.offset_from_unsigned(records.as_ptr());
                runs.push((key, at, escape.offset_from_unsigned(escapes.as_ptr())));
            }
            // One 64-byte window after the timestamp: its first two spaces end the
            // method and the endpoint (no branch on the method or the length). Byte 27
            // is the space after the timestamp, so the window starts at the method,
            // byte 28; the endpoint starts one byte after the method's space.
            let load = |p: *const u8| _mm256_loadu_si256(p.cast());
            let spaces = |at| movemask(_mm256_cmpeq_epi8(load(line.add(28 + at)), space)) as u64;
            let mask = spaces(0) | spaces(32) << 32;
            let from = 29 + mask.trailing_zeros() as usize;
            // The mask without its lowest bit: its lowest bit is now the second space.
            let rest = mask & (mask - 1);
            let mut to = 28 + rest.trailing_zeros() as usize;
            if rest == 0 {
                // No second space in bytes 28..92, so the endpoint's space is further
                // on (7.6% of lines): search on, 32 bytes at a time.
                let mut at = 64; // an endpoint of about 60 bytes or more: on by 32
                while spaces(at) == 0 {
                    at += 32;
                }
                to = 28 + at + spaces(at).trailing_zeros() as usize;
            }
            // The key: the endpoint's first 64 bytes, zero from the space on.
            let keep = KEEP.as_ptr().cast::<u8>().add(64 - (to - from).min(64));
            let half = |at| _mm256_and_si256(load(line.add(from + at)), load(keep.add(at)));
            let name = std::slice::from_raw_parts(line.add(from), to - from);
            let endpoint = table.id(&[half(0), half(32)], name);
            // The latency: up to 10 digits from 5 bytes after the endpoint (its space,
            // the three status digits and a space), to the LF.
            let (latency, count) = parse_digits(line.add(to + 5));
            // The escape slot is written for every line and kept (the cursor moves
            // on) only for a latency that does not fit: no branch.
            *escape = latency as u32;
            escape = escape.add((latency >= ESCAPE as u64) as usize);
            // The status's first digit is right after the endpoint's space; '5' is 5xx.
            let error = ((*line.add(to + 1) == b'5') as u32) << 20;
            *record = endpoint << ENDPOINT_SHIFT | error | latency.min(ESCAPE as u64) as u32;
            record = record.add(1);
            line = line.add(to + 5 + count + 1);
        }
        records.set_len(record.offset_from_unsigned(records.as_ptr()));
        escapes.set_len(escape.offset_from_unsigned(escapes.as_ptr()));
    }
    runs.push((u128::MAX, records.len(), escapes.len()));
    release(chunk);
    (records, escapes, runs)
}

/// 64 bytes of 0xFF then 64 zeros: the 64 bytes from `64 - len` keep the first `len`.
static KEEP: [[u8; 64]; 2] = [[0xFF; 64], [0; 64]];

/// An endpoint's identity: its bytes zero-padded to 64. No endpoint byte is zero,
/// so for endpoints shorter than 64 bytes equal keys mean equal endpoints. A longer
/// endpoint gets the key of its first 64 bytes, which another long one can share,
/// so those (63 of the 1,842 endpoints, on 6.2% of lines) are compared byte by byte
/// as well.
type Key = [__m256i; 2];

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

/// The value and the number of the 1 to 10 ASCII digits at `at`, which end at an
/// LF within 16 bytes: they are moved to the end of the vector behind zeros, then
/// combined pairwise by multiply-adds into two eight-digit halves.
#[inline(always)]
fn parse_digits(at: *const u8) -> (u64, usize) {
    // SAFETY: SSSE3 and SSE4.1 come with AVX2; the caller makes the 16 bytes at `at`
    // readable.
    unsafe {
        let digits = _mm_loadu_si128(at.cast());
        let lf = _mm_cmpeq_epi8(digits, _mm_set1_epi8(b'\n' as i8));
        let count = _mm_movemask_epi8(lf).trailing_zeros() as usize;
        // Byte j takes digit j + count - 16; a negative index makes it zero.
        let index = _mm_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
        let order = _mm_add_epi8(index, _mm_set1_epi8(count as i8 - 16));
        let v = _mm_shuffle_epi8(_mm_sub_epi8(digits, _mm_set1_epi8(b'0' as i8)), order);
        let v = _mm_maddubs_epi16(v, _mm_set1_epi16(0x010A)); // pairs: 10 * a + b
        let v = _mm_madd_epi16(v, _mm_set1_epi32(0x0001_0064)); // fours: 100 * a + b
        let v = _mm_packus_epi32(v, v);
        let v = _mm_madd_epi16(v, _mm_set1_epi32(0x0001_2710)); // eights: 10000 * a + b
        let both = _mm_cvtsi128_si64(v) as u64;
        ((both & 0xFFFF_FFFF) * 100_000_000 + (both >> 32), count)
    }
}

/// Open-addressing endpoint table keyed by the 64-byte key, shared by the parsing
/// threads: lookups are plain loads, the rare insert claims its slot by
/// compare-and-swap. Fixed at SLOTS, 8x the most endpoints (an 11% load): ids must
/// fit 11 bits anyway.
struct Interner {
    slots: [AtomicU16; SLOTS], // 0 = empty, else id + 1
    keys: [UnsafeCell<Key>; MAX_ENDPOINTS],
    names: [UnsafeCell<*const [u8]>; MAX_ENDPOINTS], // the endpoints' bytes
    count: AtomicUsize,                              // ids handed out
}

// SAFETY: an id's key and name are written once, by the thread that took the id,
// before a slot publishes the id (Release). They are read only once a slot holding
// the id has been seen (Acquire), or after the parse.
unsafe impl Sync for Interner {}

const SLOTS: usize = 8 * MAX_ENDPOINTS;

impl Interner {
    /// The endpoint's id, added if new. The probe starts at the key folded to 16
    /// bytes, then two CRC steps: collisions only cost a probe. SLOTS is a power
    /// of two, so `%` is a mask. Keys are compared as two vector compares, and the
    /// bytes too for endpoints of 64 bytes or more (see Key).
    #[inline(always)]
    fn id(&self, key: &Key, name: &[u8]) -> u32 {
        // SAFETY: AVX2 and SSE4.2 are enabled for this build. A published id is
        // below MAX_ENDPOINTS, and its key and name are written (see Sync above).
        unsafe {
            let x = _mm256_xor_si256(key[0], key[1]);
            let x = _mm_xor_si128(_mm256_castsi256_si128(x), _mm256_extracti128_si256(x, 1));
            let [low, high]: [u64; 2] = std::mem::transmute(x);
            let mut i = _mm_crc32_u64(_mm_crc32_u64(0, low), high) as usize % SLOTS;
            loop {
                let id = match self.slots[i].load(Acquire) {
                    0 => self.add(i, key, name),
                    slot => slot as usize - 1,
                };
                let known = &*self.keys.get_unchecked(id).get();
                let low = _mm256_xor_si256(known[0], key[0]);
                let diff = _mm256_or_si256(low, _mm256_xor_si256(known[1], key[1]));
                let same_bytes = || &**self.names.get_unchecked(id).get() == name;
                if _mm256_testz_si256(diff, diff) != 0 && (name.len() < 64 || same_bytes()) {
                    return id as u32;
                }
                i = (i + 1) % SLOTS;
            }
        }
    }

    /// Adds an endpoint not found up to the empty slot `i`, and returns the id that
    /// slot then holds: if another thread filled it first, its id wins (it may be
    /// this endpoint, then the id taken here is a duplicate no record refers to).
    /// Running out of ids stops the program rather than writing a wrong record.
    /// The bytes are copied: they are compared and printed after the input's pages
    /// are released.
    #[cold]
    #[inline(never)]
    fn add(&self, i: usize, key: &Key, name: &[u8]) -> usize {
        let id = self.count.fetch_add(1, Relaxed);
        assert!(id < MAX_ENDPOINTS, "too many distinct endpoints");
        // SAFETY: the id is this thread's alone until a slot publishes it.
        unsafe { (*self.keys[id].get(), *self.names[id].get()) = (*key, name.to_vec().leak()) };
        let won = self.slots[i].compare_exchange(0, id as u16 + 1, Release, Acquire);
        won.map_or_else(|slot| slot as usize - 1, |_| id)
    }
}

/// Nearest-rank p50, p95 and p99 (the value at 1-indexed rank ceil(p * n / 100)
/// in sorted order) of a non-empty group, by selection instead of a full sort: p50
/// first, then each next rank within the part above the last one. Each value is
/// read at once: the next selection reorders that part, its own slot included.
fn percentiles(values: &mut [u32]) -> [u32; 3] {
    let [r50, r95, r99] = [50, 95, 99].map(|p| (p * values.len()).div_ceil(100) - 1);
    // Most groups are small: 91% have 32 values or fewer (28% of the records). There
    // the standard library's small-input sort beats three selections; 32 was the
    // best of 16, 32 and 64 (see PERF.md).
    if values.len() <= 32 {
        values.sort_unstable();
        return [r50, r95, r99].map(|r| values[r]);
    }
    let p50 = *values.select_nth_unstable(r50).1;
    let p95 = *values[r50..].select_nth_unstable(r95 - r50).1;
    [p50, p95, *values[r95..].select_nth_unstable(r99 - r95).1]
}

// SAFETY: these are the C library's own prototypes on x86-64 Linux, where C's int
// is i32 and long is i64; declared by hand so that the crate has no dependencies.
unsafe extern "C" {
    fn mmap(at: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn madvise(at: *mut u8, len: usize, advice: i32) -> i32;
    fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
}

/// mmap at file offset 0. Mappings are never unmapped: the process exits right
/// after using them.
fn map(at: *mut u8, len: usize, prot: i32, flags: i32, fd: i32) -> *mut u8 {
    // SAFETY: each caller maps fresh memory or replaces its own reservation.
    let ptr = unsafe { mmap(at, len, prot, flags, fd, 0) };
    assert!(ptr as isize != -1, "mmap: {}", Error::last_os_error());
    ptr
}

/// Drops this process's pages for the whole pages inside `slice`, which is done
/// with. At exit the kernel would otherwise unmap the 58,000 pages of the input one
/// by one, on one core (1.3 ms slower overall when measured, see PERF.md); this does
/// the same work in each parsing thread, in parallel. Reading the input again is
/// still correct: the page faults back in from the page cache. Anonymous memory
/// (the records) is freed and would read as zeros, but it is never read again.
fn release<T>(slice: &[T]) {
    let start = (slice.as_ptr() as usize).next_multiple_of(4096);
    let end = slice.as_ptr_range().end as usize / 4096 * 4096;
    // SAFETY: whole pages of a private mapping whose contents are no longer needed
    // (see above); no reference observes a change. 4 is MADV_DONTNEED.
    unsafe { madvise(start as *mut u8, end.saturating_sub(start), 4) };
}
