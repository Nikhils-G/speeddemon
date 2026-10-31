//! Track B (Index): answer 50,000 BOOL and PHRASE queries over a 254 MB corpus.
//!
//! There is no index: the corpus is scanned once, in parallel slices, and every
//! query is answered during that scan, so nothing grows with the corpus.
//! 1. The queries come first. Only their terms are looked up: a small shared table
//!    of the query terms most frequent in a sample answers most tokens, a Bloom
//!    filter turns away most words no query uses, and the full term table
//!    answers the rest.
//! 2. BOOL queries are evaluated per block of 64 documents. While a block is
//!    scanned, each query term present gets a 64-bit word with one bit per
//!    document, so at the block end AND, OR and NOT are word operations, applied
//!    strictly left to right.
//! 3. A query is evaluated only in blocks that contain one of its cover terms, a
//!    set of terms every match contains (see `cover`), chosen to be rare from the
//!    sample; queries with common covers are evaluated in every block.
//! 4. Each phrase is anchored on its rarest term and checked against the token
//!    ids of the document when it ends, while they are still in L1.
//! 5. Per query and thread, the scan keeps the match count, the sum of the ids
//!    (from popcounts of bit positions) and the first ten ids. The threads' slices
//!    are in document order, so merging them in order gives the ten smallest.

mod mmap;

use std::arch::x86_64::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::thread;

#[cfg(not(target_feature = "avx2"))]
compile_error!("track-b needs AVX2: build with RUSTFLAGS=\"-C target-cpu=native\"");

/// A token's info word: its term id, plus ANCHOR if a phrase is anchored on it.
/// A token that no query uses gets MISS.
const ANCHOR: u32 = 1 << 31;
const MISS: u32 = ANCHOR - 1;
/// Terms of up to KEY bytes live in the bucket tables, keyed by their bytes.
const KEY: usize = 12;
const MAX_TERM: usize = 32;
const MAX_BOOL_TERMS: usize = 6;
/// A BOOL term word is `slot | op << 30`. The op codes make `end_block` branch-free.
const TERM: u32 = (1 << 30) - 1;
const AND: u32 = 0;
const NOT: u32 = 1;
const OR: u32 = 3;
/// Corpus bytes sampled (spread over the slices) to estimate term frequencies.
const SAMPLE_BYTES: usize = 2 << 20;
/// A query whose cover terms are expected in more blocks than this (as a share,
/// summed over the terms) is evaluated in every block: as cheap, and a block
/// then reads its entries in one run instead of one group per term.
const ALWAYS: f64 = 0.5;
/// The hot table has 2^13 buckets of 4 terms: 512 KB, well inside a core's L2.
const HOT_BITS: u32 = 13;
/// A thread's sum of matching ids is packed under its count in one u64.
const SUM_BITS: u32 = 40;

fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    if let Err(message) = run(&data_dir, &out_dir) {
        eprintln!("track-b: {message}");
        std::process::exit(1);
    }
}

fn run(data_dir: &str, out_dir: &str) -> Result<(), String> {
    mmap::no_huge_pages();
    let open = |name: &str| -> Result<&'static [u8], String> {
        let path = format!("{data_dir}/{name}");
        let file = File::open(&path).map_err(|e| format!("open {path}: {e}"))?;
        mmap::map(&file).map_err(|e| format!("mmap {path}: {e}"))
    };
    let docs = open("docs.txt")?;
    let text = &docs[..docs.len() - mmap::PAD];
    let queries_txt = open("queries.txt")?;
    let (queries, mut vocab) = parse_queries(queries_txt)?;
    let doc_count = doc_count(text)?;
    // Every per-thread sum of ids is below N^2 / 2, which must fit in SUM_BITS.
    if doc_count > 1 << 20 {
        return Err(format!("docs.txt: {doc_count} documents is more than this program supports (2^20)"));
    }
    let slices = slices(text, thread::available_parallelism().map_or(1, |n| n.get()))?;
    let nb = vocab.bool_terms as usize;

    // Sample the start of every slice for each term's document frequency.
    let df: Vec<AtomicU32> = (0..vocab.count).map(|_| AtomicU32::new(0)).collect();
    let sampled: u32 = thread::scope(|s| {
        let handles: Vec<_> = (slices.iter())
            .map(|&(start, end, first)| {
                let (vocab, df) = (&vocab, &df);
                let stop = line_end(text, start + SAMPLE_BYTES / slices.len()).min(end);
                s.spawn(move || Scan::new(docs, vocab, None, df, nb, 0).run(start, stop, first).map(|scan| scan.doc + 1 - first))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum::<Result<u32, String>>()
    })?;
    let df: Vec<u32> = df.into_iter().map(AtomicU32::into_inner).collect();
    let lines = queries.lines.len();
    let plan = Plan::new(queries, &df, sampled, nb);
    vocab.finish(&plan, &df);

    let results: Vec<(Vec<u64>, Vec<u64>, u32)> = thread::scope(|s| {
        let handles: Vec<_> = (slices.iter())
            .map(|&(start, end, first)| {
                let (vocab, plan) = (&vocab, &plan);
                s.spawn(move || {
                    let scan = Scan::new(docs, vocab, Some(plan), &[], nb, lines).run(start, end, first)?;
                    let Answers { state, mut log, logged } = scan.answers;
                    // Sorted, the log holds each line's first ids in order.
                    log.truncate(logged);
                    log.sort_unstable();
                    Ok((state, log, scan.doc))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, String>>()
    })?;
    let next_firsts = slices.iter().skip(1).map(|s| s.2).chain([doc_count]);
    if slices.first().is_some_and(|s| s.2 != 0) || results.iter().zip(next_firsts).any(|(r, next)| r.2 + 1 != next) {
        return Err("docs.txt: ids must be 0, 1, 2, ... in order".into());
    }

    // Format in parallel, one range of lines per thread.
    let parts = slices.len().max(1);
    let chunks: Vec<Vec<u8>> = thread::scope(|s| {
        let results = &results;
        let handles: Vec<_> = (0..parts).map(|p| s.spawn(move || format_lines(results, lines * p / parts..lines * (p + 1) / parts))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let path = format!("{out_dir}/results.txt");
    let mut file = File::create(&path).map_err(|e| format!("create {path}: {e}"))?;
    chunks.iter().try_for_each(|chunk| file.write_all(chunk)).map_err(|e| format!("write {path}: {e}"))
}

/// The output for `lines`: per line, the threads' counts and sums added up, and
/// the first ten ids taken from the threads in slice order.
fn format_lines(results: &[(Vec<u64>, Vec<u64>, u32)], lines: std::ops::Range<usize>) -> Vec<u8> {
    let mut out = Vec::with_capacity(lines.len() * 32);
    let mut cursors: Vec<usize> = results.iter().map(|r| r.1.partition_point(|&e| e >> 32 < lines.start as u64)).collect();
    for line in lines {
        let (mut count, mut sum, mut tops) = (0, 0, Vec::with_capacity(10));
        for ((state, log, _), cursor) in results.iter().zip(&mut cursors) {
            count += state[line] >> SUM_BITS;
            sum += state[line] & ((1 << SUM_BITS) - 1);
            while let Some(&entry) = log.get(*cursor).filter(|&&e| e >> 32 == line as u64) {
                if tops.len() < 10 {
                    tops.push(entry as u32);
                }
                *cursor += 1;
            }
        }
        push_decimal(&mut out, count);
        out.push(b' ');
        push_decimal(&mut out, sum);
        for id in tops {
            out.push(b' ');
            push_decimal(&mut out, id as u64);
        }
        out.push(b'\n');
    }
    out
}

struct Queries {
    /// Per line of queries.txt: its terms `terms[start..start + len]`, and
    /// whether it is a phrase.
    lines: Vec<(u32, u32, bool)>,
    /// BOOL terms as `id | op << 30`, the first with OR; phrase terms as ids.
    terms: Vec<u32>,
}

/// Reads all BOOL lines before the phrases, so the terms of BOOL queries get the
/// ids 0..bool_terms and can index a dense per-thread array. `padded` is the
/// file and mmap::PAD zero bytes.
fn parse_queries(padded: &'static [u8]) -> Result<(Queries, Vocab), String> {
    let text = &padded[..padded.len() - mmap::PAD];
    let body = text.strip_suffix(b"\n").unwrap_or(text);
    let lines: Vec<&[u8]> = if body.is_empty() { Vec::new() } else { body.split(|&b| b == b'\n').collect() };
    let bad = |line: &[u8]| format!("queries.txt: bad query {:?}", String::from_utf8_lossy(line));
    let mut q = Queries { lines: vec![(0, 0, false); lines.len()], terms: Vec::new() };
    let mut vocab = Vocab::new(lines.len());
    for (i, &line) in lines.iter().enumerate() {
        let Some(rest) = line.strip_prefix(b"BOOL ") else { continue };
        let start = q.terms.len();
        let mut words = rest.split(|&b| b == b' ');
        let mut op = OR;
        loop {
            q.terms.push(vocab.intern(words.next().ok_or_else(|| bad(line))?, padded)? | op << 30);
            let Some(word) = words.next() else { break };
            op = match word {
                b"AND" => AND,
                b"OR" => OR,
                b"NOT" => NOT,
                _ => return Err(bad(line)),
            };
        }
        if q.terms.len() - start > MAX_BOOL_TERMS {
            return Err(format!("queries.txt: more than {MAX_BOOL_TERMS} terms in {:?}", String::from_utf8_lossy(line)));
        }
        q.lines[i] = (start as u32, (q.terms.len() - start) as u32, false);
    }
    vocab.bool_terms = vocab.count;
    for (i, &line) in lines.iter().enumerate() {
        if line.starts_with(b"BOOL ") {
            continue;
        }
        let inner = line.strip_prefix(b"PHRASE \"").and_then(|r| r.strip_suffix(b"\"")).ok_or_else(|| bad(line))?;
        let start = q.terms.len();
        for term in inner.split(|&b| b == b' ') {
            q.terms.push(vocab.intern(term, padded)?);
        }
        q.lines[i] = (start as u32, (q.terms.len() - start) as u32, true);
    }
    Ok((q, vocab))
}

#[derive(Clone, Copy, Default)]
struct Slot {
    lo: u64,
    hi: u32,
    info: u32,
}

/// Four slots in one cache line, so a probe costs one memory access.
#[derive(Clone, Copy, Default)]
#[repr(align(64))]
struct Bucket([Slot; 4]);

/// Terms of up to 12 bytes keyed by their bytes (zero-padded: token bytes are
/// never zero, so the key is exact and lo = 0 marks an empty slot).
struct Table {
    buckets: Vec<Bucket>,
    shift: u32,
    len: usize,
}

impl Table {
    fn new(bits: u32) -> Self {
        Table { buckets: vec![Bucket::default(); 1 << bits], shift: 64 - bits, len: 0 }
    }

    /// The info of the key in bucket b, or MISS, without a branch: whether and
    /// where a key hits is random. `wide` is the key in both 128-bit lanes.
    #[inline(always)]
    fn probe(&self, b: usize, wide: __m256i) -> u32 {
        let slots = &self.buckets[b].0;
        // SAFETY: two 32-byte loads that cover the 64-byte bucket; AVX2 is
        // enabled for the whole build.
        let half = |i| unsafe { _mm256_cmpeq_epi8(_mm256_loadu_si256((slots.as_ptr() as *const __m256i).add(i)), wide) };
        let m = movemask(half(0)) as u64 | (movemask(half(1)) as u64) << 32;
        // Slot j matches when its 12 key bytes do: bits 16j..16j + 12 of m.
        // Adding 1 to such a run of ones carries into bit 16j + 12.
        let hit = ((m & 0x0FFF_0FFF_0FFF_0FFF) + 0x0001_0001_0001_0001) & 0x1000_1000_1000_1000;
        let info = slots[hit.trailing_zeros() as usize / 16 & 3].info;
        if hit != 0 { info } else { MISS }
    }

    /// The info of the key whose `hash` is h, or MISS. A full bucket continues
    /// into the next one, as `insert` does.
    fn get(&self, key: __m128i, h: u64) -> u32 {
        // SAFETY: AVX2 is enabled for the whole build.
        let wide = unsafe { _mm256_broadcastsi128_si256(key) };
        let mut b = (h >> self.shift) as usize;
        loop {
            let info = self.probe(b, wide);
            // Slots fill in order and are never removed: a free last slot ends the probe.
            if info != MISS || self.buckets[b].0[3].lo == 0 {
                return info;
            }
            b = (b + 1) & (self.buckets.len() - 1);
        }
    }

    /// Adds a key that is not in the table yet; false if `overflow` is off and its
    /// home bucket is full.
    fn insert(&mut self, lo: u64, hi: u32, info: u32, overflow: bool) -> bool {
        let mask = self.buckets.len() - 1;
        let mut b = (hash(lo, hi) >> self.shift) as usize;
        loop {
            if let Some(slot) = self.buckets[b].0.iter_mut().find(|s| s.lo == 0) {
                *slot = Slot { lo, hi, info };
                self.len += 1;
                return true;
            }
            if !overflow {
                return false;
            }
            b = (b + 1) & mask;
        }
    }

    fn slots(&mut self) -> impl Iterator<Item = &mut Slot> {
        self.buckets.iter_mut().flat_map(|b| &mut b.0).filter(|s| s.lo != 0)
    }
}

/// The table key of the token `bytes[at..at + len]`, len <= 12: its bytes,
/// ASCII-lowercased and zero-padded to 16. OR-ing 0x20 lowercases A-Z and keeps
/// digits; KEEP keeps only the token's bytes.
#[inline(always)]
fn key_at(bytes: &[u8], at: usize, len: usize) -> __m128i {
    let (raw, keep) = (&bytes[at..at + 16], &KEEP[16 - len..32 - len]);
    // SAFETY: 16-byte loads from two 16-byte slices; AVX2 is enabled for the
    // whole build.
    unsafe {
        let lower = _mm_or_si128(_mm_loadu_si128(raw.as_ptr() as *const __m128i), _mm_set1_epi8(0x20));
        _mm_and_si128(lower, _mm_loadu_si128(keep.as_ptr() as *const __m128i))
    }
}

/// Multiplicative hashing: the top bits depend on every key byte.
fn hash(lo: u64, hi: u32) -> u64 {
    (lo.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ hi as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
}

/// The first 8 and the next 4 bytes of a key.
fn parts(key: __m128i) -> (u64, u32) {
    // SAFETY: SSE4.1 is enabled for the whole build (it comes with AVX2).
    unsafe { (_mm_cvtsi128_si64(key) as u64, _mm_extract_epi32::<2>(key) as u32) }
}

/// A window into it at 16 - len keeps the first len bytes of 16.
static KEEP: [u8; 32] = {
    let mut keep = [0; 32];
    let mut i = 0;
    while i < 16 {
        keep[i] = 0xFF;
        i += 1;
    }
    keep
};

/// Every query term, with its id and (after `finish`) its ANCHOR flag.
struct Vocab {
    /// The terms most frequent in the sample, read-only and shared by all threads.
    hot: Table,
    /// All terms of up to 12 bytes, kept under 2.5 per bucket so few buckets overflow.
    cold: Table,
    /// A blocked Bloom filter of the cold table, three bits in one word per term
    /// (about 1% false positives in 128 KB): a fifth of all tokens are words no
    /// query uses, and it turns them away without a probe of the 2 MB table.
    bloom: Vec<u64>,
    bloom_shift: u32,
    /// Terms of 13 to 32 bytes: about 0.2% of the corpus tokens are that long.
    long: HashMap<&'static [u8], u32>,
    count: u32,
    bool_terms: u32,
}

impl Vocab {
    fn new(lines: usize) -> Self {
        // Sized for about 1.5 distinct terms per query, which saves the rehashes
        // of growing from small; the table still grows if there are more.
        let (hot, cold) = (Table::new(1), Table::new((lines * 3 / 5).next_power_of_two().max(1024).trailing_zeros()));
        Vocab { hot, cold, bloom: vec![!0; 2], bloom_shift: 63, long: HashMap::new(), count: 0, bool_terms: 0 }
    }

    /// The id of `term`, a slice of `padded`.
    fn intern(&mut self, term: &'static [u8], padded: &[u8]) -> Result<u32, String> {
        if term.is_empty() || term.len() > MAX_TERM || !term.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
            return Err(format!("queries.txt: bad term {:?}", String::from_utf8_lossy(term)));
        }
        let id = if term.len() > KEY {
            *self.long.entry(term).or_insert(self.count)
        } else {
            let key = key_at(padded, term.as_ptr() as usize - padded.as_ptr() as usize, term.len());
            let (lo, hi) = parts(key);
            match self.cold.get(key, hash(lo, hi)) {
                MISS => {
                    if (self.cold.len + 1) * 2 > self.cold.buckets.len() * 5 {
                        let bigger = Table::new(65 - self.cold.shift);
                        let old = std::mem::replace(&mut self.cold, bigger);
                        for slot in old.buckets.iter().flat_map(|b| b.0).filter(|s| s.lo != 0) {
                            self.cold.insert(slot.lo, slot.hi, slot.info, true);
                        }
                    }
                    self.cold.insert(lo, hi, self.count, true);
                    self.count
                }
                id => id,
            }
        };
        self.count += (id == self.count) as u32;
        Ok(id)
    }

    /// Renumbers the terms as the plan does and flags the anchors, fills the Bloom
    /// filter, then fills the hot table with the terms most frequent in the
    /// sample, dropping a term when its bucket is full. `df` is indexed by the old
    /// ids.
    fn finish(&mut self, plan: &Plan, df: &[u32]) {
        let renumber = |old: u32| {
            let id = plan.rank[old as usize];
            id | (plan.anchor[id as usize] != plan.anchor[id as usize + 1]) as u32 * ANCHOR
        };
        let words = (self.cold.len / 8).next_power_of_two().max(2);
        (self.bloom, self.bloom_shift) = (vec![0; words], 64 - words.trailing_zeros());
        let mut common: Vec<(u32, Slot)> = Vec::new();
        for s in self.cold.slots() {
            common.push((df[s.info as usize], *s));
            s.info = renumber(s.info);
            let (word, bits) = bloom_bits(hash(s.lo, s.hi), self.bloom_shift);
            self.bloom[word] |= bits;
        }
        self.long.values_mut().for_each(|info| *info = renumber(*info));
        common.retain(|c| c.0 > 0);
        common.sort_unstable_by_key(|c| std::cmp::Reverse(c.0));
        self.hot = Table::new(HOT_BITS);
        for (_, s) in common {
            self.hot.insert(s.lo, s.hi, renumber(s.info), false);
        }
    }

    #[inline(always)]
    fn get(&self, key: __m128i) -> u32 {
        let (lo, hi) = parts(key);
        let h = hash(lo, hi);
        // SAFETY: AVX2 is enabled for the whole build.
        let info = self.hot.probe((h >> self.hot.shift) as usize, unsafe { _mm256_broadcastsi128_si256(key) });
        // One branch, taken only for query terms missing from the hot table (and
        // Bloom false positives).
        let (word, bits) = bloom_bits(h, self.bloom_shift);
        if info == MISS && self.bloom[word] & bits == bits { self.cold.get(key, h) } else { info }
    }

    fn get_long(&self, token: &[u8]) -> u32 {
        if token.len() > MAX_TERM {
            return MISS;
        }
        let mut lower = [0u8; MAX_TERM];
        lower[..token.len()].copy_from_slice(token);
        lower.make_ascii_lowercase();
        self.long.get(&lower[..token.len()]).copied().unwrap_or(MISS)
    }
}

/// The Bloom filter word of a hash, and its three bits there (from well-mixed
/// middle bits of the hash).
fn bloom_bits(h: u64, shift: u32) -> (usize, u64) {
    ((h >> shift) as usize, 1 << (h >> 26 & 63) | 1 << (h >> 32 & 63) | 1 << (h >> 38 & 63))
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
    earlier: u32,
}

#[derive(Clone, Copy, Default)]
struct Anchored {
    answer: u32,
    /// The anchor's position in the phrase.
    offset: u32,
    start: u32,
    len: u32,
}

/// What the scan does with each query, decided from the sample.
struct Plan {
    /// BOOL entries triggered by slot t: `entries[group[t]..group[t + 1]]`.
    group: Vec<u32>,
    entries: Vec<Entry>,
    /// Phrases anchored on term t: `anchored[anchor[t]..anchor[t + 1]]`.
    anchor: Vec<u32>,
    anchored: Vec<Anchored>,
    /// The queries' term words, with the new ids.
    terms: Vec<u32>,
    /// The new id of each term.
    rank: Vec<u32>,
}

impl Plan {
    fn new(q: Queries, sample_df: &[u32], sampled: u32, nb: usize) -> Self {
        // Renumber the BOOL terms by sampled frequency, most frequent first, so
        // the presence words and entries the scan uses most share cache lines.
        let mut rank: Vec<u32> = (0..sample_df.len() as u32).collect();
        let mut order: Vec<u32> = (0..nb as u32).collect();
        order.sort_unstable_by_key(|&t| std::cmp::Reverse(sample_df[t as usize]));
        let mut df = sample_df.to_vec();
        for (id, &old) in order.iter().enumerate() {
            rank[old as usize] = id as u32;
            df[id] = sample_df[old as usize];
        }
        let mut terms = q.terms;
        terms.iter_mut().for_each(|word| *word = rank[(*word & TERM) as usize] | *word & !TERM);
        // The expected share of 64-document blocks that contain t: the cost of
        // triggering on it.
        let cost = |t: u32| 1.0 - (1.0 - df[t as usize] as f64 / sampled.max(1) as f64).powi(64);
        let (mut by_trigger, mut by_anchor) = (Vec::new(), Vec::new());
        for (line, &(start, len, phrase)) in q.lines.iter().enumerate() {
            let words = &terms[start as usize..(start + len) as usize];
            let answer = line as u32; // the index of the line's answer in each thread's state
            if phrase {
                let (offset, &anchor) = words.iter().enumerate().min_by_key(|&(_, &t)| df[t as usize]).unwrap();
                by_anchor.push((anchor, Anchored { answer, offset: offset as u32, start, len }));
                continue;
            }
            let mut entry = Entry { terms: [nb as u32 + 1; MAX_BOOL_TERMS], answer, earlier: 0 };
            entry.terms[..words.len()].copy_from_slice(words);
            let cover = cover(words, cost);
            if cover.iter().map(|&c| cost(c)).sum::<f64>() > ALWAYS {
                // Grouped under the all-ones slot, which every block evaluates.
                by_trigger.push((nb as u32 + 1, entry));
                continue;
            }
            for (i, &c) in cover.iter().enumerate() {
                let earlier = (words.iter().enumerate()).filter(|(_, w)| cover[..i].contains(&(*w & TERM))).fold(0, |m, (k, _)| m | 1 << k);
                by_trigger.push((c, Entry { earlier, ..entry }));
            }
        }
        let (group, entries) = csr(by_trigger, nb + 2);
        let (anchor, anchored) = csr(by_anchor, df.len());
        Plan { group, entries, anchor, anchored, terms, rank }
    }
}

/// A set of terms of which every match contains at least one, built left to
/// right: after `AND t` every match contains t, so {t} replaces the set when it
/// is cheaper; after `OR t` a match contains t or a term of the set; `NOT`
/// only removes matches.
fn cover(terms: &[u32], cost: impl Fn(u32) -> f64) -> Vec<u32> {
    let mut cover: Vec<u32> = Vec::new();
    for &word in terms {
        let t = word & TERM;
        match word >> 30 {
            OR if !cover.contains(&t) => cover.push(t),
            AND if cost(t) < cover.iter().map(|&c| cost(c)).sum() => cover = vec![t],
            _ => {}
        }
    }
    cover
}

/// Groups values by key: values of key k are `values[start[k]..start[k + 1]]`,
/// in their original order.
fn csr<T: Copy + Default>(pairs: Vec<(u32, T)>, keys: usize) -> (Vec<u32>, Vec<T>) {
    let mut start = vec![0u32; keys + 1];
    for &(k, _) in &pairs {
        start[k as usize + 1] += 1;
    }
    for k in 0..keys {
        start[k + 1] += start[k];
    }
    let (mut next, mut values) = (start.clone(), vec![T::default(); pairs.len()]);
    for (k, value) in pairs {
        values[next[k as usize] as usize] = value;
        next[k as usize] += 1;
    }
    (start, values)
}

/// One thread's pass over a slice of the corpus. Without a plan it only counts
/// document frequencies into `df`, for the sample.
struct Scan<'a> {
    docs: &'static [u8],
    vocab: &'a Vocab,
    plan: Option<&'a Plan>,
    df: &'a [AtomicU32],
    /// Number of BOOL terms.
    nb: usize,
    /// Per BOOL term, bit i: document `block + i` contains it. Slot nb collects
    /// the tokens of no BOOL query; slot nb + 1 is all ones.
    w: Vec<u64>,
    /// The slots set in this block (`touched[..n]`), to evaluate and clear.
    touched: Vec<u32>,
    n: usize,
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
    fn new(docs: &'static [u8], vocab: &'a Vocab, plan: Option<&'a Plan>, df: &'a [AtomicU32], nb: usize, lines: usize) -> Self {
        Scan {
            docs,
            vocab,
            plan,
            df,
            nb,
            w: (0..nb + 2).map(|slot| if slot == nb + 1 { !0 } else { 0 }).collect(),
            touched: vec![0; nb + 1],
            n: 0,
            toks: Vec::new(),
            anchors: Vec::new(),
            doc: 0,
            answers: Answers::new(lines),
            last: vec![u32::MAX; plan.map_or(0, |p| p.anchored.len())],
        }
    }

    /// Scans the whole lines in `docs[start..end]`, whose first id is `first`.
    fn run(mut self, start: usize, end: usize, first: u32) -> Result<Self, String> {
        let docs = self.docs;
        self.doc = first.wrapping_sub(1);
        let (mut bit, mut carry) = (0u64, 0u64);
        let mut mask = token_mask(docs, start, end);
        for at in (start..end).step_by(64) {
            let next = token_mask(docs, at + 64, end);
            let mut starts = mask & !(mask << 1 | carry);
            carry = mask >> 63;
            // A token is at most 32 bytes (longer ones match no term), so it
            // ends within this chunk and the next.
            let window = mask as u128 | (next as u128) << 64;
            while starts != 0 {
                let b = starts.trailing_zeros();
                starts &= starts - 1;
                // Only the low 64 bits: a token that runs past them is too long
                // to be a term anyway.
                let len = (!((window >> b) as u64)).trailing_zeros() as usize;
                let p = at + b as usize;
                // The raw byte: `)` | 0x20 would look like a TAB.
                let info = if docs[p + len] == b'\t' {
                    bit = self.next_doc(&docs[p..p + len])?;
                    continue;
                } else if len <= KEY {
                    self.vocab.get(key_at(docs, p, len))
                } else {
                    self.vocab.get_long(&docs[p..p + len])
                };
                let id = info & MISS;
                // Branch-free: a slot seen for the first time in this block is
                // kept in `touched`.
                let slot = (id as usize).min(self.nb);
                let x = self.w[slot];
                self.touched[self.n] = slot as u32;
                self.n += (x == 0) as usize;
                self.w[slot] = x | bit;
                if info & ANCHOR != 0 {
                    self.anchors.push(self.toks.len() as u32);
                }
                self.toks.push(id);
            }
            mask = next;
        }
        self.end_doc();
        self.end_block(self.doc & !63);
        Ok(self)
    }

    /// Ends the current document at the id token of the next one; returns its bit.
    fn next_doc(&mut self, digits: &[u8]) -> Result<u64, String> {
        let id = parse_id(digits)?;
        if id != self.doc.wrapping_add(1) {
            return Err("docs.txt: ids must be 0, 1, 2, ... in order".into());
        }
        self.end_doc();
        if id % 64 == 0 && self.n > 0 {
            self.end_block(id - 64);
        }
        self.doc = id;
        Ok(1 << (id % 64))
    }

    fn end_doc(&mut self) {
        let doc = self.doc;
        match self.plan {
            None => {
                self.toks.sort_unstable();
                self.toks.dedup();
                for &t in self.toks.iter().filter(|&&t| t != MISS) {
                    self.df[t as usize].fetch_add(1, Relaxed);
                }
            }
            Some(plan) => {
                for &at in &self.anchors {
                    let anchor = self.toks[at as usize] as usize;
                    for i in plan.anchor[anchor] as usize..plan.anchor[anchor + 1] as usize {
                        let a = plan.anchored[i];
                        let Some(from) = (at as usize).checked_sub(a.offset as usize) else { continue };
                        let phrase = &plan.terms[a.start as usize..(a.start + a.len) as usize];
                        if self.toks.get(from..from + phrase.len()) == Some(phrase) && self.last[i] != doc {
                            self.last[i] = doc;
                            self.answers.add(a.answer, 1 << (doc % 64), doc & !63);
                        }
                    }
                }
            }
        }
        self.toks.clear();
        self.anchors.clear();
    }

    /// Evaluates the queries triggered by the terms of this block (and those of
    /// the all-ones slot, see ALWAYS), the 64 documents from `base`, then clears
    /// the block. The evaluation has no branch: whether a query matches is too
    /// random to predict.
    fn end_block(&mut self, base: u32) {
        let w = &mut self.w;
        if let Some(plan) = self.plan {
            for t in [self.nb + 1].into_iter().chain(self.touched[..self.n].iter().map(|&t| t as usize)) {
                for e in &plan.entries[plan.group[t] as usize..plan.group[t + 1] as usize] {
                    let (mut r, mut seen) = (0u64, 0u64);
                    for (k, &word) in e.terms.iter().enumerate() {
                        let x = w[(word & TERM) as usize];
                        let bit = |b: u32| 0u64.wrapping_sub((b & 1) as u64);
                        // AND: r & x. NOT: r & !x. OR: (r & !x) | x = r | x.
                        r = (r & (x ^ bit(word >> 30))) | (x & bit(word >> 31));
                        seen |= x & bit(e.earlier >> k);
                    }
                    self.answers.add(e.answer, if seen == 0 { r } else { 0 }, base);
                }
            }
        }
        for &t in &self.touched[..self.n] {
            w[t as usize] = 0;
        }
        self.n = 0;
    }
}

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
    fn new(answers: usize) -> Self {
        Answers { state: vec![0; answers], log: vec![0; answers * 10 + 1], logged: 0 }
    }

    /// Adds the documents `base + i` for each bit i of `hits`, which may be 0.
    fn add(&mut self, answer: u32, hits: u64, base: u32) {
        let s = &mut self.state[answer as usize];
        let count = *s >> SUM_BITS;
        let entry = |bits: u64| (answer as u64) << 32 | (base + bits.trailing_zeros()) as u64;
        // The first id without a branch: always written, kept only if wanted.
        self.log[self.logged] = entry(hits);
        self.logged += (count < 10 && hits != 0) as usize;
        let mut rest = hits & hits.wrapping_sub(1);
        for _ in count + 1..10 {
            if rest == 0 {
                break;
            }
            self.log[self.logged] = entry(rest);
            self.logged += 1;
            rest &= rest - 1;
        }
        // The bit positions' sum, one binary digit at a time: digit j is set in
        // the positions that the mask POSITION_BITS[j] covers.
        const POSITION_BITS: [u64; 6] =
            [0xAAAA_AAAA_AAAA_AAAA, 0xCCCC_CCCC_CCCC_CCCC, 0xF0F0_F0F0_F0F0_F0F0, 0xFF00_FF00_FF00_FF00, 0xFFFF_0000_FFFF_0000, 0xFFFF_FFFF_0000_0000];
        let positions: u64 = (POSITION_BITS.iter().enumerate()).map(|(j, &m)| ((hits & m).count_ones() as u64) << j).sum();
        let n = hits.count_ones() as u64;
        *s += n << SUM_BITS | (n * base as u64 + positions);
    }
}

/// Bit i is set when byte `at + i` is a token byte ([A-Za-z0-9]) before `end`.
fn token_mask(docs: &[u8], at: usize, end: usize) -> u64 {
    let half = |at: usize| {
        let bytes = &docs[at..at + 32];
        // SAFETY: an unaligned 32-byte load from a 32-byte slice; AVX2 is enabled
        // for the whole build.
        movemask(unsafe {
            let v = _mm256_loadu_si256(bytes.as_ptr() as *const __m256i);
            let between = |x, lo: u8, hi: u8| {
                _mm256_and_si256(_mm256_cmpgt_epi8(x, _mm256_set1_epi8(lo as i8 - 1)), _mm256_cmpgt_epi8(_mm256_set1_epi8(hi as i8 + 1), x))
            };
            // Signed compares: bytes 0x80 and up are negative, never letters or
            // digits. Letters are tested on v | 0x20, digits on the raw byte.
            let letter = between(_mm256_or_si256(v, _mm256_set1_epi8(0x20)), b'a', b'z');
            _mm256_or_si256(letter, between(v, b'0', b'9'))
        })
    };
    let mask = half(at) as u64 | (half(at + 32) as u64) << 32;
    match end.saturating_sub(at) {
        64.. => mask,
        left => mask & ((1 << left) - 1),
    }
}

/// `vpmovmskb` hidden from LLVM: on CPUs without AVX-512 it rewrites the
/// combination of two movemasks into a bit-by-bit extraction that costs a quarter
/// of the runtime.
fn movemask(v: __m256i) -> u32 {
    let mask: u32;
    // SAFETY: AVX2 is enabled for this build; the instruction reads one register.
    unsafe { std::arch::asm!("vpmovmskb {0:e}, {1}", out(reg) mask, in(ymm_reg) v, options(pure, nomem, nostack)) };
    mask
}

/// Splits the corpus into about `parts` ranges of whole lines, each starting at a
/// document whose id is a multiple of 64, so no block straddles two threads.
/// Returns (start, end, first id) per slice.
fn slices(text: &[u8], parts: usize) -> Result<Vec<(usize, usize, u32)>, String> {
    let mut starts: Vec<(usize, u32)> = Vec::new();
    for p in 0..parts {
        let mut at = if p == 0 { 0 } else { line_end(text, text.len() * p / parts) };
        while at < text.len() {
            let tab = text[at..].iter().position(|&b| b == b'\t').ok_or("docs.txt: a line without a TAB")?;
            let id = parse_id(&text[at..at + tab])?;
            if id % 64 == 0 {
                if starts.last().is_none_or(|&(s, _)| s < at) {
                    starts.push((at, id));
                }
                break;
            }
            at = line_end(text, at);
        }
    }
    Ok((starts.iter().enumerate()).map(|(i, &(s, id))| (s, starts.get(i + 1).map_or(text.len(), |n| n.0), id)).collect())
}

/// The offset just after the first LF at or after `at`, or the end of `text`.
fn line_end(text: &[u8], at: usize) -> usize {
    let at = at.min(text.len());
    text[at..].iter().position(|&b| b == b'\n').map_or(text.len(), |i| at + i + 1)
}

/// Ids are dense and ascending, so the count is the last line's id + 1.
fn doc_count(text: &[u8]) -> Result<u32, String> {
    let Some(body) = text.strip_suffix(b"\n") else { return Ok(0) };
    let start = body.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let tab = body[start..].iter().position(|&b| b == b'\t').ok_or("docs.txt: a line without a TAB")?;
    Ok(parse_id(&body[start..start + tab])? + 1)
}

fn parse_id(digits: &[u8]) -> Result<u32, String> {
    if digits.is_empty() || digits.len() > 9 || !digits.iter().all(u8::is_ascii_digit) {
        return Err(format!("docs.txt: bad id {:?}", String::from_utf8_lossy(digits)));
    }
    Ok(digits.iter().fold(0, |acc, &b| acc * 10 + (b - b'0') as u32))
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
