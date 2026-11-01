//! Track B (Index): answer 50,000 BOOL and PHRASE queries over a 254 MB corpus.
//!
//! There is no index: the corpus is scanned once, in parallel slices, and every
//! query is answered during that scan, so nothing grows with the corpus.
//! 1. The queries come first. Only their terms are looked up, in a perfect hash
//!    of them: one 16-byte slot per term, fetched in advance. Each document's
//!    tokens are found first, then looked up.
//! 2. BOOL queries are evaluated per block of 64 documents. While a block is
//!    scanned, each query term present gets a 64-bit word with one bit per
//!    document, so at the block end AND, OR and NOT are word operations, applied
//!    strictly left to right.
//! 3. A query is evaluated only in blocks that contain one of its cover terms, a
//!    set of terms every match contains (see `cover`), chosen to be rare from the
//!    sample. Queries with common covers are evaluated in every superblock of 256
//!    documents instead, with AVX2 on four words per term.
//! 4. Each phrase is anchored on its rarest term and checked against the token
//!    ids of the document when it ends, while they are still in L1.
//! 5. Per query and thread, the scan keeps the match count, the sum of the ids
//!    (from popcounts of bit positions) and the first ten ids. The threads' slices
//!    are in document order, so merging them in order gives the ten smallest.

mod mmap;

use std::arch::asm;
use std::arch::x86_64::*;
use std::cmp::Reverse;
use std::mem::{replace, take, transmute};
use std::sync::Barrier;
use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, Ordering::Relaxed};

#[cfg(not(all(target_feature = "avx2", target_feature = "bmi2")))]
compile_error!("track-b needs AVX2 and BMI2: build with RUSTFLAGS=\"-C target-cpu=native\"");
#[cfg(not(target_feature = "aes"))]
compile_error!("track-b needs AES-NI: build with RUSTFLAGS=\"-C target-cpu=native\"");

/// A token's info word: its term id, plus ANCHOR if a phrase is anchored on it.
/// A token that no query uses gets MISS.
const ANCHOR: u32 = 1 << 31;
const MISS: u32 = ANCHOR - 1;
/// Terms of up to KEY bytes live in the perfect hash, keyed by their bytes.
const KEY: usize = 12;
/// The spec's grammar: a BOOL query has 1 to 6 terms.
const MAX_BOOL_TERMS: usize = 6;
/// A BOOL term word is `slot | op << 30`, and TERM masks the slot. The op codes
/// make `end_block` branch-free.
const TERM: u32 = (1 << 30) - 1;
const AND: u32 = 0;
const NOT: u32 = 1;
const OR: u32 = 3;
/// The spec's answer: the match count, the sum of the matching ids, and the
/// first (smallest) FIRST_IDS ids.
const FIRST_IDS: u64 = 10;
/// Corpus bytes sampled (spread over the slices) to estimate term frequencies:
/// about 1,030 documents on seed 1. (2 MB planned no better and took 1.4 ms more;
/// 512 KB planned worse.)
const SAMPLE_BYTES: usize = 1 << 20;
/// A query whose cover terms are expected in more blocks than this (as a share,
/// summed over the terms) is evaluated in every superblock: one evaluation then
/// covers four blocks. (0.125 and 0.5 were measured slower.)
const ALWAYS: f64 = 0.20;
/// Documents per superblock: four blocks of 64.
const SUPER: u32 = 256;
/// A thread's sum of matching ids is packed under its count in one u64.
const SUM_BITS: u32 = 40;
/// Document ids must be below this: a thread's sum of ids is then below
/// MAX_DOCS^2 / 2 = 2^39, which fits in SUM_BITS.
const MAX_DOCS: u32 = 1 << 20;
/// How far ahead the block end prefetches: the entry range of the touched slot
/// SLOTS_AHEAD on, and the words and state that the range RANGES_AHEAD on reads
/// first (PERF.md, steps 20 and 28).
const SLOTS_AHEAD: usize = 16;
const RANGES_AHEAD: usize = 4;
/// Hash groups: a key's group is the low 6 bits of its hash (hash & 63). A pilot
/// is 1 mod GROUPS, so each group owns a region of the table, and the threads
/// place their groups in parallel.
const GROUPS: usize = 64;

fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    mmap::no_huge_pages();
    let open = |name: &str| mmap::map(&format!("{data_dir}/{name}"));
    let docs = open("docs.txt");
    // The corpus: `docs` without the mmap::PAD zero bytes that follow it.
    let text = &docs[..docs.len() - mmap::PAD];
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let (queries, mut vocab) = parse_queries(open("queries.txt"), threads);
    let slices = slices(text, threads);

    // Sample the start of every slice for each term's document frequency.
    let df: Vec<AtomicU32> = (0..vocab.count).map(|_| AtomicU32::new(0)).collect();
    let sampled = par(slices.len(), &|i| {
        let (start, end, first) = slices[i];
        let stop = line_after(text, start + SAMPLE_BYTES / slices.len()).min(end);
        // SAFETY: stop <= end <= text.len(), and mmap::PAD (256) bytes follow text.
        let scan = unsafe { Scan::new(&vocab, None, &df).run(docs, start, stop, first) };
        scan.doc + 1 - first
    });
    let df: Vec<u32> = df.into_iter().map(AtomicU32::into_inner).collect();
    let plan = Plan::new(queries, &df, sampled.iter().sum::<u32>().max(1), threads);
    vocab.finish(&plan);
    // Give the setup's freed memory back before the scan: glibc keeps it after
    // the large blocks it has seen freed (about 4 MB of the memory peak).
    // SAFETY: a glibc call that takes no pointers.
    unsafe { mmap::malloc_trim(0) };

    let results = par(slices.len(), &|i| {
        let (start, end, first) = slices[i];
        // Only the answers and the last id are kept: the scan's other tables
        // are freed here, and the first ids below reuse their memory.
        let (Answers { state, log, logged }, doc) = {
            // SAFETY: end <= text.len(), and mmap::PAD (256) bytes follow text.
            let scan = unsafe { Scan::new(&vocab, Some(&plan), &[]).run(docs, start, end, first) };
            (scan.answers, scan.doc)
        };
        // Drop the slice's pages now, in parallel, instead of the kernel
        // unmapping all 62,000 at exit on one core.
        // SAFETY: docs[start..end] is part of the mapping made by mmap::map.
        unsafe { mmap::release(&docs[start..end]) };
        // The ids go on in the next slice, and stay below MAX_DOCS.
        let next = slices.get(i + 1).map_or(doc + 1, |s| s.2);
        let ok = slices[0].2 == 0 && next == doc + 1 && doc < MAX_DOCS;
        assert!(ok, "docs.txt: ids must be 0, 1, 2, ... below 2^20");
        // The first ids by line number, without a sort: an answer logged the
        // first min(count, FIRST_IDS) of its ids, in order, so each line's place in
        // the result is known from the counts.
        let logged_of = |a: u32| (state[a as usize] >> SUM_BITS).min(FIRST_IDS) as u32;
        let place = |n: &mut u32, &a: &u32| Some(replace(n, *n + logged_of(a)));
        let mut at: Vec<u32> = plan.answer_of.iter().scan(0, place).collect();
        let mut firsts = vec![0; logged];
        for &e in &log[..logged] {
            let line = plan.line_of[(e >> 32) as usize] as usize;
            firsts[at[line] as usize] = (line as u64) << 32 | e & u32::MAX as u64;
            at[line] += 1;
        }
        (state, firsts)
    });

    // Format in parallel, one range of lines per thread.
    let parts = slices.len().max(1);
    let chunks = par(parts, &|p| format_part(&plan, &results, p, parts));
    let mut file = std::fs::File::create(format!("{out_dir}/results.txt")).expect("results.txt");
    for chunk in chunks {
        std::io::Write::write_all(&mut file, &chunk).expect("write results.txt");
    }
    // Done: exit without freeing the tables and logs, one free at a time.
    std::process::exit(0)
}

/// The output lines of the p-th of `parts` ranges, from each thread's `(state,
/// log)` in `results`. Per line: the threads' counts and sums added up (each
/// state read through `answer_of`: no copy of the states, which would cost
/// 4.5 MB), then the first ids taken from the threads in slice order. The packed
/// states add up as they are: all ids are below MAX_DOCS = 2^20, so any sum of
/// them is below 2^39 and never carries into the count.
fn format_part(plan: &Plan, results: &[(Vec<u64>, Vec<u64>)], p: usize, parts: usize) -> Vec<u8> {
    let lines = part(plan.answer_of.len(), p, parts);
    // About 32 output bytes per line: a capacity only.
    let (mut out, mut row) = (Vec::with_capacity(lines.len() * 32), Vec::new());
    let first = |log: &Vec<u64>| log.partition_point(|&e| e >> 32 < lines.start as u64);
    let mut cursors: Vec<usize> = results.iter().map(|(_, log)| first(log)).collect();
    for line in lines {
        // The threads' states of the line 8 ahead: they are random reads.
        let ahead = plan.answer_of.get(line + 8).map_or(0, |&a| a as usize);
        results.iter().for_each(|(state, _)| prefetch(state.as_ptr(), ahead));
        let answer = plan.answer_of[line] as usize;
        let s: u64 = results.iter().map(|(state, _)| state[answer]).sum();
        // The output row: count, sum, then the threads' first ids, 2 + FIRST_IDS
        // = 12 fields at most.
        row.clear();
        row.extend([s >> SUM_BITS, s & ((1 << SUM_BITS) - 1)]);
        for ((_, log), cursor) in results.iter().zip(&mut cursors) {
            while let Some(&entry) = log.get(*cursor).filter(|&&e| e >> 32 == line as u64) {
                row.push(entry & u32::MAX as u64);
                *cursor += 1;
            }
        }
        row.iter().take(12).for_each(|&v| push_decimal(&mut out, v));
        // push_decimal ends each number with a space: the last one becomes the
        // LF (a row has at least its count and sum, so `out` is not empty).
        *out.last_mut().unwrap() = b'\n';
    }
    out
}

/// f(0), f(1), ... f(n - 1), each on its own thread.
fn par<T: Send>(n: usize, f: &(impl Fn(usize) -> T + Sync)) -> Vec<T> {
    std::thread::scope(|s| {
        let threads: Vec<_> = (0..n).map(|i| s.spawn(move || f(i))).collect();
        // join cannot fail: with panic = "abort", a panic ends the process first.
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    })
}

/// The p-th of `parts` near-equal ranges of 0..len.
fn part(len: usize, p: usize, parts: usize) -> std::ops::Range<usize> {
    len * p / parts..len * (p + 1) / parts
}

/// `(spans, terms)`. Per line of queries.txt, its span `(start, len, phrase)`:
/// its terms `terms[start..start + len]`, and whether it is a phrase. The terms
/// are BOOL terms as `id | op << 30`, the first with OR, and phrase terms as ids.
type Queries = (Vec<Span>, Vec<u32>);
type Span = (u32, u32, bool);

/// Splits `chunk`, whole lines of `padded` (the file and mmap::PAD zero bytes),
/// into its lines, as in `Queries` but from 0, and their terms: each as its
/// offset in `padded` and its length | op << 30 (in a BOOL query). Also the
/// hashes (with `seed`) of the terms' table keys (the zero key for a long term)
/// with their offsets and key lengths, grouped by hash & 63 (`starts`), and the
/// long terms. (The long terms' zero key gets an id that no term uses.) The
/// vectors are sized for the most terms a chunk can hold (a 1-byte term and its
/// space: len / 2) and the most lines (a line is at least "BOOL a" and LF, 7
/// bytes, under most / 3), so they never grow: their untouched pages cost nothing.
fn split_lines(chunk: &'static [u8], padded: &'static [u8], seed: u64) -> Split {
    let most = chunk.len() / 2 + 1;
    let (mut lines, mut long) = (Vec::with_capacity(most / 3), Vec::new());
    let (mut terms, mut keys) = (Vec::with_capacity(most), Vec::with_capacity(most));
    // Every line is a query of the spec's grammar (2.3) and ends with LF.
    for line in chunk.split_inclusive(|&b| b == b'\n') {
        let (start, phrase) = (terms.len(), line[0] == b'P');
        // The words after `BOOL ` or `PHRASE "`, before LF or `"` and LF.
        let (from, to) = if phrase { (8, 2) } else { (5, 1) };
        let words = &line[from..line.len() - to];
        let mut op = if phrase { 0 } else { OR };
        for (i, word) in words.split(|&b| b == b' ').enumerate() {
            if phrase || i % 2 == 0 {
                let at = (word.as_ptr() as usize - padded.as_ptr() as usize) as u32;
                terms.push((at, word.len() as u32 | op << 30));
                long.extend((word.len() > KEY).then_some(word));
                // Key length 0 gives the zero key, which marks a long term.
                let len = word.len() as u32 * (word.len() <= KEY) as u32;
                let h = hash(checked_key(padded, at as usize, len as usize), seed);
                keys.push(Key { hash: h, at, len });
            } else {
                // `AND`, `NOT` and `OR` start with bytes ending in binary 01, 10 and 11.
                // queries.txt is trusted (spec 2.3): any other word reads as AND
                // (index 0).
                op = [0, AND, NOT, OR][(word[0] & 3) as usize];
            }
        }
        lines.push((start as u32, (terms.len() - start) as u32, phrase));
    }
    // Grouped by hash & 63, the hash group.
    let by_group = keys.iter().map(|&k| ((k.hash & 63) as u32, k)).collect();
    let (starts, grouped) = csr(&[by_group], GROUPS);
    (lines, terms, starts, grouped, long)
}

/// One thread's share of the query lines, from `split_lines`: `(lines, terms,
/// starts, keys, long)`. Its lines, as spans of its terms from 0; its terms; its
/// keys, those of hash group g in `keys[starts[g]..starts[g + 1]]`; and its terms
/// over KEY bytes.
type Split = (Vec<Span>, Vec<Term>, Vec<u32>, Vec<Key>, Vec<Long>);
type Long = &'static [u8];
/// A term's offset, and its length | op << 30.
type Term = (u32, u32);

/// A term's table key: its hash, and the offset and length of its bytes (length
/// 0 for a long term, whose key is zero). The key bytes are reloaded from the
/// file when they are needed.
#[derive(Clone, Copy, Default)]
struct Key {
    hash: u64,
    at: u32,
    len: u32,
}

/// Splits the lines in parallel, then gives the terms ids: their slots in the
/// perfect hash. `padded` is the file and mmap::PAD zero bytes.
fn parse_queries(padded: &'static [u8], threads: usize) -> (Queries, Vocab) {
    let text = &padded[..padded.len() - mmap::PAD];
    // The start of the p-th of `threads` ranges of whole lines.
    let cut = |p: usize| line_after(text, text.len() * p / threads);
    let chunk = |p| &text[cut(p)..cut(p + 1)];
    let split = |seed| par(threads, &|p| split_lines(chunk(p), padded, seed));
    // Ends with probability 1: each seed gives unrelated hashes (two keys clash
    // with probability about 2^-64), and every third seed halves the load
    // (Vocab::new), so the pilot search finds free slots. Seed 0 succeeds on
    // the full seed-1, holdout seed-99 and sample seed 1 to 4 inputs.
    let (vocab, ids, splits) = (0..).find_map(|s| Vocab::new(split(s), s, padded)).unwrap();
    // The spec's 50,000 queries, about 155,000 terms on seed 1: capacities only.
    let (mut lines, mut terms) = (Vec::with_capacity(50_000), Vec::with_capacity(160_000));
    for ((spans, ..), ids) in splits.iter().zip(ids) {
        let base = terms.len() as u32;
        lines.extend(spans.iter().map(|&(s, n, p)| (base + s, n, p)));
        terms.extend(ids);
    }
    ((lines, terms), vocab)
}

/// The info of `slot` if its key is `key`, else MISS. A slot is a term's key
/// (12 bytes, zero-padded) under its info word (the top 4 bytes): the key bytes
/// are compared in one vptest (`mask` selects them), and the cmov keeps it
/// branch-free (LLVM would branch).
///
/// # Safety
///
/// `slot` points to a slot of the table: 16 readable bytes, 16-byte aligned.
#[inline(always)]
unsafe fn probe(slot: *const u128, key: __m128i) -> u32 {
    // SAFETY: `slot` is valid for an aligned 16-byte read (the caller's contract;
    // u128 is 16-byte aligned); the asm reads registers only. AVX2 is enabled for
    // the whole build.
    unsafe {
        let x = _mm_xor_si128(_mm_load_si128(slot.cast()), key);
        // The info word on its own: a load, where vpextrd would be two uops.
        let mut info = *slot.cast::<u32>().add(3);
        asm!("vptest {x}, {mask}", "cmovnz {i:e}, {miss:e}", x = in(xmm_reg) x, mask = in(xmm_reg) _mm_set_epi32(0, -1, -1, -1),
             i = inout(reg) info, miss = in(reg) MISS, options(pure, nomem, nostack));
        info
    }
}

/// The table key of the token `bytes[at..at + len]`, len <= KEY: its bytes,
/// ASCII-lowercased and zero-padded to 16. OR-ing 0x20 lowercases A-Z and keeps
/// digits; KEEP keeps only the token's bytes.
///
/// # Safety
///
/// `at + 16 <= bytes.len()` and `len <= 16`: the key is one 16-byte load from
/// `at`, masked by the 16 bytes of KEEP from `16 - len`.
#[inline(always)]
unsafe fn key_at(bytes: &[u8], at: usize, len: usize) -> __m128i {
    debug_assert!(at + 16 <= bytes.len() && len <= 16);
    // SAFETY: both loads are in bounds by the caller's contract (KEEP has 32
    // bytes, and 16 - len + 16 <= 32); AVX2 is enabled for the whole build.
    unsafe {
        let raw = _mm_loadu_si128(bytes.as_ptr().add(at).cast());
        let keep = _mm_loadu_si128(KEEP.as_flattened().as_ptr().add(16 - len).cast());
        _mm_and_si128(_mm_or_si128(raw, _mm_set1_epi8(0x20)), keep)
    }
}

/// `key_at` with its contract checked, for the setup's term records.
fn checked_key(bytes: &[u8], at: usize, len: usize) -> __m128i {
    assert!(len <= KEY, "a term key of {len} bytes, over KEY");
    // SAFETY: the slice is exactly the 16 bytes key_at loads, and len <= KEY < 16.
    unsafe { key_at(&bytes[at..at + 16], 0, len) }
}

/// The table key of the term record `e` of `file`, as the table stores it.
fn key_of(file: Long, e: Key) -> u128 {
    // SAFETY: __m128i and u128 have the same size, and any bits are valid.
    unsafe { transmute(checked_key(file, e.at as usize, e.len as usize)) }
}

/// Two AES rounds of the key, its low 64 bits as the hash. AESENC does
/// ShiftRows, SubBytes and MixColumns, then XORs the round key: after two rounds
/// every output byte depends on every key byte, non-linearly, and a different
/// first round key (the seed) gives an unrelated hash. Three instructions, one
/// cycle of throughput each. The round keys are digits of pi; a loop that
/// hashes keeps them in registers.
#[inline(always)]
fn hash(key: __m128i, seed: u64) -> u64 {
    let first = 0x243f6a8885a308d3_13198a2e03707344 ^ seed as u128;
    // SAFETY: u128 and __m128i have the same size, and any bits are valid.
    let [a, b]: [__m128i; 2] =
        unsafe { transmute([first, 0x24093822299f31d0_082efa98ec4e6c89u128]) };
    // SAFETY: AES-NI and SSE2 are enabled for the whole build (checked above).
    unsafe { _mm_cvtsi128_si64(_mm_aesenc_si128(_mm_aesenc_si128(key, a), b)) as u64 }
}

/// A window into it at 16 - len keeps the first len bytes of 16.
static KEEP: [[u8; 16]; 2] = [[0xFF; 16], [0; 16]];

/// Every query term, with its id and, after `finish`, its info.
///
/// The terms of up to 12 bytes are in a perfect hash (PTHash style): a term
/// with hash h is in bucket h & bmask, and the bucket's pilot puts it in slot
/// `(h * pilot).rotate_right(6) >> shift`. Every term has its own slot, so a
/// token is compared with one slot only. Two terms of a bucket share the low
/// bits of h but differ above, so different multipliers move them apart. A pilot
/// is 1 mod GROUPS, so the low 6 bits of h * pilot are those of h (its group):
/// rotated to the top, they make each group's slots a region of its own, and the
/// groups are placed in parallel.
struct Vocab {
    phf: Phf,
    /// 2^(64 - shift) slots of the hash, one per long term, then one with the
    /// info MISS. Empty slots have the zero key, which no token has (token bytes
    /// are never zero).
    table: Vec<u128>,
    /// The ids of the terms of 13 to 32 bytes, the last ones: about 0.2% of the
    /// corpus tokens are that long.
    long: std::collections::HashMap<&'static [u8], u32>,
    /// The number of terms: their ids are 0..count.
    count: u32,
}

/// The hash function of the table. Copy, so that the scan keeps it in registers.
#[derive(Clone, Copy)]
struct Phf {
    /// XORed into the hash's first round key.
    seed: u64,
    /// Per bucket (a power of two of them, at least 64), its pilot. Leaked (they
    /// live until the process exits), and atomic so that the threads of the build
    /// can write them (Phf::slot reads them as plain u16s, after the build).
    pilot: &'static [AtomicU16],
    /// 64 minus the table's bits: the slot is the top bits of the rotated product.
    shift: u32,
}

impl Phf {
    /// The slot of a short key (a term's if the key is one).
    #[inline(always)]
    fn slot(self, key: __m128i) -> usize {
        let h = hash(key, self.seed);
        // SAFETY: BMI2 is enabled for the whole build. The bucket, the low bits
        // of h, is below len (a power of two, at least GROUPS). The pilot is read
        // as a plain u16 (an atomic load would cost a second zero extension):
        // AtomicU16 has the layout of u16, and pilots are only read once the build
        // is over, which the build's barrier and the end of its threads order
        // before the reads.
        let pilot = unsafe {
            let bucket = _bzhi_u64(h, self.pilot.len().trailing_zeros()) as usize;
            self.pilot.get_unchecked(bucket).as_ptr().read()
        };
        place(h, pilot as u64, self.shift)
    }
}

/// The slot of the hash h with the pilot `pilot`: below 2^(64 - shift).
#[inline(always)]
fn place(h: u64, pilot: u64, shift: u32) -> usize {
    (h.wrapping_mul(pilot).rotate_right(GROUPS.ilog2()) >> shift) as usize
}

/// Thread q's keys for `Vocab::new`: those of its range of the hash groups,
/// from every split, sorted by bucket and then hash, one per hash; or None when
/// two different keys have the same hash. A bucket's hashes share their low bits,
/// so it lies in one group (hash & 63).
fn dedup_keys(splits: &[Split], q: usize, bmask: u64, file: Long) -> Option<Vec<Key>> {
    let g = part(GROUPS, q, splits.len());
    let total: usize = splits.iter().map(|(.., keys, _)| keys.len()).sum();
    // Twice a thread's mean share of keys, plus slack: a capacity only.
    let mut keys = Vec::with_capacity(2 * total / splits.len() + 64);
    for (_, _, starts, grouped, _) in splits {
        keys.extend_from_slice(&grouped[starts[g.start] as usize..starts[g.end] as usize]);
    }
    // By bucket, then hash: the rotation puts the bucket bits on top.
    keys.sort_unstable_by_key(|e| e.hash.rotate_right(bmask.count_ones()));
    let no_clash = |w: &[Key]| w[0].hash != w[1].hash || key_of(file, w[0]) == key_of(file, w[1]);
    let ok = keys.windows(2).all(no_clash);
    keys.dedup_by_key(|e| e.hash);
    ok.then_some(keys)
}

/// The first pilot whose slots for `bucket` are all free, claimed one at a time
/// with `set(key, pilot, true)` (which returns whether the slot was taken) and
/// given back at the first taken one; or None. The candidates are 1, 65, 129,
/// ...: 1 mod GROUPS keeps the group bits, and there are 1,024 of them.
fn find_pilot(bucket: &[Key], mut set: impl FnMut(&Key, u64, bool) -> bool) -> Option<u64> {
    (1..1 << 16).step_by(GROUPS).find(|&pilot| {
        let free = bucket.iter().take_while(|e| !set(e, pilot, true)).count();
        // Gives the claimed slots back unless all were free.
        let all = free == bucket.len();
        bucket[..free].iter().for_each(|e| _ = set(e, pilot, all));
        all
    })
}

impl Vocab {
    /// The vocabulary of the query terms in `splits`, with the hash of `seed`,
    /// the term words of each split with their ids, and `splits`; or None when
    /// the build needs another seed: two different keys with one hash, or a
    /// bucket that finds no pilot. Every third seed also doubles the table
    /// (parse_queries says why the seeds end).
    ///
    /// Each thread takes a range of groups and deduplicates their keys
    /// (`dedup_keys`). Then, in its own region of the table, it places their
    /// buckets and writes their keys and ids, and once every thread has, it gives
    /// its split's terms their ids. About 4.6 distinct terms per bucket and at
    /// least 1.5 slots per term (a load of 0.58 on seed 1). `file` is the query
    /// file and mmap::PAD zero bytes.
    fn new(splits: Vec<Split>, seed: u64, file: Long) -> Option<(Self, Vec<Vec<u32>>, Vec<Split>)> {
        let n = splits.len();
        let total: usize = splits.iter().map(|(.., keys, _)| keys.len()).sum();
        // One bucket per 16 term occurrences (about 4.6 distinct terms each on
        // seed 1), and at least one per group.
        let bmask = (total / 16).max(GROUPS).next_power_of_two() as u64 - 1;
        let groups = par(n, &|q| dedup_keys(&splits, q, bmask, file));
        let groups: Vec<_> = groups.into_iter().collect::<Option<_>>()?;
        let distinct: usize = groups.iter().map(Vec::len).sum();
        // At least 1.5 slots per term; every third seed doubles the table.
        let bits = (distinct * 3 / 2).max(1024).next_power_of_two().ilog2() + seed as u32 / 3;
        let (slots, shift, region) = (1 << bits, 64 - bits, bits - GROUPS.ilog2());
        let mut long = std::collections::HashMap::new();
        for &term in splits.iter().flat_map(|(.., long_terms)| long_terms) {
            let id = (distinct + long.len()) as u32;
            long.entry(term).or_insert(id);
        }
        // The table is allocated zeroed, so empty slots cost nothing until they
        // are read. Long term `id` has the slot `slots + id - distinct`.
        let count = distinct + long.len();
        let mut table = vec![0u128; slots + long.len() + 1];
        (distinct..count).for_each(|id| table[slots + id - distinct] = (id as u128) << 96);
        table[slots + long.len()] = (MISS as u128) << 96;
        let pilot: &[_] = Vec::leak((0..=bmask).map(|_| AtomicU16::new(0)).collect());
        let phf = Phf { seed, pilot, shift };
        // The table's address as a usize, so that the closure is Sync; each thread
        // writes only its own regions (below).
        let (at, barrier) = (table.as_mut_ptr().expose_provenance(), Barrier::new(n));
        let ids = par(n, &|q| {
            let table = std::ptr::with_exposed_provenance_mut::<u128>(at);
            // Largest buckets first, each with the first pilot whose slots are
            // all free (`find_pilot`). `taken` covers this thread's regions, from
            // `base`. Thread q numbers its keys from the number of keys before
            // its groups.
            let base = part(GROUPS, q, n).start << region;
            let mut taken = vec![false; part(GROUPS, q, n).len() << region];
            let mut next = groups[..q].iter().map(Vec::len).sum::<usize>() as u128;
            let same_bucket = |a: &Key, b: &Key| (a.hash ^ b.hash) & bmask == 0;
            let mut order: Vec<_> = groups[q].chunk_by(same_bucket).collect();
            order.sort_unstable_by_key(|b| Reverse(b.len()));
            let slot = |e: &Key, pilot| place(e.hash, pilot, shift) - base;
            let mut set = |e: &Key, pilot, v| replace(&mut taken[slot(e, pilot)], v);
            let placed = order.iter().all(|bucket| {
                let fits = find_pilot(bucket, &mut set);
                let Some(pilot) = fits else { return false };
                phf.pilot[(bucket[0].hash & bmask) as usize].store(pilot as u16, Relaxed);
                for e in *bucket {
                    let value = key_of(file, *e) | next << 96;
                    // SAFETY: set() indexed taken[slot] with a bounds check above,
                    // and taken covers exactly this thread's regions of the table,
                    // which no other thread writes: base + slot(e, pilot) =
                    // place(..) < slots < table.len().
                    unsafe { *table.add(base + slot(e, pilot)) = value };
                    next += 1;
                }
                true
            });
            // The ids need every thread's pilots and slots.
            barrier.wait();
            // SAFETY: Phf::slot is below 2^(64 - shift) = slots, so a slot of the
            // table, which no thread writes any more.
            let info = |key| (unsafe { *table.add(phf.slot(key)) } >> 96) as u32;
            // Only long terms are in `long`: a lookup there hashes all the bytes.
            // Every long term is in it, so the index does not fail.
            let id = |&(at, len): &Term| {
                let (at, op, len) = (at as usize, len & !TERM, (len & TERM) as usize);
                op | match len {
                    0..=KEY => info(checked_key(file, at, len)),
                    _ => long[&file[at..at + len]],
                }
            };
            // splits[q].1: this thread's split's terms.
            placed.then(|| splits[q].1.iter().map(id).collect())
        });
        let vocab = Vocab {
            phf,
            table,
            long,
            count: count as u32,
        };
        Some((vocab, ids.into_iter().collect::<Option<_>>()?, splits))
    }

    /// Gives each slot the info the plan gives its id (renumbered, ANCHOR).
    fn finish(&mut self, plan: &Plan) {
        // Empty slots read as id 0 and get id 0's info: harmless, their zero key
        // matches no token. The last slot keeps MISS (the table always has it).
        for s in self.table.split_last_mut().unwrap().1 {
            *s = *s & ((1 << 96) - 1) | (plan.info[(*s >> 96) as usize] as u128) << 96;
        }
    }

    /// A long token's key and the byte offset of its slot: the zero key, and
    /// the slot of its term or the last one, which the zero key matches.
    #[cold]
    #[inline(never)]
    fn get_long(&self, token: &[u8]) -> (__m128i, u64) {
        // A token is at most 64 bytes (its length comes from a 64-bit mask).
        let lower = &mut [0u8; 64][..token.len()];
        lower.copy_from_slice(token);
        lower.make_ascii_lowercase();
        let id = self.long.get(&*lower).map_or(self.count, |&id| id);
        let slot = self.table.len() + id as usize - 1 - self.count as usize;
        let offset = (slot * size_of::<u128>()) as u64;
        // SAFETY: SSE2 is enabled for the whole build.
        (unsafe { _mm_setzero_si128() }, offset)
    }
}

/// A BOOL query as triggered by one of its cover terms.
#[derive(Clone, Copy, Default)]
struct Entry {
    /// `slot | op << 30` per term, padded with AND of the all-ones slot, so the
    /// evaluation is the same six steps for every query.
    terms: [u32; MAX_BOOL_TERMS],
    /// The answer whose state the evaluation updates.
    answer: u32,
    /// In `Plan::entries`, bit k: term k is a cover term that comes earlier in
    /// the cover; when it is present in the block, that term's entry evaluates
    /// the query instead. In `Plan::always` (whose entries have no cover): the
    /// number of terms.
    earlier_or_len: u32,
}

/// What the scan does with each query, decided from the sample.
#[derive(Default)]
struct Plan {
    /// BOOL entries triggered by slot t: `entries[group[t]..group[t + 1]]`.
    group: Vec<u32>,
    entries: Vec<Entry>,
    /// BOOL entries evaluated in every superblock, on rows of `Scan::wide`.
    /// (For these entries, Entry::earlier_or_len holds the number of terms.)
    always: Vec<Entry>,
    /// The terms of `always` have the ids 0..rows, which are also their rows.
    rows: usize,
    /// Phrases anchored on term t: `anchored[anchor[t]..anchor[t + 1]]`.
    anchor: Vec<u32>,
    /// Per phrase: `(answer, offset, start, len)`, its answer, the anchor's
    /// position in the phrase, and its terms `terms[start..start + len]`.
    anchored: Vec<(u32, u32, u32, u32)>,
    /// The queries' term words, with the new ids.
    terms: Vec<u32>,
    /// The info word of each term, by its old id: its new id, and ANCHOR.
    info: Vec<u32>,
    /// The number of BOOL terms: they have the new ids 0..bool_terms.
    bool_terms: usize,
    /// Each line's answer, the index of its state in each thread, and back (one
    /// answer per line).
    answer_of: Vec<u32>,
    line_of: Vec<u32>,
}

impl Plan {
    /// `sample_df` counts the documents of each term in a sample of `sampled`
    /// documents (at least 1). The per-line work runs on `parts` threads, a
    /// range of lines each.
    fn new((spans, terms): Queries, sample_df: &[u32], sampled: u32, parts: usize) -> Self {
        let lines = spans.len();
        // The expected share of 64-document blocks that contain t: the cost of
        // triggering on it.
        let cost = |t: u32| 1.0 - (1.0 - sample_df[t as usize] as f64 / sampled as f64).powi(64);
        // Per old id: bit 0 for a BOOL term, bit 1 for a term of an `always` query.
        let kind: Vec<AtomicU8> = (0..sample_df.len()).map(|_| AtomicU8::new(0)).collect();
        // Whether each line is a BOOL query evaluated in every superblock.
        let always_line = |line: usize| {
            let (start, len, phrase) = spans[line];
            let words = &terms[start as usize..(start + len) as usize];
            is_always(words, phrase, cost, &kind)
        };
        let always_part = |p| part(lines, p, parts).map(always_line).collect::<Vec<_>>();
        let always_lines = par(parts, &always_part).concat();
        let kind: Vec<u8> = kind.into_iter().map(AtomicU8::into_inner).collect();
        let (order, rank) = term_order(&kind, sample_df);
        let bool_terms = kind.iter().filter(|&&k| k > 0).count();
        let rows = kind.iter().filter(|&&k| k == 3).count();
        // The costs by new id, so `cover` finds the sets `always_line` found.
        let cost = |t: u32| cost(order[t as usize]);
        let lines_part = |p: usize| {
            let mine = part(lines, p, parts);
            let mut by_trigger = Vec::with_capacity(MAX_BOOL_TERMS * mine.len());
            let (mut by_anchor, mut always) = (Vec::with_capacity(mine.len()), Vec::new());
            // This part's terms, renumbered: they follow each other in `terms`.
            let first = spans.get(mine.start).map_or(terms.len(), |s| s.0 as usize);
            let end = spans.get(mine.end).map_or(terms.len(), |s| s.0 as usize);
            let renumber = |&word: &u32| rank[(word & TERM) as usize] | word & !TERM;
            let renumbered: Vec<u32> = terms[first..end].iter().map(renumber).collect();
            for line in mine {
                let (start, len, phrase) = spans[line];
                let words = &renumbered[start as usize - first..(start + len) as usize - first];
                let answer = line as u32; // until number_answers
                if phrase {
                    // The first of the rarest terms (a phrase has at least one).
                    let rarest = |&k: &usize| sample_df[order[words[k] as usize] as usize];
                    let offset = (0..words.len()).min_by_key(rarest).unwrap();
                    by_anchor.push((words[offset], (answer, offset as u32, start, len)));
                    continue;
                }
                // Padded with AND of the all-ones row (rows + 1) or slot
                // (bool_terms + 1, the one after the sink).
                let pad = 1 + if always_lines[line] { rows } else { bool_terms };
                let mut entry = Entry {
                    terms: [pad as u32; MAX_BOOL_TERMS],
                    answer,
                    earlier_or_len: len,
                };
                entry.terms[..words.len()].copy_from_slice(words);
                if always_lines[line] {
                    always.push(entry);
                    continue;
                }
                let (cover, n) = cover(words, cost);
                for (i, &c) in cover[..n].iter().enumerate() {
                    // Bit k: term k is in cover[..i] (the bits are distinct, so + is |).
                    let early = |k: usize| (cover[..i].contains(&(words[k] & TERM)) as u32) << k;
                    entry.earlier_or_len = (0..words.len()).map(early).sum();
                    by_trigger.push((c, entry));
                }
            }
            (by_trigger, by_anchor, always, renumbered)
        };
        // Part p's (by_trigger, by_anchor, always, renumbered) is out[p].
        let mut out = par(parts, &lines_part);
        let by_trigger: Vec<_> = out.iter_mut().map(|o| take(&mut o.0)).collect();
        let by_anchor: Vec<_> = out.iter_mut().map(|o| take(&mut o.1)).collect();
        let mut always: Vec<_> = out.iter().flat_map(|o| o.2.iter().copied()).collect();
        let terms: Vec<_> = out.iter().flat_map(|o| o.3.iter().copied()).collect();
        let mut plan = Plan::default();
        // The anchors are grouped and the `always` entries sorted on a second
        // thread, while this one groups the entries.
        (plan.anchor, plan.anchored) = std::thread::scope(|s| {
            let anchors = s.spawn(|| {
                // By length and operators, so that `end_super` predicts its branches.
                always.sort_unstable_by_key(|e| (e.earlier_or_len, e.terms.map(|word| word >> 30)));
                csr(&by_anchor, order.len())
            });
            (plan.group, plan.entries) = csr(&by_trigger, bool_terms + 1);
            anchors.join().unwrap()
        });
        plan.number_answers(&mut always, lines);
        let flag = |id: usize| (plan.anchor[id] != plan.anchor[id + 1]) as u32 * ANCHOR;
        plan.info = rank.iter().map(|&id| id | flag(id as usize)).collect();
        (plan.always, plan.terms, plan.rows, plan.bool_terms) = (always, terms, rows, bool_terms);
        plan
    }

    /// Numbers the answers in the order the scan visits them: the `always` ones
    /// first, so that each superblock reads and writes their state as one stream
    /// (47 KB instead of 4,000 scattered lines), then those of the entries in
    /// their order, then the phrases. Every line gets a number: it has an entry
    /// (a cover has at least one term), is an `always` one or is a phrase.
    fn number_answers(&mut self, always: &mut [Entry], lines: usize) {
        self.answer_of = vec![u32::MAX; lines];
        let mut number = |line: &mut u32| {
            if self.answer_of[*line as usize] == u32::MAX {
                self.answer_of[*line as usize] = self.line_of.len() as u32;
                self.line_of.push(*line);
            }
            *line = self.answer_of[*line as usize];
        };
        always.iter_mut().for_each(|e| number(&mut e.answer));
        self.entries.iter_mut().for_each(|e| number(&mut e.answer));
        self.anchored.iter_mut().for_each(|a| number(&mut a.0));
    }
}

/// Whether a line, with its term words and whether it is a phrase, is a BOOL
/// query evaluated in every superblock: its cover terms are expected in more
/// than ALWAYS of the blocks, summed. A BOOL line's terms get kind bit 0, and
/// bit 1 too when it is such a query.
fn is_always(words: &[u32], phrase: bool, cost: impl Fn(u32) -> f64, kind: &[AtomicU8]) -> bool {
    let (cover, n) = cover(words, &cost);
    let always = !phrase && cover[..n].iter().map(|&t| cost(t)).sum::<f64>() > ALWAYS;
    for &word in words.iter().filter(|_| !phrase) {
        kind[(word & TERM) as usize].fetch_or(1 | (always as u8) << 1, Relaxed);
    }
    always
}

/// The new order of the terms (old ids by new id), and each old id's new id.
/// BOOL terms first, those of the `always` lines first among them, each part by
/// sampled frequency, most frequent first, so the presence words and entries the
/// scan uses most share cache lines; then the phrase-only terms in id order. A
/// counting sort by kind: 3, then 1, then 0.
fn term_order(kind: &[u8], sample_df: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let most = sample_df.iter().copied().max().unwrap_or(0) + 1;
    // Kind 2 never occurs: bit 1 is only set together with bit 0.
    let key = |t: usize| match kind[t] {
        0 => 2 * most,
        k => (3 - k as u32) / 2 * most + most - 1 - sample_df[t],
    };
    let keyed = (0..sample_df.len()).map(|t| (key(t), t as u32)).collect();
    let (_, order) = csr(&[keyed], 2 * most as usize + 1);
    let mut rank = vec![0; order.len()];
    (0..order.len()).for_each(|id| rank[order[id] as usize] = id as u32);
    (order, rank)
}

/// A set of terms of which every match contains at least one, built left to
/// right: after `AND t` every match contains t, so {t} replaces the set when it
/// is cheaper; after `OR t` a match contains t or a term of the set; `NOT`
/// only removes matches. Returns the set in an array, and its size.
fn cover(terms: &[u32], cost: impl Fn(u32) -> f64) -> ([u32; MAX_BOOL_TERMS], usize) {
    let (mut cover, mut n) = ([0; MAX_BOOL_TERMS], 0);
    for &word in terms {
        let t = word & TERM;
        match word >> 30 {
            OR if !cover[..n].contains(&t) => (cover[n], n) = (t, n + 1),
            AND if cost(t) < cover[..n].iter().map(|&c| cost(c)).sum() => (cover[0], n) = (t, 1),
            _ => {}
        }
    }
    (cover, n)
}

/// Groups values by key: values of key k are `values[start[k]..start[k + 1]]`,
/// in their original order (the parts' pairs one part after another).
fn csr<T: Copy + Default>(parts: &[Vec<(u32, T)>], keys: usize) -> (Vec<u32>, Vec<T>) {
    let mut start = vec![0u32; keys + 1];
    for &(k, _) in parts.iter().flatten() {
        start[k as usize + 1] += 1;
    }
    (0..keys).for_each(|k| start[k + 1] += start[k]);
    let (mut next, mut values) = (start.clone(), vec![T::default(); start[keys] as usize]);
    for &(k, value) in parts.iter().flatten() {
        values[next[k as usize] as usize] = value;
        next[k as usize] += 1;
    }
    (start, values)
}

/// One thread's pass over a slice of the corpus. Without a plan it only counts
/// document frequencies into `df`, for the sample.
struct Scan<'a> {
    vocab: &'a Vocab,
    plan: Option<&'a Plan>,
    df: &'a [AtomicU32],
    /// The presence words. Per BOOL term, bit i: document `base + i` of this
    /// block contains it. Slot bool_terms, the sink, collects the tokens of no
    /// BOOL query; slot bool_terms + 1 is all ones.
    presence: Vec<u64>,
    /// The slots set in this block (`touched[..n]`), to evaluate and clear.
    touched: Vec<u32>,
    n: usize,
    /// The entry ranges `(start, end)` of the touched slots that trigger any.
    work: Vec<(u32, u32)>,
    /// Row r < plan.rows: the words of BOOL term r in the 4 blocks of this
    /// superblock. Row `rows` takes the other terms; row `rows + 1` is all ones.
    wide: Vec<Row>,
    /// This document's tokens to look up: their keys and the byte offsets of
    /// their slots.
    pending: Vec<(__m128i, u64)>,
    /// This document's term ids, and the positions of anchors among them.
    toks: Vec<u32>,
    anchors: Vec<u32>,
    /// The current document.
    doc: u32,
    answers: Answers,
    /// Per anchored phrase, the last document it matched, to count each once.
    last: Vec<u32>,
}

impl<'a> Scan<'a> {
    fn new(vocab: &'a Vocab, plan: Option<&'a Plan>, df: &'a [AtomicU32]) -> Self {
        let (bool_terms, rows) = plan.map_or((0, 0), |p| (p.bool_terms, p.rows));
        let answers = plan.map_or(0, |p| p.line_of.len());
        let (mut presence, mut wide) = (written_zeros(bool_terms + 2), vec![Row([0; 4]); rows + 2]);
        (presence[bool_terms + 1], wide[rows + 1]) = (!0, Row([!0; 4]));
        Scan {
            vocab,
            plan,
            df,
            presence,
            touched: vec![0; bool_terms + 2],
            n: 0,
            work: Vec::with_capacity(bool_terms + 2),
            wide,
            pending: Vec::new(),
            toks: Vec::new(),
            anchors: Vec::new(),
            doc: 0,
            answers: Answers {
                state: written_zeros(answers),
                // One more: `add` always writes the slot after the logged ids.
                log: vec![0; answers * FIRST_IDS as usize + 1],
                logged: 0,
            },
            last: vec![u32::MAX; plan.map_or(0, |p| p.anchored.len())],
        }
    }

    /// Scans the whole lines in `docs[start..end]`, whose first id is `first`.
    ///
    /// # Safety
    ///
    /// `end + 16 <= docs.len()`: a token's key is one 16-byte load from its first
    /// byte, which is before `end`. (main passes slices of the text, which
    /// mmap::PAD zero bytes follow.) masks() also reads the 64 bytes of the next
    /// window, which starts below end + 63, so up to end + 126 (bounds-checked);
    /// PAD = 256 covers both.
    unsafe fn run(mut self, docs: &'static [u8], start: usize, end: usize, first: u32) -> Self {
        self.doc = first.wrapping_sub(1);
        let mut bit = 0u64;
        // The scan goes by windows of 64 bytes from `at`, where byte `at` is
        // never inside a token: it starts the slice (and so a line), or it is
        // the byte that ended the window before. A window is scanned up to its
        // last byte that is not a token byte (`cut`), so every token it scans
        // ends inside it, and a token's length is the distance to its last byte.
        let mut at = start;
        let (mut mask, mut lf) = masks(docs, at, end);
        while at < end {
            let cut = 63u32.wrapping_sub((!mask).leading_zeros());
            if cut.wrapping_sub(1) >= 63 {
                // No such byte after bit 0: a token of 63 bytes or more starts
                // at bit 0 (the slice's first id) or at bit 1.
                let (p, id) = (at + (mask & 1 == 0) as usize, mask & 1 != 0 || lf & 1 != 0);
                at = self.long_run(docs, p, end, id);
                (mask, lf) = masks(docs, at, end);
                continue;
            }
            // The next window, fetched before this one is scanned.
            let (next, next_lf) = masks(docs, at + cut as usize, end);
            // Bit 0 starts a token only at the slice start, which starts a line.
            // SAFETY: BMI2 is enabled for the whole build.
            let mut starts = unsafe { _bzhi_u64(mask & !(mask << 1), cut) };
            // A line that starts with a separator has no id token: its text
            // joins the previous document (the next id check fails if an id is
            // missing).
            let mut ids = starts & (lf << 1 | 1);
            // The last byte of each token (those after the cut are never used).
            let mut ends = mask & !(mask >> 1);
            // A window has at most 32 tokens: two tokens are at least one byte
            // apart, so at most 32 of its 64 bytes start one.
            self.pending.reserve(32);
            loop {
                // The tokens before the next id token, if any.
                let mut text = starts & (ids & ids.wrapping_neg()).wrapping_sub(1);
                starts ^= text;
                let buf = self.pending.as_mut_ptr();
                let mut out = buf.wrapping_add(self.pending.len());
                let (vocab, phf, table) = (self.vocab, self.vocab.phf, self.vocab.table.as_ptr());
                while text != 0 {
                    let (b, e) = (text.trailing_zeros() as usize, ends.trailing_zeros() as usize);
                    (text, ends) = (text & (text - 1), ends & (ends - 1));
                    let p = at + b;
                    // 1 - len, so that the key's KEEP window is a fixed offset from it.
                    let back = b as isize - e as isize;
                    if back > -(KEY as isize) {
                        // SAFETY: p < end (masks clips the token bits at end), so
                        // p + 16 <= docs.len() (see # Safety); 1 - back = len <= KEY.
                        let key = unsafe { key_at(docs, p, (1 - back) as usize) };
                        let off = phf.slot(key) * size_of::<u128>();
                        prefetch(table.cast::<u8>(), off);
                        // SAFETY: `reserve(32)` above made room for this window's tokens.
                        unsafe { out.write((key, off as u64)) };
                    } else {
                        // SAFETY: as above.
                        unsafe { out.write(vocab.get_long(&docs[p..=p + e - b])) };
                    }
                    out = out.wrapping_add(1);
                }
                // SAFETY: the entries up to `out` are written.
                unsafe { self.pending.set_len(out.offset_from(buf) as usize) };
                if ids == 0 {
                    break;
                }
                ids &= ids - 1;
                let (b, e) = (starts.trailing_zeros(), ends.trailing_zeros());
                (starts, ends) = (starts & (starts - 1), ends & (ends - 1));
                let (len, p) = ((e + 1 - b) as usize, at + b as usize);
                let id = parse_id(&docs[p..p + len]);
                let ok = docs[p + len] == b'\t' && id == self.doc.wrapping_add(1);
                assert!(ok, "docs.txt: ids must be 0, 1, 2, ..., each before a TAB");
                // End the current document at the id token of the next one.
                self.look_up(bit);
                self.end_doc();
                if id % 64 == 0 && self.n > 0 {
                    self.end_block(id - 64);
                }
                // At the first id of a slice, this may evaluate an empty
                // superblock: harmless.
                if id % SUPER == 0 && id > 0 {
                    self.end_super(id - SUPER);
                }
                self.doc = id;
                bit = 1 << (id % 64);
            }
            at += cut as usize;
            (mask, lf) = (next, next_lf);
        }
        self.look_up(bit);
        self.end_doc();
        self.end_block(self.doc & !63);
        self.end_super(self.doc / SUPER * SUPER);
        self
    }

    /// A token from `p` that is too long for a window (63 bytes or more), so too
    /// long to be a term: it goes to `pending` as a long token. An id token that
    /// long is rejected by `parse_id`. Returns the end of the token.
    #[cold]
    fn long_run(&mut self, docs: &[u8], p: usize, end: usize, id: bool) -> usize {
        let len = docs[p..end].iter().take_while(|b| b.is_ascii_alphanumeric()).count();
        if id {
            parse_id(&docs[p..p + len]);
        }
        self.pending.push(self.vocab.get_long(&docs[p..p + len.min(64)]));
        p + len
    }

    /// Looks up the tokens of this document, whose bit is `bit`, and sets their
    /// presence words. Each step runs over the whole document, so that the memory
    /// accesses of one step overlap: the slots were fetched when the tokens were
    /// found. (Fetching the words too cost more instructions than it saved.)
    fn look_up(&mut self, bit: u64) {
        // The sink: the slot after the BOOL terms', where the other tokens go.
        let (sink, table) = (self.presence.len() - 2, self.vocab.table.as_ptr());
        self.toks.reserve(self.pending.len());
        let (toks, mut len) = (self.toks.as_mut_ptr(), self.toks.len());
        for &(key, off) in &self.pending {
            // SAFETY: off is the byte offset of a slot of the table: Phf::slot gives
            // one below 2^(64 - shift), and get_long a long term's or the last one.
            let info = unsafe { probe(table.byte_add(off as usize), key) };
            let id = info & MISS;
            if info & ANCHOR != 0 {
                self.anchors.push(len as u32);
            }
            // SAFETY: room for every pending token was reserved above.
            unsafe { toks.add(len).write(id) };
            len += 1;
        }
        // SAFETY: the first len entries are written.
        unsafe { self.toks.set_len(len) };
        self.pending.clear();
        let (presence, touched) = (self.presence.as_mut_ptr(), self.touched.as_mut_ptr());
        let mut n = self.n;
        for &id in &self.toks {
            // Branch-free: the slot is always written at touched[n], and kept
            // (n advances) only when it is seen for the first time in this
            // block, its word still zero.
            let slot = (id as usize).min(sink);
            // SAFETY: slot <= sink < presence.len(), and n <= sink + 1 <
            // touched.len() = sink + 2: n counts distinct slots, at most sink + 1.
            unsafe {
                let x = *presence.add(slot);
                *touched.add(n) = slot as u32;
                n += (x == 0) as usize;
                *presence.add(slot) = x | bit;
            }
        }
        self.n = n;
    }

    /// Ends the current document: with the plan, counts the phrases anchored on
    /// its tokens that match; for the sample, its distinct terms.
    fn end_doc(&mut self) {
        let doc = self.doc;
        if let Some(plan) = self.plan {
            for &at in &self.anchors {
                let anchor = self.toks[at as usize] as usize;
                for i in plan.anchor[anchor] as usize..plan.anchor[anchor + 1] as usize {
                    let (answer, offset, start, len) = plan.anchored[i];
                    let phrase = &plan.terms[start as usize..(start + len) as usize];
                    let from = (at as usize).checked_sub(offset as usize);
                    let hit = from.and_then(|f| self.toks.get(f..f + phrase.len())) == Some(phrase);
                    if hit && self.last[i] != doc {
                        self.last[i] = doc;
                        // SAFETY: a phrase's answer is an answer number, below state.len().
                        unsafe { self.answers.add(answer, 1 << (doc % 64), doc & !63) };
                    }
                }
            }
        } else {
            // The sample: count the document's distinct terms.
            self.toks.sort_unstable();
            self.toks.dedup();
            for &t in self.toks.iter().filter(|&&t| t != MISS) {
                self.df[t as usize].fetch_add(1, Relaxed);
            }
        }
        self.toks.clear();
        self.anchors.clear();
    }

    /// Evaluates the queries triggered by the terms of this block, the 64
    /// documents from `base`, then moves its words to `wide` and clears it. The
    /// evaluation has no branch: whether a query matches is too random to predict.
    fn end_block(&mut self, base: u32) {
        let presence = &mut self.presence;
        if let Some(plan) = self.plan {
            // Two passes, so that the cold lines of an evaluation are fetched
            // well before it. The first keeps the entry ranges of the touched
            // slots that trigger any (about a third) and fetches their first
            // entries; the second, RANGES_AHEAD ranges ahead, fetches the words
            // and the state the first entry of a range reads, then evaluates it.
            let (touched, group) = (&self.touched[..self.n], plan.group.as_ptr());
            let (entries, work) = (plan.entries.as_ptr(), self.work.as_mut_ptr());
            let mut kept = 0;
            for (i, &t) in touched.iter().enumerate() {
                prefetch(group, *touched.get(i + SLOTS_AHEAD).unwrap_or(&0) as usize);
                // SAFETY: touched slots are at most bool_terms (the sink), and
                // group has bool_terms + 2 entries.
                let (from, to) = unsafe { (*group.add(t as usize), *group.add(t as usize + 1)) };
                // An empty range fetches entry 0 instead, which stays in L1.
                let first = if_zero((from ^ to) as u64, 0, from as u64);
                prefetch(entries, first as usize);
                // SAFETY: work has room for every touched slot.
                unsafe { work.add(kept).write((from, to)) };
                kept += (from != to) as usize;
            }
            // SAFETY: the first `kept` ranges were written.
            unsafe { self.work.set_len(kept) };
            let state = self.answers.state.as_ptr();
            for (k, &(from, to)) in self.work.iter().enumerate() {
                let next = self.work.get(k + RANGES_AHEAD).map_or(0, |r| r.0);
                // SAFETY: an entry of the plan (entry 0 at the end), which the
                // first pass fetched.
                let ahead = unsafe { &*entries.add(next as usize) };
                for &word in &ahead.terms {
                    prefetch(presence.as_ptr(), (word & TERM) as usize);
                }
                prefetch(state, ahead.answer as usize);
                // SAFETY: group holds ranges of entries.
                for entry in unsafe { plan.entries.get_unchecked(from as usize..to as usize) } {
                    // All ones when bit 0 of b is set: bit(op) for NOT and OR,
                    // bit(op >> 1) only for OR.
                    let bit = |b: u32| 0u64.wrapping_sub((b & 1) as u64);
                    let r = entry.terms.iter().fold(0u64, |r, &word| {
                        // SAFETY: every term word's slot is below bool_terms or the
                        // pad slot bool_terms + 1, and presence has bool_terms + 2.
                        let x = unsafe { *presence.get_unchecked((word & TERM) as usize) };
                        // AND: r & x. NOT: r & !x. OR: (r & !x) | x = r | x.
                        (r & (x ^ bit(word >> 30))) | (x & bit(word >> 31))
                    });
                    // A branch on the few entries with earlier cover terms: is one
                    // of them (bit k of `earlier_or_len`) present in the block?
                    let word = |k: usize| presence[(entry.terms[k] & TERM) as usize];
                    let present = |k| word(k) & bit(entry.earlier_or_len >> k) != 0;
                    let seen = entry.earlier_or_len != 0 && (0..MAX_BOOL_TERMS).any(present);
                    let hits = if seen { 0 } else { r };
                    // SAFETY: an entry's answer is an answer number, below state.len().
                    unsafe { self.answers.add(entry.answer, hits, base) };
                }
            }
        }
        // `block`: which of the superblock's 4 blocks this is.
        let (rows, block) = (self.wide.len() - 2, (base / 64 % (SUPER / 64)) as usize);
        let wide = self.wide.as_mut_ptr();
        for &t in &self.touched[..self.n] {
            // SAFETY: min(t, rows) < wide.len() = rows + 2, t <= sink <
            // presence.len(), and block < 4.
            unsafe {
                let row = &mut (*wide.add((t as usize).min(rows))).0;
                *row.get_unchecked_mut(block) = *presence.get_unchecked(t as usize);
                *presence.get_unchecked_mut(t as usize) = 0;
            }
        }
        self.n = 0;
    }

    /// Evaluates the `always` queries on the superblock of 256 documents from
    /// `base`, like `end_block` but on four words at once, then clears it.
    fn end_super(&mut self, base: u32) {
        let Some(plan) = self.plan else { return };
        let wide = self.wide.as_ptr() as *const __m256i;
        for entry in &plan.always {
            // For `always` entries, `earlier_or_len` holds the number of terms.
            let terms = &entry.terms[..entry.earlier_or_len as usize];
            // SAFETY: 32-byte loads of rows, which are 32-byte aligned (Row is
            // repr(align(32))); the terms of `always` entries are rows below
            // plan.rows < wide.len(). AVX2 is enabled for the whole build.
            let hits = unsafe {
                terms.iter().fold(_mm256_setzero_si256(), |r, &word| {
                    let x = _mm256_load_si256(wide.add((word & TERM) as usize));
                    match word >> 30 {
                        AND => _mm256_and_si256(r, x),
                        NOT => _mm256_andnot_si256(x, r),
                        _ => _mm256_or_si256(r, x),
                    }
                })
            };
            self.answers.add_wide(entry.answer, hits, base);
        }
        self.wide[..plan.rows].fill(Row([0; 4]));
    }
}

/// `len` zeros, written now: a lazily zeroed page that is read first faults
/// again when written, and that fault flushes the TLB of every thread.
fn written_zeros(len: usize) -> Vec<u64> {
    std::iter::repeat_n(std::hint::black_box(0), len).collect()
}

/// One `wide` row, aligned for 32-byte loads.
#[derive(Clone, Copy)]
#[repr(align(32))]
struct Row([u64; 4]);

/// One thread's answers.
struct Answers {
    /// Per answer: count << SUM_BITS | sum of the matching ids.
    state: Vec<u64>,
    /// `answer << 32 | id` for the first FIRST_IDS matches of each answer, in
    /// `log[..logged]`. Its zero pages cost no memory until they are written.
    log: Vec<u64>,
    logged: usize,
}

impl Answers {
    /// Adds the documents `base + i` for each bit i of `hits`, which may be 0.
    ///
    /// # Safety
    ///
    /// `answer < self.state.len()`: the state is indexed without a check.
    unsafe fn add(&mut self, answer: u32, hits: u64, base: u32) {
        // SAFETY: answer indexes the state (the caller's contract); an answer logs
        // at most FIRST_IDS ids, so logged <= FIRST_IDS * answers < log.len().
        let s = unsafe { &mut *self.state.as_mut_ptr().add(answer as usize) };
        let log = self.log.as_mut_ptr();
        let count = *s >> SUM_BITS;
        let entry = |bits: u64| (answer as u64) << 32 | (base + bits.trailing_zeros()) as u64;
        // The first id without a branch: always written, kept only if wanted.
        // SAFETY: as above.
        unsafe { *log.add(self.logged) = entry(hits) };
        self.logged += (count < FIRST_IDS && hits != 0) as usize;
        // Then up to FIRST_IDS in all, while hits are left.
        let (n, mut rest) = (hits.count_ones() as u64, hits & hits.wrapping_sub(1));
        for _ in count + 1..(count + n).min(FIRST_IDS) {
            // SAFETY: as above.
            unsafe { *log.add(self.logged) = entry(rest) };
            self.logged += 1;
            rest &= rest - 1;
        }
        // The bit positions' sum, one binary digit at a time: digit j is set in
        // the positions that `digit(j)` covers (0xAAAA..., 0xCCCC..., 0xF0F0...,
        // ..., 0xFFFF_FFFF_0000_0000), constants once the loop is unrolled.
        let digit = |j: u32| !0u64 / ((1 << (1 << j)) + 1) << (1 << j);
        let positions = (0..6).map(|j| ((hits & digit(j)).count_ones() as u64) << j);
        *s += n << SUM_BITS | (n * base as u64 + positions.sum::<u64>());
    }

    /// `add` for the 256 documents from `base`, one word of `hits` per 64.
    fn add_wide(&mut self, answer: u32, hits: __m256i, base: u32) {
        // SAFETY: AVX2 is enabled for the whole build.
        let has_hits = |v| unsafe { _mm256_testz_si256(v, v) } == 0;
        if self.state[answer as usize] >> SUM_BITS >= FIRST_IDS {
            let (n, positions) = count_positions(hits);
            self.state[answer as usize] += n << SUM_BITS | (n * base as u64 + positions);
        } else if has_hits(hits) {
            // Still logging first ids: a word at a time.
            // SAFETY: __m256i and [u64; 4] have the same size, and any bits are valid.
            let words: [u64; 4] = unsafe { std::mem::transmute(hits) };
            // SAFETY: the state index above checked answer < state.len().
            (0..4).for_each(|i| unsafe { self.add(answer, words[i], base + 64 * i as u32) });
        }
    }
}

/// The number of set bits in `v`, and the sum of their positions, from tables of
/// the 4-bit nibbles: the nibble at bits 4m..4m + 4 adds 4m per set bit, plus its
/// own bit positions.
fn count_positions(v: __m256i) -> (u64, u64) {
    // SAFETY: AVX2 is enabled for the whole build; the loads read the 16-byte
    // tables and the 32 bytes of EIGHT_B; __m256i and [u32; 8] have the same size,
    // and any bits are valid.
    unsafe {
        let table = |t: &[u8; 16]| _mm256_broadcastsi128_si256(_mm_loadu_si128(t.as_ptr().cast()));
        // `pop`: the set bits of a nibble.
        let pop = table(&[0, 1, 1, 2, 1, 2, 2, 3, 1, 2, 2, 3, 2, 3, 3, 4]);
        // The bit positions of a low nibble, and of a high one (4 more per bit).
        let low = table(&[0, 0, 1, 1, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6]);
        let high = table(&[0, 4, 5, 9, 6, 10, 11, 15, 7, 11, 12, 16, 13, 17, 18, 22]);
        let nibble = _mm256_set1_epi8(0x0F);
        let (lo, hi) = (_mm256_and_si256(v, nibble), _mm256_srli_epi16::<4>(v));
        let hi = _mm256_and_si256(hi, nibble);
        let count = _mm256_add_epi8(_mm256_shuffle_epi8(pop, lo), _mm256_shuffle_epi8(pop, hi));
        let own = _mm256_add_epi8(_mm256_shuffle_epi8(low, lo), _mm256_shuffle_epi8(high, hi));
        // Byte b adds 8b per set bit: 16-bit sums of byte pairs, then 32-bit sums,
        // with the count added at bit 16 (the position sum is below 2^15).
        // maddubs reads its first operand as unsigned bytes, 0 to 248 here.
        let eight_b = _mm256_loadu_si256(EIGHT_B.as_ptr().cast());
        let (ones8, ones16) = (_mm256_set1_epi8(1), _mm256_set1_epi16(1));
        let pos = _mm256_maddubs_epi16(eight_b, count);
        let pos = _mm256_add_epi16(pos, _mm256_maddubs_epi16(own, ones8));
        let counts = _mm256_madd_epi16(_mm256_maddubs_epi16(count, ones8), ones16);
        let pos = _mm256_madd_epi16(pos, ones16);
        let sums = _mm256_add_epi32(pos, _mm256_slli_epi32::<16>(counts));
        // The eight 32-bit sums added up (they cannot overflow 32 bits).
        let total = std::mem::transmute::<_, [u32; 8]>(sums).iter().sum::<u32>() as u64;
        (total >> 16, total & 0xFFFF)
    }
}

/// 8b at byte b.
static EIGHT_B: [u8; 32] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120, 128, 136, 144, 152, 160, 168,
    176, 184, 192, 200, 208, 216, 224, 232, 240, 248,
];

/// The masks of the 64 bytes from `at`: bit i of the first is set when byte
/// `at + i` is a token byte ([A-Za-z0-9]) before `end`, of the second when it is LF.
fn masks(docs: &[u8], at: usize, end: usize) -> (u64, u64) {
    let half = |at: usize| {
        let bytes = &docs[at..at + 32];
        // SAFETY: an unaligned 32-byte load from a 32-byte slice; AVX2 is enabled
        // for the whole build.
        let (token, lf) = unsafe {
            let v = _mm256_loadu_si256(bytes.as_ptr() as *const __m256i);
            let at_least = |x, lo: u8| _mm256_cmpgt_epi8(x, _mm256_set1_epi8(lo as i8 - 1));
            let at_most = |x, hi: u8| _mm256_cmpgt_epi8(_mm256_set1_epi8(hi as i8 + 1), x);
            let between = |x, lo, hi| _mm256_and_si256(at_least(x, lo), at_most(x, hi));
            // Signed compares: bytes 0x80 and up are negative, never letters or
            // digits. Letters are tested on v | 0x20, digits on the raw byte.
            let letter = between(_mm256_or_si256(v, _mm256_set1_epi8(0x20)), b'a', b'z');
            let lf = _mm256_cmpeq_epi8(v, _mm256_set1_epi8(b'\n' as i8));
            (_mm256_or_si256(letter, between(v, b'0', b'9')), lf)
        };
        (movemask(token) as u64, movemask(lf) as u64)
    };
    let ((token0, lf0), (token1, lf1)) = (half(at), half(at + 32));
    // SAFETY: BMI2 is enabled for the whole build. bzhi keeps the bits below
    // its index, all of them from 64 on.
    let token = unsafe { _bzhi_u64(token0 | token1 << 32, end.saturating_sub(at).min(64) as u32) };
    (token, lf0 | lf1 << 32)
}

/// Fetches the cache line of `base[i]` into L1.
fn prefetch<T>(base: *const T, i: usize) {
    // SAFETY: a prefetch reads nothing and never faults, whatever the address.
    unsafe { _mm_prefetch::<_MM_HINT_T0>(base.wrapping_add(i).cast()) }
}

/// `if c == 0 { a } else { b }` as a cmov: LLVM turns such selects of loaded
/// values into branches, which mispredict when c is random.
fn if_zero(c: u64, a: u64, b: u64) -> u64 {
    let mut r = b;
    // SAFETY: register-only instructions.
    unsafe {
        asm!("test {c}, {c}", "cmovz {r}, {a}", c = in(reg) c, a = in(reg) a, r = inout(reg) r, options(pure, nomem, nostack))
    };
    r
}

/// `vpmovmskb` hidden from LLVM: on CPUs without AVX-512 it rewrites the
/// combination of two movemasks into a bit-by-bit extraction that costs a quarter
/// of the runtime.
fn movemask(v: __m256i) -> u32 {
    let m: u32;
    // SAFETY: AVX2 is enabled for this build; the instruction reads one register.
    unsafe { asm!("vpmovmskb {:e}, {}", out(reg) m, in(ymm_reg) v, options(pure, nomem, nostack)) };
    m
}

/// Splits the corpus into about `parts` ranges of whole lines. A block or
/// superblock may straddle two slices: each thread sees the other's documents
/// as containing no term, so they match nothing there.
/// Returns (start, end, first id) per slice.
fn slices(text: &[u8], parts: usize) -> Vec<(usize, usize, u32)> {
    // The cuts from 0 to text.len(), without repeats: slices are never empty.
    let cut = |p: usize| line_after(text, text.len() * p / parts);
    let mut cuts: Vec<usize> = (0..=parts).map(cut).collect();
    cuts.dedup();
    let id = |at: usize| {
        let tab = text[at..].iter().position(|&b| b == b'\t');
        parse_id(&text[at..at + tab.expect("docs.txt: a line without a TAB")])
    };
    cuts.windows(2).map(|c| (c[0], c[1], id(c[0]))).collect()
}

/// The start of the line after the one that holds byte `at`, or the end of
/// `text`; 0 for `at` = 0, so that splitting at these offsets keeps line 0.
fn line_after(text: &[u8], at: usize) -> usize {
    let at = at.min(text.len());
    let next = text[at..].iter().position(|&b| b == b'\n');
    let next = next.map_or(text.len(), |i| at + i + 1);
    if at == 0 { 0 } else { next }
}

/// A document id: 1 to 9 digits, so that it fits in a u32.
fn parse_id(digits: &[u8]) -> u32 {
    let ok = !digits.is_empty() && digits.len() <= 9 && digits.iter().all(u8::is_ascii_digit);
    assert!(ok, "docs.txt: bad id {:?}", String::from_utf8_lossy(digits));
    digits.iter().fold(0, |n, &b| n * 10 + (b - b'0') as u32)
}

/// Appends `value` in decimal, then a space.
fn push_decimal(out: &mut Vec<u8>, mut value: u64) {
    let digits = value.checked_ilog10().unwrap_or(0) as usize + 1;
    out.resize(out.len() + digits + 1, b' ');
    for digit in out.iter_mut().rev().skip(1).take(digits) {
        *digit = b'0' + (value % 10) as u8;
        value /= 10;
    }
}
