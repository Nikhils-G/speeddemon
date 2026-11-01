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
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU8, Ordering::Relaxed};

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
/// A BOOL term word is `slot | op << 30`. The op codes make `end_block` branch-free.
const TERM: u32 = (1 << 30) - 1;
const AND: u32 = 0;
const NOT: u32 = 1;
const OR: u32 = 3;
/// Corpus bytes sampled (spread over the slices) to estimate term frequencies:
/// about 1,030 documents on seed 1. (2 MB planned no better and took 1.5 ms more;
/// 512 KB planned worse.)
const SAMPLE_BYTES: usize = 1 << 20;
/// A query whose cover terms are expected in more blocks than this (as a share,
/// summed over the terms) is evaluated in every superblock: one evaluation then
/// covers four blocks. (0.125 and 0.5 were measured slower.)
const ALWAYS: f64 = 0.25;
/// Documents per superblock: four blocks of 64.
const SUPER: u32 = 256;
/// A thread's sum of matching ids is packed under its count in one u64.
const SUM_BITS: u32 = 40;

fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    mmap::no_huge_pages();
    let open = |name: &str| mmap::map(&format!("{data_dir}/{name}"));
    let docs = open("docs.txt");
    let text = &docs[..docs.len() - mmap::PAD];
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let (queries, mut vocab) = parse_queries(open("queries.txt"), threads);
    let slices = slices(text, threads);

    // Sample the start of every slice for each term's document frequency.
    let df: Vec<AtomicU32> = (0..vocab.count).map(|_| AtomicU32::new(0)).collect();
    let sampled = par(slices.len(), &|i| {
        let (start, end, first) = slices[i];
        let stop = line_after(text, start + SAMPLE_BYTES / slices.len()).min(end);
        let scan = Scan::new(&vocab, None, &df).run(docs, start, stop, first);
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
            let scan = Scan::new(&vocab, Some(&plan), &[]).run(docs, start, end, first);
            (scan.answers, scan.doc)
        };
        // Drop the slice's pages now, in parallel, instead of the kernel
        // unmapping all 62,000 at exit on one core.
        mmap::release(&docs[start..end]);
        // The ids go on in the next slice. Every per-thread sum of ids is below
        // N^2 / 2, which must fit in SUM_BITS.
        let next = slices.get(i + 1).map_or(doc + 1, |s| s.2);
        let ok = slices[0].2 == 0 && next == doc + 1 && doc < 1 << 20;
        assert!(ok, "docs.txt: ids must be 0, 1, 2, ... below 2^20");
        // The first ids by line number, without a sort: an answer logged the
        // first min(count, 10) of its ids, in order, so each line's place in
        // the result is known from the counts.
        let place = |n: &mut u32, &a: &u32| Some(replace(n, *n + (state[a as usize] >> SUM_BITS).min(10) as u32));
        let mut at: Vec<u32> = plan.answer_of.iter().scan(0, place).collect();
        let mut firsts = vec![0; logged];
        for &e in &log[..logged] {
            let line = plan.line_of[(e >> 32) as usize] as usize;
            firsts[at[line] as usize] = (line as u64) << 32 | e & u32::MAX as u64;
            at[line] += 1;
        }
        (state, firsts)
    });

    // Format in parallel, one range of lines per thread. Per line: the threads'
    // counts and sums added up (from the state of its answer: no copy of the
    // states, which would cost 4.5 MB), then the first ten ids taken from the
    // threads in slice order. The packed states add up as they are: all ids are
    // below 2^20, so any sum of them is below 2^39 and never carries into the count.
    let parts = slices.len().max(1);
    let chunks = par(parts, &|p| {
        let lines = part(plan.answer_of.len(), p, parts);
        let (mut out, mut row) = (Vec::with_capacity(lines.len() * 32), Vec::new());
        let first = |log: &Vec<u64>| log.partition_point(|&e| e >> 32 < lines.start as u64);
        let mut cursors: Vec<usize> = results.iter().map(|r| first(&r.1)).collect();
        for line in lines {
            // The threads' states of the line 8 ahead: they are random reads.
            let ahead = plan.answer_of.get(line + 8).map_or(0, |&a| a as usize);
            results.iter().for_each(|r| prefetch(r.0.as_ptr(), ahead));
            let answer = plan.answer_of[line] as usize;
            let s: u64 = results.iter().map(|r| r.0[answer]).sum();
            // The output row: count, sum, then the threads' first ids (12 fields at most).
            row.clear();
            row.extend([s >> SUM_BITS, s & ((1 << SUM_BITS) - 1)]);
            for ((_, log), cursor) in results.iter().zip(&mut cursors) {
                while let Some(&entry) = log.get(*cursor).filter(|&&e| e >> 32 == line as u64) {
                    row.push(entry & u32::MAX as u64);
                    *cursor += 1;
                }
            }
            row.iter().take(12).for_each(|&v| push_decimal(&mut out, v));
            *out.last_mut().unwrap() = b'\n';
        }
        out
    });
    let mut file = std::fs::File::create(format!("{out_dir}/results.txt")).expect("results.txt");
    for chunk in chunks {
        std::io::Write::write_all(&mut file, &chunk).expect("write results.txt");
    }
    // Done: exit without freeing the tables and logs, one free at a time.
    std::process::exit(0)
}

/// f(0), f(1), ... f(n - 1), each on its own thread.
fn par<T: Send>(n: usize, f: &(impl Fn(usize) -> T + Sync)) -> Vec<T> {
    std::thread::scope(|s| {
        let threads: Vec<_> = (0..n).map(|i| s.spawn(move || f(i))).collect();
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
/// vectors are sized for the most terms a chunk can hold, so they never grow:
/// their untouched pages cost nothing.
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
                let key = key_at(padded, at as usize, len as usize);
                keys.push((hash(key, seed), at, len));
            } else {
                // `AND`, `NOT` and `OR` start with bytes ending in binary 01, 10 and 11.
                op = [0, AND, NOT, OR][(word[0] & 3) as usize];
            }
        }
        lines.push((start as u32, (terms.len() - start) as u32, phrase));
    }
    // Grouped by hash & 63.
    let by_group = keys.iter().map(|&k| ((k.0 & 63) as u32, k)).collect();
    let (starts, grouped) = csr(&[by_group], 64);
    (lines, terms, starts, grouped, long)
}

type Split = (Vec<Span>, Vec<Term>, Vec<u32>, Vec<Key>, Vec<Long>);
type Long = &'static [u8];
/// A term's offset, and its length | op << 30.
type Term = (u32, u32);
/// A key's hash, then the offset and length of its bytes (0 for a long term).
type Key = (u64, u32, u32);

/// Splits the lines in parallel, then gives the terms ids: their slots in the
/// perfect hash. `padded` is the file and mmap::PAD zero bytes.
fn parse_queries(padded: &'static [u8], threads: usize) -> (Queries, Vocab) {
    let text = &padded[..padded.len() - mmap::PAD];
    // The start of the p-th of `threads` ranges of whole lines.
    let cut = |p: usize| line_after(text, text.len() * p / threads);
    let chunk = |p| &text[cut(p)..cut(p + 1)];
    let split = |seed| par(threads, &|p| split_lines(chunk(p), padded, seed));
    let (vocab, ids, splits) = (0..).find_map(|s| Vocab::new(split(s), s, padded)).unwrap();
    let (mut lines, mut terms) = (Vec::with_capacity(50_000), Vec::with_capacity(160_000));
    for (split, ids) in splits.iter().zip(ids) {
        let base = terms.len() as u32;
        lines.extend(split.0.iter().map(|&(s, n, p)| (base + s, n, p)));
        terms.extend(ids);
    }
    ((lines, terms), vocab)
}

/// The info of `slot` if its key is `key`, else MISS. A slot is a term's key
/// (12 bytes, zero-padded) under its info word (the top 4 bytes): the key bytes
/// are compared in one vptest, and the cmov keeps it branch-free (LLVM would
/// branch).
#[inline(always)]
fn probe(slot: *const u128, key: __m128i) -> u32 {
    // SAFETY: `slot` points into the table (u128 is 16-byte aligned); the asm
    // reads registers only. AVX2 is enabled for the whole build.
    unsafe {
        let x = _mm_xor_si128(_mm_load_si128(slot.cast()), key);
        // The info word on its own: a load, where vpextrd would be two uops.
        let mut info = *slot.cast::<u32>().add(3);
        asm!("vptest {x}, {m}", "cmovnz {i:e}, {miss:e}", x = in(xmm_reg) x, m = in(xmm_reg) _mm_set_epi32(0, -1, -1, -1),
             i = inout(reg) info, miss = in(reg) MISS, options(pure, nomem, nostack));
        info
    }
}

/// The table key of the token `bytes[at..at + len]`, len <= 12: its bytes,
/// ASCII-lowercased and zero-padded to 16. OR-ing 0x20 lowercases A-Z and keeps
/// digits; KEEP keeps only the token's bytes.
#[inline(always)]
fn key_at(bytes: &[u8], at: usize, len: usize) -> __m128i {
    debug_assert!(at + 16 <= bytes.len() && len <= 16);
    // SAFETY: 16-byte loads inside `bytes` (a token starts before the end of
    // its file, which mmap::PAD zero bytes follow) and KEEP (len <= 16); AVX2
    // is enabled for the whole build.
    unsafe {
        let raw = _mm_loadu_si128(bytes.as_ptr().add(at).cast());
        let keep = _mm_loadu_si128(KEEP.as_flattened().as_ptr().add(16 - len).cast());
        _mm_and_si128(_mm_or_si128(raw, _mm_set1_epi8(0x20)), keep)
    }
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
/// with hash h is in bucket h & bmask, and the bucket's pilot m puts it in slot
/// `(h * m).rotate_right(6) >> shift`. Every term has its own slot, so a token
/// is compared with one slot only. Two terms of a bucket share the low bits of h
/// but differ above, so different multipliers move them apart. A pilot is 1 mod
/// 64, so the low 6 bits of h * m are those of h (its group): rotated to the
/// top, they make each group's slots a region of its own, and the groups are
/// placed in parallel.
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

/// The hash function of the table: the seed, the pilots (per bucket, a
/// power of two of them) and the shift. Copy, so that the scan keeps it in
/// registers. The pilots are leaked (they live until the process exits), and
/// atomic so that the threads of the build can write them.
#[derive(Clone, Copy)]
struct Phf(u64, &'static [AtomicU16], u32);

impl Phf {
    /// The slot of a short key (a term's if the key is one).
    #[inline(always)]
    fn slot(self, key: __m128i) -> usize {
        let h = hash(key, self.0);
        // SAFETY: BMI2 is enabled for the whole build. The bucket, the low bits
        // of h, is below len (a power of two). The pilot is read as a plain u16
        // (an atomic load would cost a second zero extension): AtomicU16 has the
        // layout of u16, and pilots are only read once the build is over, which
        // the build's barrier and the end of its threads order before the reads.
        let m = unsafe {
            let bucket = _bzhi_u64(h, self.1.len().trailing_zeros()) as usize;
            self.1.get_unchecked(bucket).as_ptr().read()
        };
        place(h, m as u64, self.2)
    }
}

/// The slot of the hash h with the pilot m.
#[inline(always)]
fn place(h: u64, m: u64, shift: u32) -> usize {
    (h.wrapping_mul(m).rotate_right(6) >> shift) as usize
}

impl Vocab {
    /// The vocabulary of the query terms in `splits`, with the hash of `seed`,
    /// the term words of each split with their ids, and `splits`; or None when the build
    /// needs another seed: two different keys with one hash, or a bucket that
    /// finds no pilot. Every third seed also doubles the table, so some seed
    /// succeeds.
    ///
    /// A bucket's hashes share their low bits, so it lies in one group (hash &
    /// 63). Each thread takes a range of groups: it sorts their keys by bucket
    /// and hash and keeps one of each hash, checking that equal hashes have
    /// equal keys. Then, in its own region of the table, it places their buckets
    /// and writes their keys and ids, and once every thread has, it gives its
    /// split's terms their ids. About 4.6 distinct terms per bucket and at least
    /// 1.5 slots per term (a load of 0.58 on seed 1). `file` is the query file
    /// and mmap::PAD zero bytes.
    fn new(splits: Vec<Split>, seed: u64, file: Long) -> Option<(Self, Vec<Vec<u32>>, Vec<Split>)> {
        let n = splits.len();
        let total: usize = splits.iter().map(|s| s.3.len()).sum();
        // The key bytes of a hash. SAFETY: __m128i and u128 have the same size,
        // and any bits are valid.
        let key = |e: &Key| -> u128 { unsafe { transmute(key_at(file, e.1 as _, e.2 as _)) } };
        let bmask = (total / 16).max(64).next_power_of_two() as u64 - 1;
        let groups = par(n, &|q| {
            let g = part(64, q, n);
            let mut keys = Vec::with_capacity(2 * total / n + 64);
            for (_, _, starts, grouped, _) in &splits {
                keys.extend_from_slice(&grouped[starts[g.start] as usize..starts[g.end] as usize]);
            }
            // By bucket, then hash: the rotation puts the bucket bits on top.
            keys.sort_unstable_by_key(|e| e.0.rotate_right(bmask.count_ones()));
            let same = |w: &[Key]| w[0].0 != w[1].0 || key(&w[0]) == key(&w[1]);
            let ok = keys.windows(2).all(same);
            keys.dedup_by_key(|e| e.0);
            ok.then_some(keys)
        });
        let groups: Vec<_> = groups.into_iter().collect::<Option<_>>()?;
        let distinct: usize = groups.iter().map(Vec::len).sum();
        let bits = (distinct * 3 / 2).max(1024).next_power_of_two().ilog2() + seed as u32 / 3;
        let (slots, shift, region) = (1 << bits, 64 - bits, bits - 6);
        let mut long = std::collections::HashMap::new();
        for &term in splits.iter().flat_map(|s| &s.4) {
            let id = (distinct + long.len()) as u32;
            long.entry(term).or_insert(id);
        }
        // The table is allocated zeroed, so empty slots cost nothing until they
        // are read. Long term `id` has the slot `slots + id - distinct`.
        let count = distinct + long.len();
        let mut table = vec![0u128; slots + long.len() + 1];
        (distinct..count).for_each(|id| table[slots + id - distinct] = (id as u128) << 96);
        table[slots + long.len()] = (MISS as u128) << 96;
        let pilots: &[_] = Vec::leak((0..=bmask).map(|_| AtomicU16::new(0)).collect());
        let phf = Phf(seed, pilots, shift);
        let (at, barrier) = (table.as_mut_ptr() as usize, std::sync::Barrier::new(n));
        let ids = par(n, &|q| {
            // Largest buckets first: for each the first pilot whose slots are
            // all free, claimed one at a time and given back at the first taken
            // one. `taken` covers this thread's regions, from `base`. Thread q
            // numbers its keys from the number of keys before its groups.
            let base = part(64, q, n).start << region;
            let mut taken = vec![false; part(64, q, n).len() << region];
            let mut next = groups[..q].iter().map(Vec::len).sum::<usize>() as u128;
            let bucket = |a: &Key, b: &Key| (a.0 ^ b.0) & bmask == 0;
            let mut order: Vec<_> = groups[q].chunk_by(bucket).collect();
            order.sort_unstable_by_key(|b| Reverse(b.len()));
            let slot = |e: &Key, m| place(e.0, m, shift) - base;
            let mut set = |e, m, v| replace(&mut taken[slot(e, m)], v);
            let placed = order.iter().all(|it| {
                let fits = (1..1 << 16).step_by(64).find(|&m| {
                    let free = it.iter().take_while(|e| !set(e, m, true)).count();
                    // Gives the claimed slots back unless all were free.
                    let all = free == it.len();
                    it[..free].iter().for_each(|e| _ = set(e, m, all));
                    all
                });
                let Some(m) = fits else { return false };
                pilots[(it[0].0 & bmask) as usize].store(m as u16, Relaxed);
                for e in *it {
                    // SAFETY: the slot is in this thread's regions of the table,
                    // which no other thread writes.
                    unsafe { *(at as *mut u128).add(base + slot(e, m)) = key(e) | next << 96 };
                    next += 1;
                }
                true
            });
            // The ids need every thread's pilots and slots.
            barrier.wait();
            // SAFETY: a slot of the table, which no thread writes any more.
            let info = |key| (unsafe { *(at as *const u128).add(phf.slot(key)) } >> 96) as u32;
            // Only long terms are in `long`: a lookup there hashes all the bytes.
            let id = |&(at, len): &Term| {
                let (at, op, len) = (at as usize, len & !TERM, (len & TERM) as usize);
                op | if len > KEY { long[&file[at..at + len]] } else { info(key_at(file, at, len)) }
            };
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
        // Empty slots have info 0 too: harmless, their key matches no token.
        // The last slot keeps MISS.
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
        // SAFETY: SSE2 is enabled for the whole build.
        (unsafe { _mm_setzero_si128() }, (slot * size_of::<u128>()) as u64)
    }
}

/// A BOOL query as triggered by one of its cover terms.
#[derive(Clone, Copy, Default)]
struct Entry {
    /// `slot | op << 30` per term, padded with AND of the all-ones slot, so the
    /// evaluation is the same six steps for every query.
    terms: [u32; MAX_BOOL_TERMS],
    answer: u32,
    /// Bit k: term k is a cover term that comes earlier in the cover. When it is
    /// present in the block, that term's entry evaluates the query instead.
    /// The `always` entries have no cover: for them, the number of terms.
    earlier: u32,
}

/// What the scan does with each query, decided from the sample.
#[derive(Default)]
struct Plan {
    /// BOOL entries triggered by slot t: `entries[group[t]..group[t + 1]]`.
    group: Vec<u32>,
    entries: Vec<Entry>,
    /// Per entry, two slots of terms that every match contains (see `precheck`).
    pre: Vec<(u32, u32)>,
    /// Bit t: slot t triggers entries.
    trig: Vec<u64>,
    /// BOOL entries evaluated in every superblock, on rows of `Scan::wide`.
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
    /// The BOOL terms have the new ids 0..nb.
    nb: usize,
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
        let line_terms = |start: u32, len: u32| &terms[start as usize..(start + len) as usize];
        // The expected share of 64-document blocks that contain t: the cost of
        // triggering on it.
        let cost = |t: u32| 1.0 - (1.0 - sample_df[t as usize] as f64 / sampled as f64).powi(64);
        // Per old id: bit 0 for a BOOL term, bit 1 for a term of an `always` query.
        let kind: Vec<AtomicU8> = (0..sample_df.len()).map(|_| AtomicU8::new(0)).collect();
        // Whether each line is a BOOL query evaluated in every superblock.
        let always_line = |line: usize| {
            let (start, len, phrase) = spans[line];
            let (cover, n) = cover(line_terms(start, len), cost);
            let always = !phrase && cover[..n].iter().map(|&t| cost(t)).sum::<f64>() > ALWAYS;
            for &word in line_terms(start, len).iter().filter(|_| !phrase) {
                kind[(word & TERM) as usize].fetch_or(1 | (always as u8) << 1, Relaxed);
            }
            always
        };
        let always_part = |p| part(lines, p, parts).map(always_line).collect::<Vec<_>>();
        let always_lines = par(parts, &always_part).concat();
        let kind: Vec<u8> = kind.into_iter().map(AtomicU8::into_inner).collect();
        // Renumber the terms: BOOL terms first, those of the `always` lines
        // first among them, each part by sampled frequency, most frequent first,
        // so the presence words and entries the scan uses most share cache lines.
        // A counting sort: kind 3, kind 1, then phrase-only terms in id order.
        let most = sample_df.iter().copied().max().unwrap_or(0) + 1;
        let key = |t: usize| match kind[t] {
            0 => 2 * most,
            k => (3 - k as u32) / 2 * most + most - 1 - sample_df[t],
        };
        let keyed = (0..sample_df.len()).map(|t| (key(t), t as u32)).collect();
        let (_, order) = csr(&[keyed], 2 * most as usize + 1);
        let nb = kind.iter().filter(|&&k| k > 0).count();
        let rows = kind.iter().filter(|&&k| k == 3).count();
        let mut rank = vec![0; order.len()];
        for (id, &old) in order.iter().enumerate() {
            rank[old as usize] = id as u32;
        }
        // The costs by new id, so `cover` finds the sets `always_line` found.
        let cost = |t: u32| cost(order[t as usize]);
        let lines_part = |p: usize| {
            let mine = part(lines, p, parts);
            let mut by_trigger = Vec::with_capacity(6 * mine.len());
            let (mut by_anchor, mut always) = (Vec::with_capacity(mine.len()), Vec::new());
            // This part's terms, renumbered: they follow each other in `terms`.
            let first = spans.get(mine.start).map_or(terms.len(), |s| s.0 as usize);
            let end = spans.get(mine.end).map_or(terms.len(), |s| s.0 as usize);
            let renumber = |&w: &u32| rank[(w & TERM) as usize] | w & !TERM;
            let renumbered: Vec<u32> = terms[first..end].iter().map(renumber).collect();
            for line in mine {
                let (start, len, phrase) = spans[line];
                let words = &renumbered[start as usize - first..(start + len) as usize - first];
                let answer = line as u32; // renumbered below
                if phrase {
                    // The first of the rarest terms.
                    let rarest = |&k: &usize| sample_df[order[words[k] as usize] as usize];
                    let offset = (0..words.len()).min_by_key(rarest).unwrap();
                    by_anchor.push((words[offset], (answer, offset as u32, start, len)));
                    continue;
                }
                // Padded with AND of the all-ones slot (or row).
                let pad = if always_lines[line] { rows + 1 } else { nb + 1 };
                let mut entry = Entry {
                    terms: [pad as u32; MAX_BOOL_TERMS],
                    answer,
                    earlier: len,
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
                    entry.earlier = (0..words.len()).map(early).sum();
                    by_trigger.push((c, entry));
                }
            }
            (by_trigger, by_anchor, always, renumbered)
        };
        let mut out = par(parts, &lines_part);
        let (by_trigger, by_anchor): (Vec<_>, Vec<_>) = out.iter_mut().map(|o| (take(&mut o.0), take(&mut o.1))).unzip();
        let mut always: Vec<_> = out.iter().flat_map(|o| o.2.iter().copied()).collect();
        let terms: Vec<_> = out.iter().flat_map(|o| o.3.iter().copied()).collect();
        // By length and operators, so that `end_super` predicts its branches.
        always.sort_unstable_by_key(|e| (e.earlier, e.terms.map(|word| word >> 30)));
        let mut plan = Plan::default();
        (plan.group, plan.entries) = csr(&by_trigger, nb + 1);
        plan.pre = plan.entries.iter().map(|e| precheck(e, nb as u32 + 1)).collect();
        plan.trig = vec![0; nb / 64 + 1];
        (0..=nb).filter(|&t| plan.group[t] != plan.group[t + 1]).for_each(|t| plan.trig[t / 64] |= 1 << (t % 64));
        (plan.anchor, plan.anchored) = csr(&by_anchor, order.len());
        // Answers numbered in the order the scan visits them: the `always` ones
        // first, so that each superblock reads and writes their state as one
        // stream (47 KB instead of 4,000 scattered lines), then those of the
        // entries in their order, then the phrases.
        plan.answer_of = vec![u32::MAX; lines];
        let mut number = |line: &mut u32| {
            if plan.answer_of[*line as usize] == u32::MAX {
                plan.answer_of[*line as usize] = plan.line_of.len() as u32;
                plan.line_of.push(*line);
            }
            *line = plan.answer_of[*line as usize];
        };
        always.iter_mut().for_each(|e| number(&mut e.answer));
        plan.entries.iter_mut().for_each(|e| number(&mut e.answer));
        // Every line has an entry (a cover has at least one term) or is a phrase.
        plan.anchored.iter_mut().for_each(|a| number(&mut a.0));
        let flag = |id: usize| (plan.anchor[id] != plan.anchor[id + 1]) as u32 * ANCHOR;
        plan.info = rank.iter().map(|&id| id | flag(id as usize)).collect();
        (plan.always, plan.terms, plan.rows, plan.nb) = (always, terms, rows, nb);
        plan
    }
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

/// Two slots of terms that every match of the entry contains, the two highest
/// (the rarest: ids go by frequency), or the all-ones `pad`: if the two words
/// have no document in common, the entry has no match in the block. Left to
/// right, a match contains the first term; AND adds a term, NOT none, and OR
/// keeps only its own term, if it was required already.
fn precheck(e: &Entry, pad: u32) -> (u32, u32) {
    let mut req = vec![e.terms[0] & TERM];
    for &word in e.terms[1..].iter().filter(|&&w| w & TERM != pad) {
        req.retain(|&t| word >> 30 != OR || t == word & TERM);
        req.extend((word >> 30 == AND).then_some(word & TERM));
    }
    req.sort_unstable();
    let top = |k: usize| *req.iter().rev().nth(k).or(req.last()).unwrap_or(&pad);
    (top(0), top(1))
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
    /// Per BOOL term, bit i: document `block + i` contains it. Slot nb collects
    /// the tokens of no BOOL query; slot nb + 1 is all ones.
    w: Vec<u64>,
    /// The slots set in this block (`touched[..n]`), to evaluate and clear.
    touched: Vec<u32>,
    n: usize,
    /// The touched slots that trigger entries, and the entries that pass their
    /// precheck.
    hot: Vec<u32>,
    live: Vec<u32>,
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
        let (nb, rows) = plan.map_or((0, 0), |p| (p.nb, p.rows));
        let answers = plan.map_or(0, |p| p.line_of.len());
        let (mut w, mut wide) = (written_zeros(nb + 2), vec![Row([0; 4]); rows + 2]);
        (w[nb + 1], wide[rows + 1]) = (!0, Row([!0; 4]));
        Scan {
            vocab,
            plan,
            df,
            w,
            touched: vec![0; nb + 2],
            n: 0,
            hot: vec![0; nb + 2],
            live: Vec::with_capacity(plan.map_or(0, |p| p.entries.len())),
            wide,
            pending: Vec::new(),
            toks: Vec::new(),
            anchors: Vec::new(),
            doc: 0,
            answers: Answers {
                state: written_zeros(answers),
                // One more: `add` always writes the slot after the logged ids.
                log: vec![0; answers * 10 + 1],
                logged: 0,
            },
            last: vec![u32::MAX; plan.map_or(0, |p| p.anchored.len())],
        }
    }

    /// Scans the whole lines in `docs[start..end]`, whose first id is `first`.
    fn run(mut self, docs: &'static [u8], start: usize, end: usize, first: u32) -> Self {
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
                    // SAFETY: `reserve(32)` above made room for this window's tokens.
                    if back > -(KEY as isize) {
                        let key = key_at(docs, p, (1 - back) as usize);
                        let off = phf.slot(key) * size_of::<u128>();
                        prefetch(table.cast::<u8>(), off);
                        unsafe { out.write((key, off as u64)) };
                    } else {
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
    /// words. Each step runs over the whole document, so that the memory accesses
    /// of one step overlap: the slots were fetched when the tokens were found.
    /// (Fetching the words too cost more instructions than it saved.)
    fn look_up(&mut self, bit: u64) {
        // The number of BOOL terms.
        let (nb, table) = (self.w.len() - 2, self.vocab.table.as_ptr());
        self.toks.reserve(self.pending.len());
        let (toks, mut len) = (self.toks.as_mut_ptr(), self.toks.len());
        for &(key, off) in &self.pending {
            // SAFETY: pending offsets are those of slots of the table.
            let info = probe(unsafe { table.byte_add(off as usize) }, key);
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
        let (w, touched, mut n) = (self.w.as_mut_ptr(), self.touched.as_mut_ptr(), self.n);
        for &id in &self.toks {
            // Branch-free: the slot is always written at touched[n], and kept
            // (n advances) only when it is seen for the first time in this
            // block, its word still zero.
            let slot = (id as usize).min(nb);
            // SAFETY: slot <= nb < w.len(), and n <= nb + 1 < touched.len(): n
            // counts distinct slots, at most nb + 1.
            unsafe {
                let x = *w.add(slot);
                *touched.add(n) = slot as u32;
                n += (x == 0) as usize;
                *w.add(slot) = x | bit;
            }
        }
        self.n = n;
    }

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
                        self.answers.add(answer, 1 << (doc % 64), doc & !63);
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
        let w = &mut self.w;
        if let Some(plan) = self.plan {
            // In passes, so that the cold lines of an evaluation are fetched
            // well before it. First the touched slots that trigger entries (a
            // third). Then per slot, the entries of its range that pass their
            // precheck (two in three), fetching the range 16 slots ahead, the
            // first entry's precheck pair 8 ahead and the pair's words 4 ahead.
            // Then their evaluation, fetching the entry 8 ahead and the words
            // and state of the entry 4 ahead.
            let (hot, mut h) = (self.hot.as_mut_ptr(), 0);
            for &t in &self.touched[..self.n] {
                // SAFETY: hot has room for every touched slot.
                unsafe { hot.add(h).write(t) };
                h += (plan.trig[t as usize / 64] >> (t % 64) & 1) as usize;
            }
            // SAFETY: slots are below w.len(), hot slots at most nb (group has
            // nb + 2 entries, slot 0 stands in past the end), and group's entry
            // indices index pre.
            let word = |s: u32| unsafe { *w.get_unchecked(s as usize) };
            let (hot, group, pre) = (&self.hot[..h], plan.group.as_ptr(), plan.pre.as_ptr());
            let first = |i: usize| unsafe { *group.add(*hot.get(i).unwrap_or(&0) as usize) };
            let (live, mut l) = (self.live.as_mut_ptr(), 0);
            for (i, &t) in hot.iter().enumerate() {
                prefetch(group, *hot.get(i + 16).unwrap_or(&0) as usize);
                prefetch(pre, first(i + 8) as usize);
                // SAFETY: as above; live has room for every entry.
                unsafe {
                    let (a, b) = *pre.add(first(i + 4) as usize);
                    (prefetch(w.as_ptr(), a as usize), prefetch(w.as_ptr(), b as usize));
                    for e in *group.add(t as usize)..*group.add(t as usize + 1) {
                        let (a, b) = *pre.add(e as usize);
                        live.add(l).write(e);
                        l += (word(a) & word(b) != 0) as usize;
                    }
                }
            }
            // SAFETY: the first l were written.
            unsafe { self.live.set_len(l) };
            let (entries, state) = (plan.entries.as_ptr(), self.answers.state.as_ptr());
            for (k, &i) in self.live.iter().enumerate() {
                prefetch(entries, self.live.get(k + 8).map_or(0, |&i| i) as usize);
                // SAFETY: entries of the plan (entry 0 at the end).
                let (next, e) = unsafe { (&*entries.add(*self.live.get(k + 4).unwrap_or(&0) as usize), &*entries.add(i as usize)) };
                for &t in &next.terms {
                    prefetch(w.as_ptr(), (t & TERM) as usize);
                }
                prefetch(state, next.answer as usize);
                // All ones when bit 0 of b is set: bit(op) for NOT and OR,
                // bit(op >> 1) only for OR.
                let bit = |b: u32| 0u64.wrapping_sub((b & 1) as u64);
                // AND: r & x. NOT: r & !x. OR: (r & !x) | x = r | x.
                let step = |r, &t: &u32| (r & (word(t & TERM) ^ bit(t >> 30))) | (word(t & TERM) & bit(t >> 31));
                let r = e.terms.iter().fold(0u64, step);
                // A branch on the few entries with earlier cover terms.
                let present = |k| word(e.terms[k] & TERM) & bit(e.earlier >> k) != 0;
                let seen = e.earlier != 0 && (0..MAX_BOOL_TERMS).any(present);
                self.answers.add(e.answer, if seen { 0 } else { r }, base);
            }
        }
        let (rows, block) = (self.wide.len() - 2, (base / 64 % (SUPER / 64)) as usize);
        let wide = self.wide.as_mut_ptr();
        for &t in &self.touched[..self.n] {
            // SAFETY: min(t, rows) < wide.len(), t <= nb < w.len(), block < 4.
            unsafe {
                let row = &mut (*wide.add((t as usize).min(rows))).0;
                *row.get_unchecked_mut(block) = *w.get_unchecked(t as usize);
                *w.get_unchecked_mut(t as usize) = 0;
            }
        }
        self.n = 0;
    }

    /// Evaluates the `always` queries on the superblock of 256 documents from
    /// `base`, like `end_block` but on four words at once, then clears it.
    fn end_super(&mut self, base: u32) {
        let Some(plan) = self.plan else { return };
        let wide = self.wide.as_ptr() as *const __m256i;
        for e in &plan.always {
            // For `always` entries, `earlier` holds the number of terms.
            let terms = &e.terms[..e.earlier as usize];
            // SAFETY: 32-byte loads of rows, which are 32-byte aligned (Row is
            // repr(align(32))); the plan's rows are below wide.len(). AVX2 is
            // enabled for the whole build.
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
            self.answers.add_wide(e.answer, hits, base);
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
    /// `answer << 32 | id` for the first ten matches of each answer, in
    /// `log[..logged]`. Its zero pages cost no memory until they are written.
    log: Vec<u64>,
    logged: usize,
}

impl Answers {
    /// Adds the documents `base + i` for each bit i of `hits`, which may be 0.
    fn add(&mut self, answer: u32, hits: u64, base: u32) {
        // SAFETY: answers index the state; an answer logs at most 10 ids, so
        // logged <= 10 * answers < log.len().
        let s = unsafe { &mut *self.state.as_mut_ptr().add(answer as usize) };
        let log = self.log.as_mut_ptr();
        let count = *s >> SUM_BITS;
        let entry = |bits: u64| (answer as u64) << 32 | (base + bits.trailing_zeros()) as u64;
        // The first id without a branch: always written, kept only if wanted.
        // SAFETY: as above.
        unsafe { *log.add(self.logged) = entry(hits) };
        self.logged += (count < 10 && hits != 0) as usize;
        // Then up to 10 in all, while hits are left.
        let (n, mut rest) = (hits.count_ones() as u64, hits & hits.wrapping_sub(1));
        for _ in count + 1..(count + n).min(10) {
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
        if self.state[answer as usize] >> SUM_BITS >= 10 {
            let (n, positions) = count_positions(hits);
            self.state[answer as usize] += n << SUM_BITS | (n * base as u64 + positions);
            // SAFETY: AVX2 is enabled for the whole build.
        } else if unsafe { _mm256_testz_si256(hits, hits) } == 0 {
            // Still logging first ids: a word at a time.
            // SAFETY: __m256i and [u64; 4] have the same size, and any bits are valid.
            let words: [u64; 4] = unsafe { std::mem::transmute(hits) };
            (0..4).for_each(|i| self.add(answer, words[i], base + 64 * i as u32));
        }
    }
}

/// The number of set bits in `v`, and the sum of their positions, from tables of
/// the 4-bit nibbles: the nibble at bits 4m..4m + 4 adds 4m per set bit, plus its
/// own bit positions.
fn count_positions(v: __m256i) -> (u64, u64) {
    // SAFETY: AVX2 is enabled for the whole build.
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
