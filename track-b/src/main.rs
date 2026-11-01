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
//!    lookup. Each term keeps a one-byte stamp of its last document (see
//!    STAMPS), so presence is one compare.
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

mod mmap;

use std::arch::asm;
use std::arch::x86_64::*;
use std::cmp::Reverse;
use std::mem::{replace, take, transmute};
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
/// always this document's. DONE marks a trigger term that `end_doc` has taken.
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
/// about 1,030 documents on seed 1. (2 MB planned no better and took 1.4 ms more;
/// 512 KB planned worse.)
const SAMPLE_BYTES: usize = 1 << 20;
/// A query whose cover terms are expected in more 64-document blocks than this
/// (as a share, summed over the terms) is evaluated in every superblock instead
/// of per document. Chosen on the box: an in-process probe of the per-document
/// design read 0.30 2.3 ms faster than 0.25 (PERF.md, step 53); the replica could
/// not tell them apart (−0.33%, 31 pairs), and found 0.20 and 0.35 slower. (On the
/// earlier block engine 0.25 beat 0.125, 0.30 and 0.5; 512-document superblocks
/// were 6.5 ms slower.)
const ALWAYS: f64 = 0.30;
/// Documents per superblock: four blocks of 64.
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

fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    mmap::set_up_memory();
    let open = |name: &str| mmap::map(&format!("{data_dir}/{name}"));
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
    let plan = Plan::new(queries, &df, sampled.iter().sum::<u32>().max(1), threads);
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
        // Drop the slice's pages now, in parallel, instead of the kernel
        // unmapping all 62,000 at exit on one core.
        // SAFETY: docs[start..end] is part of the mapping made by mmap::map.
        unsafe { mmap::release(&docs[start..end]) };
        // The ids go on in the next slice, and stay below MAX_DOCS.
        let next = slices.get(i + 1).map_or(doc + 1, |s| s.2);
        // A slice's ids, and the queries, also fit the first-id log (see MIN_SLICES).
        let fits = doc - first < 1 << 16 && plan.line_of.len() <= 1 << 16;
        let ok = slices[0].2 == 0 && next == doc + 1 && doc < MAX_DOCS && fits;
        assert!(ok, "ids 0, 1, ... below 2^20, 2^16 a slice; 2^16 queries");
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
        let place = |&a: &u32| (n, n += logged_of(a)).0;
        let mut ends: Vec<u32> = plan.answer_of.iter().map(place).collect();
        let mut ids = vec![0; n as usize];
        for &e in log[..logged].iter().filter(|&&e| keep(e >> 16) == 1) {
            let line = plan.line_of[(e >> 16) as usize] as usize;
            // The id: the slice's first, plus the low 16 bits of the difference.
            ids[ends[line] as usize] = first + (e.wrapping_sub(first) & 0xFFFF);
            ends[line] += 1;
        }
        Results { state, ids, ends }
    });

    // Formatted in parallel, one range of lines per thread, then written in order.
    let mut file = std::fs::File::create(format!("{out_dir}/results.txt")).expect("results.txt");
    for chunk in par(parts, &|p| format_part(&plan, &results, p, parts)) {
        std::io::Write::write_all(&mut file, &chunk).expect("write results.txt");
    }
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

/// f(0), f(1), ... f(n - 1): f(0) on this thread and f(i) on worker i of a pool
/// made by the first call (parts beyond it run on this thread). Between calls the
/// workers wait (`wait`): a call spawns no thread, each worker keeps its heap
/// arena, and its CPU stays out of the deep idle states that take up to 1 ms to
/// wake from.
fn par<T: Send + Sync>(n: usize, f: &(impl Fn(usize) -> T + Sync)) -> Vec<T> {
    // One call at a time: the job slot and the counters below serve one job, so an
    // overlapping call (from another thread, or from a job) would mix two jobs' counts
    // and could return while a worker still runs its job. The program calls par from
    // the main thread only; this makes a misuse stop instead of corrupting memory.
    assert!(!BUSY.swap(true, Acquire), "par: one call at a time");
    let out: Vec<_> = (0..n).map(|_| OnceLock::new()).collect();
    // Part i, if there is one: every worker runs the job with its i.
    let job = |i: usize| _ = out.get(i).map(|r| r.set(f(i)));
    let job: &(dyn Fn(usize) + Sync) = &job;
    // SAFETY: the lifetime is erased so that the static can hold the job. A worker
    // reads JOB only after GENERATION reaches this call's generation, which is
    // published after this store, and holds the read lock until it has run the job;
    // it then counts it in FINISHED. This call returns only once every worker has, so
    // no worker uses the job after it is dropped. The reference left in JOB afterwards
    // is never read: the next call replaces it before it moves GENERATION on.
    *JOB.write().unwrap() = Some(unsafe { transmute(job) });
    let generation = GENERATION.fetch_add(1, Release) + 1;
    let workers = *WORKERS.get_or_init(|| (1..n).map(|i| spawn(move || worker(i))).count());
    (0..n).filter(|&i| i == 0 || i > workers).for_each(job);
    wait(&FINISHED, |f| f >= generation * workers);
    BUSY.store(false, Release);
    out.into_iter().flat_map(OnceLock::into_inner).collect()
}

/// The pool's job, its lifetime erased (see `par`).
static JOB: RwLock<Option<&'static (dyn Fn(usize) + Sync)>> = RwLock::new(None);
/// Set while a call to `par` runs.
static BUSY: AtomicBool = AtomicBool::new(false);
/// The number of jobs so far, the number of times a worker has finished one,
/// and the number of workers.
static GENERATION: AtomicUsize = AtomicUsize::new(0);
static FINISHED: AtomicUsize = AtomicUsize::new(0);
static WORKERS: OnceLock<usize> = OnceLock::new();
/// Whether the CPU has WAITPKG (CPUID leaf 7, ECX bit 5), for `wait`.
static WAITPKG: LazyLock<bool> = LazyLock::new(|| __cpuid_count(7, 0).ecx & 1 << 5 != 0);

/// Waits until `done(word)`: in C0.2 (umwait) on a CPU with WAITPKG, woken by a
/// write to the word's cache line or by the kernel's limit (100,000 TSC cycles),
/// so a waiting thread leaves its core to its SMT sibling (in the phase probe,
/// spinning workers with `pause` gained 1.19 ms over spawning threads, umwait
/// 1.29; PERF.md, step 55). Without WAITPKG, it polls.
fn wait(word: &AtomicUsize, done: impl Fn(usize) -> bool) {
    while !done(word.load(Acquire)) && *WAITPKG {
        // SAFETY: the CPU has WAITPKG. umonitor arms the monitor on the word's
        // address, and umwait (deadline u64::MAX, so the kernel's) waits on it
        // unless the word changed meanwhile; neither writes memory.
        unsafe { asm!("umonitor {}", in(reg) word.as_ptr(), options(nostack, preserves_flags)) };
        if !done(word.load(Acquire)) {
            // SAFETY: as above.
            unsafe {
                asm!(
                    "umwait {:e}", in(reg) 0, in("edx") u32::MAX, in("eax") u32::MAX,
                    options(nostack)
                )
            };
        }
    }
    while !done(word.load(Acquire)) {
        std::hint::spin_loop();
    }
}

/// The corpus and the number of equal parts it is mapped in by (main's nominal
/// number of slices), once known: workers that wait for a job map its pages in
/// ahead of the scan (`ahead`). And the next chunk to map.
static AHEAD: OnceLock<(&'static [u8], usize)> = OnceLock::new();
static AHEAD_NEXT: AtomicUsize = AtomicUsize::new(0);

/// Maps in the next 64 KB of the corpus, for a worker that waits for a job, so
/// that the scan finds its pages mapped (it would take the same page faults).
/// Chunk k is the (k / n)-th 64 KB of the (k % n)-th of n equal parts, so every
/// slice gains alike. None once all are mapped. (madvise(MADV_POPULATE_READ)
/// instead held mmap_lock and stalled the main thread's allocations; a page fault
/// takes only its VMA's lock.)
fn ahead() -> Option<()> {
    let &(text, n) = AHEAD.get()?;
    let (k, part) = (AHEAD_NEXT.fetch_add(1, Relaxed), text.len().div_ceil(n));
    let at = k % n * part + (k / n << 16);
    // One read: the kernel maps the 16 pages around it (fault-around).
    text.get(at).map(|&b| std::hint::black_box(b));
    (k / n << 16 < part).then_some(())
}

/// Worker i of `par`'s pool: runs each job with its i.
fn worker(i: usize) {
    for generation in 1.. {
        while GENERATION.load(Acquire) < generation && ahead().is_some() {}
        wait(&GENERATION, |g| g >= generation);
        // The read lock is held until the job is done: `par` writes the next job
        // only once every worker has finished this one.
        JOB.read().unwrap().unwrap()(i);
        FINISHED.fetch_add(1, Release);
    }
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
/// hashes (attempt `seed`'s: see SEEDS) of the terms' table keys (the zero key
/// for a long term) with their offsets and key lengths, grouped by hash & 63
/// (`starts`), and the long terms. (The long terms' zero key gets an id that no
/// term uses.) The
/// vectors are sized for the most terms a chunk can hold (a 1-byte term and its
/// space: len / 2) and the most lines (a line is at least "BOOL a" and LF, 7
/// bytes, under most / 3), so they never grow: their untouched pages cost nothing.
fn split_lines(chunk: &'static [u8], padded: &'static [u8], seed: u64) -> Split {
    let seed = seed.wrapping_mul(SEEDS);
    let most = chunk.len() / 2 + 1;
    let (mut lines, mut long) = (Vec::with_capacity(most / 3), Vec::new());
    let mut terms = Vec::with_capacity(most);
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
            } else {
                // `AND`, `NOT` and `OR` start with bytes ending in binary 01, 10 and 11.
                // queries.txt is trusted (spec 2.3): any other word reads as AND
                // (index 0).
                op = [0, AND, NOT, OR][(word[0] & 3) as usize];
            }
        }
        lines.push((start as u32, (terms.len() - start) as u32, phrase));
    }
    // The terms' keys grouped by hash & 63, the hash group. Key length 0 gives
    // the zero key, which marks a long term.
    let key = |&(at, len): &Term| {
        let len = (len & TERM) * ((len & TERM) as usize <= KEY) as u32;
        let hash = hash(checked_key(padded, at as usize, len as usize), seed);
        ((hash & 63) as u32, Key { hash, at, len })
    };
    let (starts, keys) = csr(|| terms.iter().map(key), GROUPS);
    Split {
        lines,
        terms,
        starts,
        keys,
        long,
    }
}

/// One thread's share of the query lines, from `split_lines`.
struct Split {
    /// Its lines, as spans of its terms from 0.
    lines: Vec<Span>,
    terms: Vec<Term>,
    /// Its keys, those of hash group g in `keys[starts[g]..starts[g + 1]]`.
    starts: Vec<u32>,
    keys: Vec<Key>,
    /// Its terms over KEY bytes.
    long: Vec<Long>,
}
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
    // the full seed-1, holdout seed-99, sample seed 1 to 4 and 18 fresh full
    // inputs; over 300 seeds on each of those 24, none needed a second seed.
    let (vocab, ids, splits) = (0..).find_map(|s| Vocab::new(split(s), s, padded)).unwrap();
    // The spec's 50,000 queries, about 155,000 terms on seed 1: capacities only.
    let (mut lines, mut terms) = (Vec::with_capacity(50_000), Vec::with_capacity(160_000));
    for (Split { lines: spans, .. }, ids) in splits.iter().zip(ids) {
        let base = terms.len() as u32;
        lines.extend(spans.iter().map(|&(s, n, p)| (base + s, n, p)));
        terms.extend(ids);
    }
    ((lines, terms), vocab)
}

/// The info of `slot` if its key is `key`, else `miss`. A slot is a term's key
/// (12 bytes, zero-padded) under its info word (the top 4 bytes): the key bytes
/// are compared in one vptest (`mask` selects them), and the cmov keeps it
/// branch-free (LLVM would branch).
///
/// # Safety
///
/// `slot` points to a slot of the table: 16 readable bytes, 16-byte aligned.
#[inline(always)]
unsafe fn probe(slot: *const u128, key: __m128i, miss: u32) -> u32 {
    // SAFETY: `slot` is valid for an aligned 16-byte read (the caller's contract;
    // u128 is 16-byte aligned); the asm reads registers only. AVX2 is enabled for
    // the whole build.
    unsafe {
        let x = _mm_xor_si128(_mm_load_si128(slot.cast()), key);
        // The info word on its own: a load, where vpextrd would be two uops
        // (volatile, or LLVM extracts it from the vector load above).
        let mut info = slot.cast::<u32>().add(3).read_volatile();
        asm!("vptest {x}, {mask}", "cmovnz {i:e}, {miss:e}",
             x = in(xmm_reg) x, mask = in(xmm_reg) _mm_set_epi32(0, -1, -1, -1),
             i = inout(reg) info, miss = in(reg) miss, options(pure, nomem, nostack));
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
/// first round key (the seed: see SEEDS) gives an unrelated hash. Three
/// instructions, one cycle of throughput each. The round keys are digits of pi; a loop that
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

/// Attempt a's hash seed is a * SEEDS (an odd constant), which changes all 8 bytes
/// it is XORed into. (With the seed a, only bytes 0 and 1 changed, and the second
/// AES round carries them into hash bits 0-31 alone: bits 32-63, which decide the
/// slots a key can reach, stayed the same at every attempt.)
const SEEDS: u64 = 0x9e37_79b9_7f4a_7c15;

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
    /// info `count`. Empty slots have the zero key, which no token has (token bytes
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
/// from every split, sorted by bucket and then hash, one per hash. A bucket's
/// hashes share their low bits, so it lies in one group (hash & 63).
fn dedup_keys(splits: &[Split], q: usize, bmask: u64) -> Vec<Key> {
    let g = part(GROUPS, q, splits.len());
    let range = |s: &Split| s.starts[g.start] as usize..s.starts[g.end] as usize;
    let mine = || splits.iter().flat_map(|s| &s.keys[range(s)]);
    // The buckets of these groups numbered in order from 0: the bucket's bits
    // above the group's, then its group.
    let bucket =
        |e: &Key| ((e.hash & bmask) >> 6) as usize * g.len() + (e.hash & 63) as usize - g.start;
    let buckets = (bmask as usize + 1) / GROUPS * g.len();
    let (start, mut keys) = csr(|| mine().map(|e| (bucket(e) as u32, *e)), buckets);
    for b in 0..buckets {
        keys[start[b] as usize..start[b + 1] as usize].sort_unstable_by_key(|e| e.hash);
    }
    keys.dedup_by_key(|e| e.hash);
    keys
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
    /// The vocabulary of the query terms in `splits`, with attempt `seed`'s hash,
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
        let total: usize = splits.iter().map(|s| s.keys.len()).sum();
        // One bucket per 16 term occurrences (about 4.6 distinct terms each on
        // seed 1), and at least one per group.
        let bmask = (total / 16).max(GROUPS).next_power_of_two() as u64 - 1;
        let groups = par(n, &|q| dedup_keys(&splits, q, bmask));
        let distinct: usize = groups.iter().map(Vec::len).sum();
        // At least 1.5 slots per term; every third seed doubles the table.
        let bits = (distinct * 3 / 2).max(1024).next_power_of_two().ilog2() + seed as u32 / 3;
        let (slots, shift, region) = (1 << bits, 64 - bits, bits - GROUPS.ilog2());
        let mut long = std::collections::HashMap::new();
        for &term in splits.iter().flat_map(|s| &s.long) {
            let id = (distinct + long.len()) as u32;
            long.entry(term).or_insert(id);
        }
        // The table is allocated zeroed, so empty slots cost nothing until they
        // are read. Long term `id` has the slot `slots + id - distinct`.
        let count = distinct + long.len();
        let mut table = vec![0u128; slots + long.len() + 1];
        (distinct..count).for_each(|id| table[slots + id - distinct] = (id as u128) << 96);
        table[slots + long.len()] = (count as u128) << 96;
        let pilot: &[_] = Vec::leak((0..=bmask).map(|_| AtomicU16::new(0)).collect());
        let seed = seed.wrapping_mul(SEEDS);
        let phf = Phf { seed, pilot, shift };
        // The table's address as a usize, so that the closure is Sync; each thread
        // writes only its own regions (below). n = threads, the n of par's first call,
        // which made the pool: every part runs at once, as the barrier needs.
        let (at, barrier) = (table.as_mut_ptr().expose_provenance(), Barrier::new(n));
        let ids = par(n, &|q| {
            let table = std::ptr::with_exposed_provenance_mut::<u128>(at);
            // Largest buckets first, each with the first pilot whose slots are
            // all free (`find_pilot`). `taken` covers this thread's regions, from
            // `base`. Thread q numbers its keys from the number of keys before
            // its groups.
            let regions = part(GROUPS, q, n);
            let (base, len) = (regions.start << region, regions.len() << region);
            let first = groups[..q].iter().map(Vec::len).sum::<usize>() as u128;
            let same_bucket = |a: &Key, b: &Key| (a.hash ^ b.hash) & bmask == 0;
            let mut order: Vec<_> = groups[q].chunk_by(same_bucket).collect();
            order.sort_unstable_by_key(|b| Reverse(b.len()));
            let slot = |e: &Key, pilot| place(e.hash, pilot, shift) - base;
            // A key can reach few slots (find_pilot's candidates move it by one
            // fixed step each), which may all be taken: its bucket then goes first
            // in another attempt. (A failed attempt's keys left in the table are
            // harmless: a token only looks up the final slot of its own term.)
            let placed = (0..4).any(|_| {
                let (mut taken, mut next) = (vec![false; len], first);
                let mut set = |e: &Key, pilot, v| replace(&mut taken[slot(e, pilot)], v);
                let failed = order.iter().position(|bucket| {
                    let Some(pilot) = find_pilot(bucket, &mut set) else {
                        return true;
                    };
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
                    false
                });
                failed.inspect(|&i| order[..=i].rotate_right(1)).is_none()
            });
            // The ids need every thread's pilots and slots.
            barrier.wait();
            // The info of a key's slot. A slot that holds another key means that
            // two keys have the same hash (dedup_keys keeps one key per hash): the
            // build then needs another seed. SAFETY: Phf::slot is below
            // 2^(64 - shift) = slots, so a slot of the table, which no thread writes
            // any more.
            let mut clash = false;
            let mut info = |key| {
                let slot = unsafe { *table.add(phf.slot(key)) };
                // SAFETY: __m128i and u128 have the same size, and any bits are valid.
                clash |= slot & ((1 << 96) - 1) != unsafe { transmute::<__m128i, u128>(key) };
                (slot >> 96) as u32
            };
            // Only long terms are in `long`: a lookup there hashes all the bytes.
            // Every long term is in it, so the index does not fail.
            let id = |&(at, len): &Term| {
                let (at, op, len) = (at as usize, len & !TERM, (len & TERM) as usize);
                op | match len {
                    0..=KEY => info(checked_key(file, at, len)),
                    _ => long[&file[at..at + len]],
                }
            };
            let ids = placed.then(|| splits[q].terms.iter().map(id).collect());
            ids.filter(|_| !clash)
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
        // matches no token. The last slot keeps `count` (the table always has it).
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

/// A BOOL query evaluated in every superblock.
#[derive(Clone, Copy, Default)]
struct Entry {
    /// `row | op << 30` per term, then zeros: `end_super` reads only the first `len`.
    terms: [u32; MAX_BOOL_TERMS],
    /// The answer whose state the evaluation updates.
    answer: u32,
    /// The number of terms.
    len: u32,
}

/// A BOOL query as triggered, per document, by one of its cover terms c: the
/// query's other distinct terms, padded with a term no document has, and its
/// truth table over their presence (c being present). The table is zero where
/// a cover term before c is present: that term's entry counts the document.
/// Seven u32 words in this order, in 32 bytes (repr(C)): `trigger` gathers them.
#[derive(Clone, Copy, Default)]
#[repr(C, align(32))]
struct DocEntry {
    terms: [u32; MAX_BOOL_TERMS - 1],
    /// Bit a: whether the query matches when the terms of the set bits of a are
    /// present and the others absent.
    truth: u32,
    answer: u32,
}

/// The entry of the BOOL query `words` triggered by its cover term `c`, with
/// `earlier` the cover terms before c and `pad` a term no document has.
fn doc_entry(words: &[u32], c: u32, earlier: &[u32], answer: u32, pad: u32) -> DocEntry {
    let (mut terms, mut k) = ([pad; MAX_BOOL_TERMS - 1], 0);
    for t in words.iter().map(|&w| w & TERM) {
        if t != c && !terms[..k].contains(&t) {
            (terms[k], k) = (t, k + 1);
        }
    }
    // A term's presence in all 32 assignments at once, bit a for assignment a, so
    // the query is evaluated on all of them in one pass: c, the one word not in
    // `terms`, is in every document. (The padding terms are never present, so
    // the table's bits from 2^k on, a copy of the first 2^k, are never read.)
    let index = |t: u32| terms[..k].iter().position(|&o| o == t);
    let present = |t: u32| index(t).map_or(!0, |i| PRESENT[i]);
    let r = words.iter().fold(0, |r, &w| {
        let p = present(w & TERM);
        match w >> 30 {
            AND => r & p,
            NOT => r & !p,
            _ => r | p,
        }
    });
    DocEntry {
        terms,
        truth: earlier.iter().fold(r, |r, &e| r & !present(e)),
        answer,
    }
}

/// Bit a of PRESENT[i] is bit i of a: where term i of a DocEntry is present.
const PRESENT: [u32; MAX_BOOL_TERMS - 1] =
    [0xaaaaaaaa, 0xcccccccc, 0xf0f0f0f0, 0xff00ff00, 0xffff0000];

/// A phrase as its anchor term finds it: its term ids (the spec's phrases have 2
/// to 8 terms), its answer, the anchor's position in it, and its length. One
/// cache line: a check reads one line of the plan.
#[derive(Clone, Copy, Default)]
#[repr(align(64))]
struct Phrase {
    terms: [u32; 8],
    answer: u32,
    offset: u32,
    len: u32,
}

/// What the scan does with each query, decided from the sample.
#[derive(Default)]
struct Plan {
    /// BOOL entries triggered by term t: `entries[group[t]..group[t + 1]]`.
    group: Vec<u32>,
    entries: Vec<DocEntry>,
    /// BOOL entries evaluated in every superblock, on rows of `Scan::wide`.
    always: Vec<Entry>,
    /// The terms of `always` have the ids 0..rows, which are also their rows.
    rows: usize,
    /// Phrases anchored on term t: `anchored[anchor[t]..anchor[t + 1]]`.
    anchor: Vec<u32>,
    anchored: Vec<Phrase>,
    /// The info word of each term, by its old id: its new id, ANCHOR and TRIG.
    info: Vec<u32>,
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
            classify(words, phrase, cost, &kind)
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
                    assert!(words.len() <= 8, "queries.txt: a phrase of over 8 terms");
                    let mut phrase = Phrase {
                        answer,
                        offset: offset as u32,
                        len,
                        ..Phrase::default()
                    };
                    phrase.terms[..words.len()].copy_from_slice(words);
                    by_anchor.push((words[offset], phrase));
                    continue;
                }
                assert!(words.len() <= 6, "queries.txt: a BOOL of over 6 terms");
                if always_lines[line] {
                    // end_super reads only the first len terms: the rest stay 0.
                    let mut terms = [0; MAX_BOOL_TERMS];
                    terms[..words.len()].copy_from_slice(words);
                    always.push(Entry { terms, answer, len });
                    continue;
                }
                // Padded with the term after the sink, in no document.
                let (cover, n, _) = cover(words, cost);
                for (i, &c) in cover[..n].iter().enumerate() {
                    by_trigger.push((
                        c,
                        doc_entry(words, c, &cover[..i], answer, order.len() as u32 + 1),
                    ));
                }
            }
            (by_trigger, by_anchor, always)
        };
        // Part p's (by_trigger, by_anchor, always) is out[p].
        let mut out = par(parts, &lines_part);
        let by_trigger: Vec<_> = out.iter_mut().map(|o| take(&mut o.0)).collect();
        let by_anchor: Vec<_> = out.iter_mut().map(|o| take(&mut o.1)).collect();
        let always: Vec<_> = out.iter().flat_map(|o| o.2.iter().copied()).collect();
        let mut plan = Plan::default();
        // The anchors are grouped and the `always` entries sorted on a second
        // thread, while this one groups the entries.
        let (always, entries, anchors) = (Mutex::new(always), OnceLock::new(), OnceLock::new());
        par(2, &|i| match i {
            0 => _ = entries.set(csr(|| by_trigger.iter().flatten().copied(), bool_terms + 1)),
            _ => {
                // By length and operators, so that `end_super` predicts its branches.
                always.lock().unwrap().sort_unstable_by_key(|e| {
                    e.terms
                        .iter()
                        .fold(e.len, |key, &word| key << 2 | word >> 30)
                });
                _ = anchors.set(csr(|| by_anchor.iter().flatten().copied(), order.len()));
            }
        });
        (plan.group, plan.entries) = entries.into_inner().unwrap();
        (plan.anchor, plan.anchored) = anchors.into_inner().unwrap();
        let mut always = always.into_inner().unwrap();
        plan.number_answers(&mut always, lines);
        // 8 entries after the last that never match (truth 0): `trigger` writes up
        // to 7 indices past a range, and pads its last 8 with the last of them.
        plan.entries.extend([DocEntry::default(); 8]);
        let trig = |id: usize| id < bool_terms && plan.group[id] != plan.group[id + 1];
        let flag = |id: usize| {
            (plan.anchor[id] != plan.anchor[id + 1]) as u32 * ANCHOR | trig(id) as u32 * TRIG
        };
        // In parallel, a range of old ids per part.
        let word = |&id: &u32| id | flag(id as usize);
        let ids = |p| &rank[part(rank.len(), p, parts)];
        let info = par(parts, &|p| ids(p).iter().map(word).collect::<Vec<_>>());
        plan.info = info.concat();
        (plan.always, plan.rows) = (always, rows);
        plan
    }

    /// Numbers the answers in the order the scan visits them: the `always` ones
    /// first, so that each superblock reads and writes their state as one stream
    /// (47 KB instead of 4,000 scattered lines), then those of the entries in
    /// their order, then the phrases. Every line gets a number: it has an entry
    /// (a cover has at least one term), is an `always` one or is a phrase.
    fn number_answers(&mut self, always: &mut [Entry], lines: usize) {
        (self.answer_of, self.line_of) = (vec![u32::MAX; lines], Vec::with_capacity(lines));
        let mut number = |line: &mut u32| {
            if self.answer_of[*line as usize] == u32::MAX {
                self.answer_of[*line as usize] = self.line_of.len() as u32;
                self.line_of.push(*line);
            }
            *line = self.answer_of[*line as usize];
        };
        always.iter_mut().for_each(|e| number(&mut e.answer));
        self.entries.iter_mut().for_each(|e| number(&mut e.answer));
        self.anchored.iter_mut().for_each(|a| number(&mut a.answer));
    }
}

/// Marks the kinds of a line's terms and returns whether the line is a BOOL query
/// evaluated in every superblock (its cover terms are expected in more than ALWAYS
/// of the blocks, summed). A BOOL line's terms get kind bit 0, and bit 1 too when
/// it is such a query; a phrase's terms get nothing here.
fn classify(words: &[u32], phrase: bool, cost: impl Fn(u32) -> f64, kind: &[AtomicU8]) -> bool {
    let always = !phrase && cover(words, &cost).2 > ALWAYS;
    for &word in words.iter().filter(|_| !phrase) {
        kind[(word & TERM) as usize].fetch_or(1 | (always as u8) << 1, Relaxed);
    }
    always
}

/// The new order of the terms (old ids by new id), and each old id's new id.
/// BOOL terms first, those of the `always` lines first among them, each part by
/// sampled frequency, most frequent first, so the stamps, rows and entries the
/// scan uses most share cache lines; then the phrase-only terms in id order. A
/// counting sort by kind: 3, then 1, then 0.
fn term_order(kind: &[u8], sample_df: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let most = sample_df.iter().copied().max().unwrap_or(0) + 1;
    // Kind 2 never occurs: bit 1 is only set together with bit 0.
    let key = |t: usize| match kind[t] {
        0 => 2 * most,
        k => (3 - k as u32) / 2 * most + most - 1 - sample_df[t],
    };
    let keyed: Vec<_> = (0..sample_df.len()).map(|t| (key(t), t as u32)).collect();
    let (_, order) = csr(|| keyed.iter().copied(), 2 * most as usize + 1);
    let mut rank = vec![0; order.len()];
    (0..order.len()).for_each(|id| rank[order[id] as usize] = id as u32);
    (order, rank)
}

/// A set of terms of which every match contains at least one, built left to
/// right: after `AND t` every match contains t, so {t} replaces the set when it
/// is cheaper; after `OR t` a match contains t or a term of the set; `NOT`
/// only removes matches. Returns the set in an array, its size and its cost (the
/// sum of its terms' costs, kept as the set changes).
fn cover(terms: &[u32], cost: impl Fn(u32) -> f64) -> ([u32; MAX_BOOL_TERMS], usize, f64) {
    let (mut cover, mut n, mut sum) = ([0; MAX_BOOL_TERMS], 0, 0.0);
    for &word in terms {
        let t = word & TERM;
        match word >> 30 {
            OR if !cover[..n].contains(&t) => (cover[n], n, sum) = (t, n + 1, sum + cost(t)),
            AND if cost(t) < sum => (cover[0], n, sum) = (t, 1, cost(t)),
            _ => {}
        }
    }
    (cover, n, sum)
}

/// Groups values by key: values of key k are `values[start[k]..start[k + 1]]`,
/// in their original order. `pairs` makes the (key, value) pairs, the same ones
/// at each call: they are counted, then placed (so nothing copies them first).
fn csr<T, I: DoubleEndedIterator<Item = (u32, T)>>(
    pairs: impl Fn() -> I,
    keys: usize,
) -> (Vec<u32>, Vec<T>) {
    // The end of each key's run, then each value put before it, last value first:
    // the ends step down to the starts.
    let mut start = vec![0u32; keys + 1];
    // for_each, not a for loop: a flat_map then loops over each slice directly.
    pairs().for_each(|(k, _)| start[k as usize] += 1);
    (1..=keys).for_each(|k| start[k] += start[k - 1]);
    let mut values = Vec::with_capacity(start[keys] as usize);
    pairs().rev().for_each(|(k, value)| {
        start[k as usize] -= 1;
        values.spare_capacity_mut()[start[k as usize] as usize].write(value);
    });
    // SAFETY: the counts add up to start[keys], so each of its slots was written once.
    unsafe { values.set_len(start[keys] as usize) };
    (start, values)
}

/// One thread's pass over a slice of the corpus. Without a plan it only counts
/// document frequencies into `df`, for the sample.
struct Scan<'a> {
    vocab: &'a Vocab,
    plan: Option<&'a Plan>,
    df: &'a [AtomicU32],
    /// Per term, the stamp of the last document that contains it (see STAMPS).
    /// Slot `count` takes the tokens no query uses; slot count + 1 is in no
    /// document.
    lastdoc: Vec<u8>,
    /// The current document's stamp.
    stamp: u8,
    /// This document's trigger term occurrences (as info words); then the
    /// entries of its trigger terms, each term once, as indices, and those of
    /// them that match (see `trigger`; both as long as the entries).
    trig: Vec<u32>,
    evals: Vec<u32>,
    hits: Vec<u32>,
    /// Per block of this superblock, its words: word r < plan.rows of block b,
    /// `wide[b * (rows + 1) + r]`, is BOOL term r's in that block. Word `rows`
    /// takes the other terms (its bits are never read). Kept block by block,
    /// so that a block's words, which every token updates, are contiguous.
    wide: Vec<u64>,
    /// This document's tokens to look up: their keys and the byte offsets of
    /// their slots.
    pending: Vec<(__m128i, u64)>,
    /// This document's tokens' info words, and the positions of anchors among them.
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
        let rows = plan.map_or(0, |p| p.rows);
        let answers = plan.map_or(0, |p| p.line_of.len());
        let wide = written_zeros(4 * (rows + 1));
        Scan {
            vocab,
            plan,
            df,
            // 3 bytes more: `trigger` gathers 4 bytes from a stamp.
            lastdoc: vec![EMPTY; plan.map_or(0, |_| vocab.count as usize + 5)],
            stamp: 0,
            trig: Vec::new(),
            evals: vec![0; plan.map_or(0, |p| p.entries.len())],
            hits: vec![0; plan.map_or(0, |p| p.entries.len())],
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
                    let b = text.trailing_zeros() as usize;
                    let e = ends.trailing_zeros() as usize;
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
                // At the first id of a slice, this may evaluate an empty
                // superblock: harmless.
                if id % SUPER == 0 && id > 0 {
                    self.end_super(id - SUPER);
                }
                self.doc = id;
                bit = 1 << (id % 64);
                // A new window of stamps (the slice's first one starts empty).
                if id % STAMPS == 0 {
                    self.lastdoc.fill(EMPTY);
                }
                self.stamp = (id % STAMPS) as u8;
            }
            at += cut as usize;
            (mask, lf) = (next, next_lf);
        }
        self.look_up(bit);
        self.end_doc();
        self.end_super(self.doc / SUPER * SUPER);
        self
    }

    /// A token from `p` that is too long for a window (63 bytes or more), so too
    /// long to be a term: it goes to `pending` as a long token. An id token that
    /// long is rejected by `parse_id`. Returns the end of the token.
    #[cold]
    fn long_run(&mut self, docs: &[u8], p: usize, end: usize, id: bool) -> usize {
        let alnum = |b: &&u8| b.is_ascii_alphanumeric();
        let len = docs[p..end].iter().take_while(alnum).count();
        if id {
            parse_id(&docs[p..p + len]);
        }
        self.pending
            .push(self.vocab.get_long(&docs[p..p + len.min(64)]));
        p + len
    }

    /// Looks up the tokens of this document, whose bit is `bit`, then records
    /// their presence. Each step runs over the whole document, so that the memory
    /// accesses of one step overlap: the slots were fetched when the tokens were
    /// found. (In the earlier block design, fetching the presence words too cost
    /// more than it saved, and one loop for both steps was slower.)
    fn look_up(&mut self, bit: u64) {
        let (miss, table) = (self.vocab.count, self.vocab.table.as_ptr());
        let (doc, rows) = (self.doc, self.wide.len() / 4 - 1);
        // This document's block in the superblock: its word in each row.
        let block = (doc / 64 % (SUPER / 64)) as usize;
        let wide = self.wide.as_mut_ptr().wrapping_add(block * (rows + 1));
        // 8 more: end_doc's phrase compares read 8 ids from any token.
        self.toks.reserve(self.pending.len() + 8);
        self.trig.reserve(self.pending.len());
        let (toks, mut len) = (self.toks.as_mut_ptr(), self.toks.len());
        let (trig, mut nt) = (self.trig.as_mut_ptr(), self.trig.len());
        let anchor = self.plan.map_or(std::ptr::null(), |p| p.anchor.as_ptr());
        for &(key, off) in &self.pending {
            // SAFETY: off is the byte offset of a slot of the table: Phf::slot gives
            // one below 2^(64 - shift), and get_long a long term's or the last one.
            let info = unsafe { probe(table.byte_add(off as usize), key, miss) };
            if info & ANCHOR != 0 {
                self.anchors.push(len as u32);
                // Its phrases' range, read at the document's end.
                prefetch(anchor, (info & ID) as usize);
            }
            // SAFETY: room for every pending token was reserved above.
            unsafe {
                toks.add(len).write(info);
                // Branch-free: every trigger occurrence is kept (end_doc takes
                // each term once).
                trig.add(nt).write(info);
            }
            nt += (info & TRIG != 0) as usize;
            len += 1;
        }
        // SAFETY: the first len and nt entries are written.
        unsafe { (self.toks.set_len(len), self.trig.set_len(nt)) };
        // SAFETY: room for 8 more ids was reserved: `miss`, which no phrase term has.
        unsafe { toks.add(len).cast::<[u32; 8]>().write([miss; 8]) };
        self.pending.clear();
        // The sample only counts documents per term (end_doc).
        let Some(_) = self.plan else { return };
        let (lastdoc, stamp) = (self.lastdoc.as_mut_ptr(), self.stamp);
        for &info in &self.toks {
            let id = (info & ID) as usize;
            // SAFETY: id <= count < lastdoc.len(), and id.min(rows) < rows + 1, a
            // word of this block's.
            unsafe {
                *lastdoc.add(id) = stamp;
                *wide.add(id.min(rows)) |= bit;
            }
        }
    }

    /// Ends the current document: with the plan, counts the phrases anchored on
    /// its tokens that match, and the BOOL queries its trigger terms trigger that
    /// match; for the sample, its distinct terms.
    fn end_doc(&mut self) {
        let doc = self.doc;
        if let Some(plan) = self.plan {
            // The phrases' first records are random reads: fetched now, and read
            // after the BOOL evaluation (they update other answers).
            for &at in &self.anchors {
                let anchor = (self.toks[at as usize] & ID) as usize;
                prefetch(plan.anchored.as_ptr(), plan.anchor[anchor] as usize);
            }
            self.trigger(plan);
            for &at in &self.anchors {
                let anchor = (self.toks[at as usize] & ID) as usize;
                for i in plan.anchor[anchor] as usize..plan.anchor[anchor + 1] as usize {
                    let p = &plan.anchored[i];
                    // The phrase's start: none before the document's first token.
                    let Some(f) = (at as usize).checked_sub(p.offset as usize) else {
                        continue;
                    };
                    // SAFETY: 8 words from f in toks: its tokens and the 8 words look_up
                    // writes after them (f <= at < toks.len()), and the 32 aligned bytes of
                    // p.terms (Phrase is 64-byte aligned); AVX2 and BMI2 are enabled for
                    // the build.
                    let hit = unsafe {
                        let a = _mm256_loadu_si256(self.toks.as_ptr().add(f).cast());
                        let a = _mm256_and_si256(a, _mm256_set1_epi32(ID as i32));
                        let b = _mm256_load_si256(p.terms.as_ptr().cast());
                        let need = _bzhi_u32(!0, 4 * p.len);
                        movemask(_mm256_cmpeq_epi32(a, b)) & need == need
                    };
                    if hit && self.last[i] != doc {
                        self.last[i] = doc;
                        // SAFETY: a phrase's answer is an answer number, below state.len().
                        unsafe { self.answers.add(p.answer, 1 << (doc % 64), doc & !63) };
                    }
                }
            }
        } else {
            // The sample: count the document's distinct terms (the table has no
            // flags yet, so the info words are the ids).
            self.toks.sort_unstable();
            self.toks.dedup();
            for &t in self.toks.iter().filter(|&&t| t != self.vocab.count) {
                self.df[t as usize].fetch_add(1, Relaxed);
            }
        }
        self.toks.clear();
        self.anchors.clear();
        self.trig.clear();
    }

    /// Evaluates the BOOL entries of this document's trigger terms, each term
    /// once (its stamp is then marked DONE, which the evaluation masks off), 8 at
    /// a time: the fresh terms' entry ranges, about 1.5 entries each, become one
    /// list of entry indices (each range's first entry fetched), and each 8 of them
    /// are evaluated in AVX2 registers, their terms' stamps gathered. Only the
    /// matches, about half, update their answers. No branch on the data: whether a
    /// query matches is too random to predict (so was a loop per range).
    fn trigger(&mut self, plan: &Plan) {
        let (doc, stamp, entries) = (self.doc, self.stamp, plan.entries.as_ptr());
        let (evals, hits) = (self.evals.as_mut_ptr(), self.hits.as_mut_ptr());
        let (group, lastdoc) = (plan.group.as_ptr(), self.lastdoc.as_mut_ptr());
        let mut n = 0;
        // SAFETY: trigger terms are below bool_terms, and group has bool_terms + 2
        // entries. Fresh terms are distinct, so their ranges are disjoint, and n
        // stays at most the number of real entries, entries.len() - 8: a store of
        // 8 indices from n + j (j < the range's length; 0 for a repeat) ends inside
        // evals, which is as long as the entries. AVX2 is enabled for the build.
        unsafe {
            let lane = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);
            for t in self.trig.iter().map(|&info| (info & ID) as usize) {
                // Branch-free: a repeated trigger term is written again and not kept.
                let fresh = *lastdoc.add(t) == stamp;
                *lastdoc.add(t) = stamp | DONE;
                let (from, to) = (*group.add(t), *group.add(t + 1));
                prefetch(entries, from as usize);
                // Its entries' indices, 8 per store: the next range overwrites the
                // spare ones, and a range of over 8 (rare) goes on.
                let store = |f: u32| {
                    let at = evals.add(n + (f - from) as usize).cast();
                    _mm256_storeu_si256(at, _mm256_add_epi32(_mm256_set1_epi32(f as i32), lane));
                };
                store(from);
                if fresh && to - from > 8 {
                    (from + 8..to).step_by(8).for_each(store);
                }
                n += fresh as usize * (to - from) as usize;
            }
            // The last 8's spare lanes: the last padding entry, which never matches.
            let padding = _mm256_set1_epi32(plan.entries.len() as i32 - 1);
            _mm256_storeu_si256(evals.add(n).cast(), padding);
        }
        // Word 8i + k of the entries is terms[k] of entry i, and 8i + 5 its truth.
        let (words, stamps) = (entries.cast::<i32>(), lastdoc.cast::<i32>().cast_const());
        let mut nh = 0;
        for b in (0..n).step_by(8) {
            // SAFETY: AVX2 and BMI2 are enabled for the whole build. evals[b..b + 8]
            // (b < n) were written above, with indices below entries.len() (a range's
            // spare ones are at most 7 past it, among the padding), so the words 8i
            // to 8i + 5 gathered are inside entries. Entry terms are below count + 2,
            // and lastdoc has 3 bytes more, so the 4 bytes gathered from each stamp
            // are inside it. nh <= b < n, so the 8 lanes stored at hits[nh..] are
            // inside hits, as long as evals.
            unsafe {
                let idx = _mm256_loadu_si256(evals.add(b).cast());
                let word = |k| _mm256_add_epi32(_mm256_slli_epi32::<3>(idx), _mm256_set1_epi32(k));
                // Bit k: term k is in this document, from the stamp byte without
                // DONE (EMPTY's 0x7F is no stamp): has is -1 if so, and the bits
                // are shifted in from the last term (present * 2 - has).
                let mut present = _mm256_setzero_si256();
                for k in (0..MAX_BOOL_TERMS as i32 - 1).rev() {
                    let terms = _mm256_i32gather_epi32::<4>(words, word(k));
                    let s = _mm256_i32gather_epi32::<1>(stamps, terms);
                    let s = _mm256_and_si256(s, _mm256_set1_epi32(!DONE as i32));
                    let has = _mm256_cmpeq_epi32(s, _mm256_set1_epi32(stamp as i32));
                    present = _mm256_sub_epi32(_mm256_add_epi32(present, present), has);
                }
                // Bit 31 of a lane: bit `present` of its truth table, whether it matches.
                let truth = _mm256_i32gather_epi32::<4>(words, word(5));
                let hit = _mm256_slli_epi32::<31>(_mm256_srlv_epi32(truth, present));
                let mask = _mm256_movemask_ps(_mm256_castsi256_ps(hit)) as u64;
                // The matches' indices to the front: pext picks their lane numbers.
                let bytes = _pdep_u64(mask, 0x0101_0101_0101_0101) * 0xFF;
                let lanes = _mm_cvtsi64_si128(_pext_u64(0x0706_0504_0302_0100, bytes) as i64);
                let packed = _mm256_permutevar8x32_epi32(idx, _mm256_cvtepu8_epi32(lanes));
                _mm256_storeu_si256(hits.add(nh).cast(), packed);
                nh += mask.count_ones() as usize;
            }
        }
        for &e in &self.hits[..nh] {
            // SAFETY: an entry's answer is an answer number, below state.len().
            unsafe { self.answers.add_doc(plan.entries[e as usize].answer, doc) };
        }
    }

    /// Evaluates the `always` queries on the superblock of 256 documents from
    /// `base`, on four words at once (one bit per document), then clears it.
    fn end_super(&mut self, base: u32) {
        let Some(plan) = self.plan else { return };
        let (wide, stride) = (self.wide.as_ptr(), plan.rows + 1);
        for entry in &plan.always {
            let terms = &entry.terms[..entry.len as usize];
            // SAFETY: the terms of `always` entries are rows below plan.rows, so
            // their 4 words are in wide. AVX2 is enabled for the whole build.
            let hits = unsafe {
                terms.iter().fold(_mm256_setzero_si256(), |r, &word| {
                    let w = wide.add((word & TERM) as usize);
                    let words = [0, 1, 2, 3].map(|b| *w.add(b * stride) as i64);
                    let x = _mm256_set_epi64x(words[3], words[2], words[1], words[0]);
                    match word >> 30 {
                        AND => _mm256_and_si256(r, x),
                        NOT => _mm256_andnot_si256(x, r),
                        _ => _mm256_or_si256(r, x),
                    }
                })
            };
            self.answers.add_wide(entry.answer, hits, base);
        }
        (0..4).for_each(|b| self.wide[b * stride..b * stride + plan.rows].fill(0));
    }
}

/// `len` zeros, written now: a lazily zeroed page that is read first faults
/// again when written, and that fault flushes the TLB of every thread.
fn written_zeros(len: usize) -> Vec<u64> {
    std::iter::repeat_n(std::hint::black_box(0), len).collect()
}

/// One thread's answers.
struct Answers {
    /// Per answer: count << SUM_BITS | sum of the matching ids.
    state: Vec<u64>,
    /// `answer << 16 | id & 0xFFFF` for the first FIRST_IDS matches of each answer
    /// (see MIN_SLICES), in `log[..logged]`. Its zero pages cost no memory until
    /// they are written.
    log: Vec<u32>,
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
        let entry = |bits: u64| answer << 16 | (base + bits.trailing_zeros()) & 0xFFFF;
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

    /// Adds document `doc`, branch-free.
    ///
    /// # Safety
    ///
    /// As for `add`.
    unsafe fn add_doc(&mut self, answer: u32, doc: u32) {
        // SAFETY: as in `add`.
        unsafe {
            let s = &mut *self.state.as_mut_ptr().add(answer as usize);
            *self.log.as_mut_ptr().add(self.logged) = answer << 16 | doc & 0xFFFF;
            self.logged += (*s >> SUM_BITS < FIRST_IDS) as usize;
            *s += 1 << SUM_BITS | doc as u64;
        }
    }

    /// `add` for the 256 documents from `base`, one word of `hits` per 64.
    fn add_wide(&mut self, answer: u32, hits: __m256i, base: u32) {
        // SAFETY: AVX2 is enabled for the whole build.
        let has_hits = unsafe { _mm256_testz_si256(hits, hits) } == 0;
        if self.state[answer as usize] >> SUM_BITS >= FIRST_IDS {
            let (n, positions) = count_positions(hits);
            self.state[answer as usize] += n << SUM_BITS | (n * base as u64 + positions);
        } else if has_hits {
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

/// `vpmovmskb` hidden from LLVM: on CPUs without AVX-512 it rewrites the
/// combination of two movemasks into a bit-by-bit extraction that costs a quarter
/// of the runtime.
fn movemask(v: __m256i) -> u32 {
    let m: u32;
    // SAFETY: AVX2 is enabled for this build; the instruction reads one register.
    unsafe { asm!("vpmovmskb {:e}, {}", out(reg) m, in(ymm_reg) v, options(pure, nomem, nostack)) };
    m
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
    let id = |line: &[u8]| {
        let tab = line.iter().position(|&b| b == b'\t');
        parse_id(&line[..tab.expect("docs.txt: a line without a TAB")])
    };
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
