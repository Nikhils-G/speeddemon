//! Track B (Index): answer 50,000 BOOL and PHRASE queries over a 254 MB corpus.
//!
//! There is no index: the corpus is scanned once, in parallel slices, and every
//! query is answered during that scan, so nothing grows with the corpus.
//! 1. The queries come first. Only their terms are looked up, in a perfect hash
//!    of them: one 16-byte slot per term, fetched in advance. Each document's
//!    tokens are found first, then looked up.
//! 2. A BOOL query is evaluated when a document ends, from a truth table over
//!    the presence of its terms in the document, made when the queries are
//!    planned: AND, OR and NOT, strictly left to right, are then one table
//!    lookup, done for 8 entries at once in AVX2 (`trigger`). Each term keeps a
//!    one-byte stamp of its last document (see STAMPS), so presence is one compare.
//! 3. A query is evaluated only in documents that contain one of its cover
//!    terms, a set of terms every match contains (see `cover`), chosen to be rare
//!    from the sample. Queries with common covers are evaluated in every
//!    superblock of 256 documents instead: each of their terms gets a row of
//!    four 64-bit words, one bit per document, and AND, OR and NOT are AVX2
//!    operations on one vector per term.
//! 4. Each phrase is anchored on its rarest term and checked against the token
//!    ids of the document when it ends, while they are still in L1.
//! 5. Per query and thread, the scan keeps the match count, the sum of the ids
//!    (from popcounts of bit positions) and the first ten ids. The threads' slices
//!    are in document order, so merging them in order gives the ten smallest.
//!
//! Modules: `vocab` (1), `plan` (what each query needs, 2 to 4), `scan` (the scan
//! and 5), `pool` (the threads of `par`) and `mmap` (the system calls).

// The C runtime calls `main` below directly: std's start-up (the main thread's stack
// guard, found by reading /proc/self/maps, and signal handlers with an alternate signal
// stack, which every spawned thread then maps too) is not needed, as nothing here
// recurses deeply. Panics still print their message, then abort (panic = "abort").
#![no_main]

mod mmap;
mod plan;
mod pool;
mod scan;
mod vocab;

use {plan::*, pool::*, scan::*, vocab::*};

use std::arch::asm;
use std::arch::x86_64::*;
use std::cmp::Reverse;
use std::mem::{replace, transmute};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicUsize};
use std::sync::{Barrier, LazyLock, Mutex, OnceLock, RwLock};
use std::thread::spawn;

#[cfg(not(all(target_feature = "avx2", target_feature = "bmi2")))]
compile_error!("track-b needs AVX2 and BMI2: build with RUSTFLAGS=\"-C target-cpu=native\"");
#[cfg(not(target_feature = "aes"))]
compile_error!("track-b needs AES-NI: build with RUSTFLAGS=\"-C target-cpu=native\"");

/// A token's info word: its term id (ID masks it), plus ANCHOR if a phrase is
/// anchored on it and TRIG if it triggers BOOL entries. A token that no query
/// uses gets the id `Vocab::count`, which no term has.
const ANCHOR: u32 = 1 << 31;
const TRIG: u32 = 1 << 30;
const ID: u32 = TRIG - 1;
/// A document's stamp is its id modulo STAMPS, and each term keeps the stamp of
/// its last document (`Scan::lastdoc`): one byte per term, a quarter of a u32.
/// The bytes are reset to EMPTY every STAMPS documents, so an equal stamp is
/// always this document's. DONE marks a trigger term whose entries `trigger` has
/// already listed for this document.
const STAMPS: u32 = 127;
const EMPTY: u8 = 0xFF;
const DONE: u8 = 0x80;
/// Terms of up to KEY bytes live in the perfect hash, keyed by their bytes.
const KEY: usize = 12;
/// The spec's grammar: a BOOL query has 1 to 6 terms.
const MAX_BOOL_TERMS: usize = 6;
/// A BOOL term word is `slot | op << 30`, and TERM masks the slot.
const TERM: u32 = (1 << 30) - 1;
const AND: u32 = 0;
const NOT: u32 = 1;
const OR: u32 = 3;
/// The spec's answer: the match count, the sum of the matching ids, and the
/// first (smallest) FIRST_IDS ids.
const FIRST_IDS: u64 = 10;
/// Corpus bytes sampled (spread over the slices) to estimate term frequencies:
/// about 2,600 documents on seed 1. The better plan pays for the longer sample.
/// On the replica (PERF.md, experiments), against 1 MB, 2.5 MB ran 0.44-0.74 ms
/// faster on seeds 1 and 99, 1.5 MB the same, 768 KB 0.4-0.8 ms slower and
/// 512 KB 2.1-2.5 ms slower; 2, 3 and 3.5 MB were no faster than 2.5 MB.
const SAMPLE_BYTES: usize = 5 << 19;
/// A query whose cover terms are expected in more 64-document blocks than this
/// (as a share, summed over the terms) is evaluated in every superblock instead
/// of per document. Raised from 0.30 to 0.40 once `end_super` read one 32-byte
/// row per term: on the replica (PERF.md, experiments), against 0.30, 0.40 ran
/// 0.45-0.48 ms faster on seed 1 and 0.35-0.52 ms on seed 99 (PGO, 26 to 28 of 31
/// pairs). Without PGO: 0.25 ran 0.75 ms slower, 0.35 0.35 ms faster, 0.40 0.75,
/// 0.45 0.47 and 0.50 0.36 ms faster, 0.60 0.23 ms slower. (Set at 0.30 in step
/// 53; on the earlier block engine 0.25 beat 0.125, 0.30 and 0.5.)
const ALWAYS: f64 = 0.40;
/// Documents per superblock: four blocks of 64, one AVX2 vector per row. 128 ran
/// 12.4% slower on step 66's code (half-empty vectors, evaluated twice as often),
/// and 512 (two vectors a row) read 6.5 ms slower in a box probe on step 50's
/// (PERF.md, experiments, and steps 50 and 51).
const SUPER: u32 = 256;
/// A thread's sum of matching ids is packed under its count in one u64.
const SUM_BITS: u32 = 40;
/// Document ids must be below this: a thread's sum of ids is then below
/// MAX_DOCS^2 / 2 = 2^39, which fits in SUM_BITS.
const MAX_DOCS: u32 = 1 << 20;
/// A first-id log entry is `answer << 16 | id`, the id's low 16 bits, so an answer
/// is below 2^16 (the spec has 50,000 queries) and a slice holds at most 2^16
/// documents (`slices`): the corpus is cut into at least this many slices (run in
/// turns on fewer threads), at most about 31,000 documents each at the spec's
/// 250,000 when the lines are about equally long.
const MIN_SLICES: usize = 8;
/// Hash groups: a key's group is the low 6 bits of its hash (hash & 63). A pilot
/// is 1 mod GROUPS, so each group owns a region of the table, and the threads
/// place their groups in parallel.
const GROUPS: usize = 64;

// SAFETY: the one symbol named `main`, with the C signature the C runtime calls it by.
#[unsafe(no_mangle)]
extern "C" fn main(_: i32, _: *const *const u8) -> i32 {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    mmap::set_up_memory();
    // Joined with concat rather than format!: that code size keeps the scan's hot functions at
    // their 64-byte offsets of before no_main (format! moved them by 32: 0.085 ms slower).
    let open = |name: &str| mmap::map(&[&data_dir, "/", name].concat());
    let docs = open("docs.txt");
    // The corpus: `docs` without the mmap::PAD zero bytes that follow it.
    let text = &docs[..docs.len() - mmap::PAD];
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    // Waiting workers map the corpus in ahead of the scan (see `ahead`).
    _ = AHEAD.set((text, threads.max(MIN_SLICES)));
    let (queries, mut vocab) = parse_queries(open("queries.txt"), threads);
    let slices = slices(text, threads.max(MIN_SLICES));

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
    let plan = Plan::new(queries, &df, sampled.iter().sum::<u32>().max(1));
    // An answer number fits the first-id log's 16 bits (see MIN_SLICES).
    assert!(plan.line_of.len() <= 1 << 16, "over 2^16 queries");
    vocab.finish(&plan);

    // The output is formatted in `parts` ranges of lines.
    let parts = slices.len().max(1);
    // Per answer, whether the first slice alone has FIRST_IDS ids. Then the later
    // slices' ids are never printed, so their threads skip the answer: most of
    // their logs, as such answers are the frequent ones.
    let full = OnceLock::new();
    let results = par(slices.len(), &|i| {
        let (start, end, first) = slices[i];
        // Only the answers and the last id are kept: the scan's other tables
        // are freed here, and the first ids below reuse their memory.
        let (Answers { state, log, logged }, doc) = {
            // SAFETY: end <= text.len(), and mmap::PAD (256) bytes follow text.
            let scan = unsafe { Scan::new(&vocab, Some(&plan), &[]).run(docs, start, end, first) };
            (scan.answers, scan.doc)
        };
        // The ids go on in the next slice, and stay below MAX_DOCS.
        let next = slices.get(i + 1).map_or(doc + 1, |s| s.2);
        // A slice's ids also fit the first-id log (see MIN_SLICES).
        let ok = slices[0].2 == 0 && next == doc + 1 && doc < MAX_DOCS && doc - first < 1 << 16;
        assert!(ok, "ids 0, 1, ... below 2^20, 2^16 a slice");
        if i == 0 {
            let is_full = |&s: &u64| s >> SUM_BITS >= FIRST_IDS;
            full.get_or_init(|| state.iter().map(is_full).collect::<Vec<_>>());
        }
        // The other threads wait here for the first slice's scan.
        let full: &[bool] = full.wait();
        let keep = |a: u32| (i == 0 || !full[a as usize]) as u32;
        // The first ids by line number, without a sort: an answer logged the
        // first min(count, FIRST_IDS) of its ids, in order, so each line's place in
        // the result is known from the counts. `place` gives a line the number n
        // of ids kept before it, then adds its own to n. That start advances as the
        // line's ids are placed, and ends where the next line's ids start.
        let logged_of = |a: u32| (state[a as usize] >> SUM_BITS).min(FIRST_IDS) as u32 * keep(a);
        let mut n = 0;
        let place = |&a: &u32| {
            let start = n;
            n += logged_of(a);
            start
        };
        let mut ends: Vec<u32> = plan.answer_of.iter().map(place).collect();
        let mut ids = vec![0; n as usize];
        for &e in log[..logged].iter().filter(|&&e| keep(e >> 16) == 1) {
            let line = plan.line_of[(e >> 16) as usize] as usize;
            // The id: the slice's first, plus the low 16 bits of the difference.
            ids[ends[line] as usize] = first + (e.wrapping_sub(first) & 0xFFFF);
            ends[line] += 1;
        }
        // The log is a block over 1 MB, an mmap of its own: dropped, the 12 threads'
        // unmaps, which take mmap_lock for writing, ran one after the other (0.5 ms,
        // the replica). Its pages go back now (madvise takes the lock for reading),
        // and the mapping is left to the exit.
        // SAFETY: the log is not read again.
        unsafe { mmap::release(&log) };
        std::mem::forget(log);
        Results { state, ids, ends }
    });

    // Formatted in parallel, one range of lines per thread, then written in order.
    // A reference to the path, not the String: that code size keeps the scan's hot functions at
    // their 64-byte offsets (al.sh) now that Scan::new is smaller.
    let file = std::fs::File::create(&format!("{out_dir}/results.txt")).expect("results.txt");
    let chunks = par(parts, &|p| format_part(&plan, &results, p, parts));
    // This thread writes them while the workers drop the corpus's pages and give the
    // big tables back to the kernel (not the plan's 0.6 MB of entries: that release did
    // not pay for its line), none of which is read again: the exit has fewer to free.
    par(parts, &|p| {
        if p == 0 {
            for chunk in &chunks {
                std::io::Write::write_all(&mut &file, chunk).expect("write results.txt");
                // SAFETY: written, and not read again before the process exits.
                unsafe { mmap::release(chunk) };
            }
            return;
        }
        let share = |len| part(len, p - 1, parts - 1);
        // SAFETY: none of these is read again before the process exits.
        unsafe { mmap::release(&text[share(text.len())]) };
        for r in &results[share(results.len())] {
            // SAFETY: as above.
            unsafe { mmap::release(&r.state) };
            // SAFETY: as above.
            unsafe { mmap::release(&r.ends) };
            // SAFETY: as above.
            unsafe { mmap::release(&r.ids) };
        }
        // SAFETY: as above.
        unsafe { mmap::release(&vocab.table[share(vocab.table.len())]) };
        // SAFETY: as above.
        unsafe { mmap::release(&plan.anchored[share(plan.anchored.len())]) };
    });
    // Done: exit without freeing the tables and logs, one free at a time.
    std::process::exit(0)
}

/// One thread's answers, after its scan.
struct Results {
    /// Per answer: count << SUM_BITS | sum of the matching ids.
    state: Vec<u64>,
    /// The first ids it kept of each answer, min(count, FIRST_IDS) of them (none
    /// if the first slice has FIRST_IDS), the answers in line order, and where
    /// each line's ids end in them.
    ids: Vec<u32>,
    ends: Vec<u32>,
}

/// The output lines of the p-th of `parts` ranges, from each thread's results.
/// Per line: the threads' counts and sums added up (each state read through
/// `answer_of`: no copy of the states, which would cost 4.5 MB), then the first
/// ids taken from the threads in slice order. The packed states add up as they
/// are: all ids are below MAX_DOCS = 2^20, so any sum of them is below 2^39 and
/// never carries into the count.
fn format_part<'a>(plan: &Plan, results: &'a [Results], p: usize, parts: usize) -> Vec<u8> {
    let lines = part(plan.answer_of.len(), p, parts);
    // About 32 output bytes per line: a capacity only.
    let mut out = Vec::with_capacity(lines.len() * 32);
    for line in lines {
        let answer = plan.answer_of[line] as usize;
        let s: u64 = results.iter().map(|r| r.state[answer]).sum();
        push_decimal(&mut out, s >> SUM_BITS);
        push_decimal(&mut out, s & ((1 << SUM_BITS) - 1));
        // Then every thread's first ids of the line, in slice order. Slices are in
        // document order, so the first FIRST_IDS of them are the line's.
        let from = |r: &Results| line.checked_sub(1).map_or(0, |l| r.ends[l]) as usize;
        let ids = |r: &'a Results| &r.ids[from(r)..r.ends[line] as usize];
        for &id in results.iter().flat_map(ids).take(FIRST_IDS as usize) {
            push_decimal(&mut out, id as u64);
        }
        // push_decimal ends each number with a space: the last one becomes the
        // LF (a line has at least its count and sum, so `out` is not empty).
        *out.last_mut().unwrap() = b'\n';
    }
    out
}

/// The p-th of `parts` near-equal ranges of 0..len.
fn part(len: usize, p: usize, parts: usize) -> std::ops::Range<usize> {
    len * p / parts..len * (p + 1) / parts
}

/// Splits the corpus into about `parts` ranges of whole lines, and a range of
/// more than 2^16 documents (short lines) into more: a slice holds at most 2^16
/// (MIN_SLICES). A block or superblock may straddle two slices: each thread sees
/// the other's documents as containing no term, so they match nothing there.
/// Returns (start, end, first id) per slice.
fn slices(text: &[u8], parts: usize) -> Vec<(usize, usize, u32)> {
    // The cuts from 0 to text.len(), without repeats: slices are never empty.
    let cut = |p: usize| line_after(text, text.len() * p / parts);
    let mut cuts: Vec<usize> = (0..=parts).map(cut).collect();
    cuts.dedup();
    // A line's id: its bytes before the TAB, or before the LF if it has none (such a
    // line then stops the program: in parse_id, or in the scan, which wants the TAB).
    let id = |line: &[u8]| parse_id(line.split(|&b| b == b'\t' || b == b'\n').next().unwrap());
    // The number of lines: the last line's id + 1.
    let last = text.rsplit(|&b| b == b'\n').nth(1);
    let lines = last.map_or(0, |line| id(line) + 1);
    let mut out = Vec::new();
    for c in cuts.windows(2) {
        let (mut start, mut first) = (c[0], id(&text[c[0]..]));
        // The next range's first id, or the number of lines after the last range.
        let rest = &text[c[1]..];
        let end = if rest.is_empty() { lines } else { id(rest) };
        while end > first + (1 << 16) {
            let next = text[start..].split_inclusive(|&b| b == b'\n').take(1 << 16);
            let cut = start + next.map(<[u8]>::len).sum::<usize>();
            out.push((start, cut, first));
            (start, first) = (cut, first + (1 << 16));
        }
        out.push((start, c[1], first));
    }
    out
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
