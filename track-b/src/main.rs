//! Track B (Index): answer 50,000 BOOL and PHRASE queries over a 254 MB corpus.
//!
//! There is no index: the corpus is scanned once, in parallel slices, and every
//! query is answered during that scan, so nothing grows with the corpus.
//! 1. The queries come first. Only their terms are looked up, in one shared
//!    table, its bucket fetched in advance: each document's tokens are found
//!    first, then looked up.
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
/// summed over the terms) is evaluated in every superblock: one evaluation then
/// covers four blocks.
const ALWAYS: f64 = 0.25;
/// Documents per superblock: four blocks of 64.
const SUPER: u32 = 256;
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
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let (queries, mut vocab) = parse_queries(queries_txt, threads)?;
    let slices = slices(text, threads)?;
    let nb = vocab.bool_terms as usize;

    // Sample the start of every slice for each term's document frequency.
    let df: Vec<AtomicU32> = (0..vocab.count).map(|_| AtomicU32::new(0)).collect();
    let sampled = par(slices.len(), |i| {
        let (start, end, first) = slices[i];
        let stop = line_end(text, start + SAMPLE_BYTES / slices.len()).min(end);
        Scan::new(docs, &vocab, None, &df, nb, 0).run(start, stop, first).map(|scan| scan.doc + 1 - first)
    });
    let sampled = sampled.into_iter().sum::<Result<u32, String>>()?;
    let df: Vec<u32> = df.into_iter().map(AtomicU32::into_inner).collect();
    let lines = queries.lines.len();
    let plan = Plan::new(queries, &df, sampled, nb, threads);
    vocab.finish(&plan);

    let results = par(slices.len(), |i| {
                    let (start, end, first) = slices[i];
                    let scan = Scan::new(docs, &vocab, Some(&plan), &[], nb, lines).run(start, end, first)?;
                    // Drop the slice's pages now, in parallel, instead of the kernel
                    // unmapping all 62,000 at exit on one core.
                    mmap::release(&docs[start..end]);
                    let Answers { state, mut log, logged } = scan.answers;
                    // Sorted, the log holds each line's first ids in order.
                    log.truncate(logged);
                    log.sort_unstable();
                    Ok((state, log, scan.doc))
    });
    let results: Vec<(Vec<u64>, Vec<u64>, u32)> = results.into_iter().collect::<Result<_, String>>()?;
    let next_firsts = slices.iter().skip(1).map(|s| s.2);
    if slices.first().is_some_and(|s| s.2 != 0) || results.iter().zip(next_firsts).any(|(r, next)| r.2 + 1 != next) {
        return Err("docs.txt: ids must be 0, 1, 2, ... in order".into());
    }
    // Every per-thread sum of ids is below N^2 / 2, which must fit in SUM_BITS.
    if results.last().is_some_and(|r| r.2 >= 1 << 20) {
        return Err("docs.txt: more than 2^20 documents, more than this program supports".into());
    }

    // Format in parallel, one range of lines per thread.
    let parts = slices.len().max(1);
    let chunks = par(parts, |p| format_lines(&results, lines * p / parts..lines * (p + 1) / parts));
    let path = format!("{out_dir}/results.txt");
    let mut file = File::create(&path).map_err(|e| format!("create {path}: {e}"))?;
    chunks.iter().try_for_each(|chunk| file.write_all(chunk)).map_err(|e| format!("write {path}: {e}"))?;
    // Done: exit without freeing the tables and logs, one free at a time.
    std::process::exit(0)
}

/// f(0), f(1), ... f(n - 1), each on its own thread.
fn par<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let f = &f;
    thread::scope(|s| (0..n).map(|i| s.spawn(move || f(i))).collect::<Vec<_>>().into_iter().map(|h| h.join().unwrap()).collect())
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

/// Whole lines of queries.txt, split: per line its kind and its terms' offset
/// in `bools` or `phrases`. A term comes with its table key (zero for a long
/// term) and, in a BOOL query, its op << 30.
#[derive(Default)]
struct Split {
    lines: Vec<(u32, u32, bool)>,
    bools: Vec<(&'static [u8], u64, u32, u32)>,
    phrases: Vec<(&'static [u8], u64, u32, u32)>,
}

/// Splits `chunk`, whole lines of `padded`, the file and mmap::PAD zero bytes.
fn split_lines(chunk: &'static [u8], padded: &'static [u8]) -> Result<Split, String> {
    let mut out = Split::default();
    let bad = |line: &[u8]| format!("queries.txt: bad query {:?}", String::from_utf8_lossy(line));
    let term = |term: &'static [u8], op: u32| {
        if term.is_empty() || term.len() > MAX_TERM || !term.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
            return Err(format!("queries.txt: bad term {:?}", String::from_utf8_lossy(term)));
        }
        let (lo, hi) = if term.len() > KEY { (0, 0) } else { parts(key_at(padded, term.as_ptr() as usize - padded.as_ptr() as usize, term.len())) };
        Ok((term, lo, hi, op))
    };
    let body = chunk.strip_suffix(b"\n").unwrap_or(chunk);
    for line in body.split(|&b| b == b'\n').filter(|_| !chunk.is_empty()) {
        if let Some(rest) = line.strip_prefix(b"BOOL ") {
            let start = out.bools.len();
            let mut words = rest.split(|&b| b == b' ');
            let mut op = OR;
            loop {
                out.bools.push(term(words.next().ok_or_else(|| bad(line))?, op << 30)?);
                let Some(word) = words.next() else { break };
                op = match word {
                    b"AND" => AND,
                    b"OR" => OR,
                    b"NOT" => NOT,
                    _ => return Err(bad(line)),
                };
            }
            if out.bools.len() - start > MAX_BOOL_TERMS {
                return Err(format!("queries.txt: more than {MAX_BOOL_TERMS} terms in {:?}", String::from_utf8_lossy(line)));
            }
            out.lines.push((start as u32, (out.bools.len() - start) as u32, false));
        } else {
            let inner = line.strip_prefix(b"PHRASE \"").and_then(|r| r.strip_suffix(b"\"")).ok_or_else(|| bad(line))?;
            let start = out.phrases.len();
            for word in inner.split(|&b| b == b' ') {
                out.phrases.push(term(word, 0)?);
            }
            out.lines.push((start as u32, (out.phrases.len() - start) as u32, true));
        }
    }
    Ok(out)
}

/// Splits the lines in parallel, then gives the terms ids, all BOOL terms
/// first, so the terms of BOOL queries get the ids 0..bool_terms and can index
/// a dense per-thread array. `padded` is the file and mmap::PAD zero bytes.
fn parse_queries(padded: &'static [u8], parts: usize) -> Result<(Queries, Vocab), String> {
    let text = &padded[..padded.len() - mmap::PAD];
    let cuts: Vec<usize> = (0..=parts).map(|p| if p == 0 { 0 } else { line_end(text, text.len() * p / parts) }).collect();
    let splits = par(parts, |p| split_lines(&text[cuts[p]..cuts[p + 1]], padded)).into_iter().collect::<Result<Vec<_>, String>>()?;
    let bool_terms: usize = splits.iter().map(|s| s.bools.len()).sum();
    let mut q = Queries { lines: Vec::new(), terms: Vec::new() };
    let (mut bool_at, mut phrase_at) = (0, bool_terms as u32);
    for split in &splits {
        let at = |&(start, len, phrase): &(u32, u32, bool)| (start + if phrase { phrase_at } else { bool_at }, len, phrase);
        q.lines.extend(split.lines.iter().map(at));
        (bool_at, phrase_at) = (bool_at + split.bools.len() as u32, phrase_at + split.phrases.len() as u32);
    }
    let mut vocab = Vocab::new(q.lines.len());
    // Each term's bucket is fetched 16 terms in advance: the table is in L3.
    let terms = || splits.iter().flat_map(|s| &s.bools).chain(splits.iter().flat_map(|s| &s.phrases));
    let mut ahead = terms().skip(16);
    for (k, &(term, lo, hi, op)) in terms().enumerate() {
        if let Some(&(_, lo, hi, _)) = ahead.next() {
            // SAFETY: a prefetch of a bucket of the table.
            unsafe { _mm_prefetch::<_MM_HINT_T0>(vocab.table.buckets.as_ptr().add((hash(lo, hi) >> vocab.table.shift) as usize) as *const i8) };
        }
        if k == bool_terms {
            vocab.bool_terms = vocab.count;
        }
        q.terms.push(vocab.intern(term, lo, hi) | op);
    }
    if q.terms.len() == bool_terms {
        vocab.bool_terms = vocab.count;
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
    fn probe(&self, b: usize, wide: __m256i) -> (u32, u64) {
        // SAFETY: callers pass b < buckets.len().
        let slots = unsafe { &self.buckets.get_unchecked(b).0 };
        // SAFETY: two 32-byte loads that cover the 64-byte bucket; AVX2 is
        // enabled for the whole build.
        let half = |i| unsafe { _mm256_cmpeq_epi8(_mm256_loadu_si256((slots.as_ptr() as *const __m256i).add(i)), wide) };
        let m = movemask(half(0)) as u64 | (movemask(half(1)) as u64) << 32;
        // Slot j matches when its 12 key bytes do: bits 16j..16j + 12 of m.
        // Adding 1 to such a run of ones carries into bit 16j + 12.
        let hit = ((m & 0x0FFF_0FFF_0FFF_0FFF) + 0x0001_0001_0001_0001) & 0x1000_1000_1000_1000;
        let info = slots[hit.trailing_zeros() as usize / 16 & 3].info;
        // A miss with a full bucket also returns its last key, else 0.
        (if_zero(hit, MISS as u64, info as u64) as u32, if_zero(hit, slots[3].lo, 0))
    }

    /// The info of the key whose home bucket is b, or MISS. A full bucket continues
    /// into the next one, as `insert` does.
    fn get(&self, key: __m128i, mut b: usize) -> u32 {
        // SAFETY: AVX2 is enabled for the whole build.
        let wide = unsafe { _mm256_broadcastsi128_si256(key) };
        loop {
            // Slots fill in order and are never removed: a free last slot ends the
            // probe. One branch, rarely taken: a miss in a full bucket.
            let (info, full_miss) = self.probe(b, wide);
            if full_miss != 0 {
                b = (b + 1) & (self.buckets.len() - 1);
                continue;
            }
            return info;
        }
    }

    /// Adds a key that is not in the table yet.
    fn insert(&mut self, lo: u64, hi: u32, info: u32) {
        let mask = self.buckets.len() - 1;
        let mut b = (hash(lo, hi) >> self.shift) as usize;
        loop {
            if let Some(slot) = self.buckets[b].0.iter_mut().find(|s| s.lo == 0) {
                *slot = Slot { lo, hi, info };
                self.len += 1;
                return;
            }
            b = (b + 1) & mask;
        }
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
        let lower = _mm_or_si128(_mm_loadu_si128(bytes.as_ptr().add(at) as *const __m128i), _mm_set1_epi8(0x20));
        _mm_and_si128(lower, _mm_loadu_si128(KEEP.as_ptr().add(16 - len) as *const __m128i))
    }
}

/// CRC32 of the key, in the top bits (the tables index by those).
fn hash(lo: u64, hi: u32) -> u64 {
    // SAFETY: SSE4.2 is enabled for the whole build (it comes with AVX2).
    (unsafe { _mm_crc32_u64(hi as u64, lo) }) << 32
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
    /// All terms of up to 12 bytes, kept under 2.5 per bucket so that most
    /// buckets have a free slot: a word no query uses is then turned away by one
    /// probe.
    table: Table,
    /// Terms of 13 to 32 bytes: about 0.2% of the corpus tokens are that long.
    long: HashMap<&'static [u8], u32>,
    count: u32,
    bool_terms: u32,
}

impl Vocab {
    fn new(lines: usize) -> Self {
        // Sized for about 1.5 distinct terms per query, about 2.3 per bucket (2 MB
        // for 50,000 queries): twice that is 1% faster but 2 MB more memory. The
        // table still grows if there are more terms.
        let table = Table::new((lines * 3 / 5).next_power_of_two().max(1024).trailing_zeros());
        Vocab { table, long: HashMap::new(), count: 0, bool_terms: 0 }
    }

    /// The id of `term`, whose table key is (lo, hi).
    fn intern(&mut self, term: &'static [u8], lo: u64, hi: u32) -> u32 {
        let id = if term.len() > KEY {
            *self.long.entry(term).or_insert(self.count)
        } else {
            // SAFETY: SSE4.1 is enabled for the whole build (it comes with AVX2).
            let key = unsafe { _mm_insert_epi32::<2>(_mm_cvtsi64_si128(lo as i64), hi as i32) };
            match self.table.get(key, (hash(lo, hi) >> self.table.shift) as usize) {
                MISS => {
                    if (self.table.len + 1) * 2 > self.table.buckets.len() * 5 {
                        let bigger = Table::new(65 - self.table.shift);
                        let old = std::mem::replace(&mut self.table, bigger);
                        for slot in old.buckets.iter().flat_map(|b| b.0).filter(|s| s.lo != 0) {
                            self.table.insert(slot.lo, slot.hi, slot.info);
                        }
                    }
                    self.table.insert(lo, hi, self.count);
                    self.count
                }
                id => id,
            }
        };
        self.count += (id == self.count) as u32;
        id
    }

    /// Renumbers the terms as the plan does and flags the anchors.
    fn finish(&mut self, plan: &Plan) {
        let renumber = |old: u32| {
            let id = plan.rank[old as usize];
            id | (plan.anchor[id as usize] != plan.anchor[id as usize + 1]) as u32 * ANCHOR
        };
        let slots = self.table.buckets.iter_mut().flat_map(|b| &mut b.0).filter(|s| s.lo != 0);
        slots.for_each(|s| s.info = renumber(s.info));
        self.long.values_mut().for_each(|info| *info = renumber(*info));
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
    /// BOOL entries evaluated in every superblock, on rows of `Scan::wide`.
    always: Vec<Entry>,
    /// The terms of `always` have the ids 0..rows, which are also their rows.
    rows: usize,
    /// Phrases anchored on term t: `anchored[anchor[t]..anchor[t + 1]]`.
    anchor: Vec<u32>,
    anchored: Vec<Anchored>,
    /// The queries' term words, with the new ids.
    terms: Vec<u32>,
    /// The new id of each term.
    rank: Vec<u32>,
}

impl Plan {
    /// The per-line work runs on `parts` threads, a range of lines each.
    fn new(q: Queries, sample_df: &[u32], sampled: u32, nb: usize, parts: usize) -> Self {
        let range = |p: usize| q.lines.len() * p / parts..q.lines.len() * (p + 1) / parts;
        // The expected share of 64-document blocks that contain t: the cost of
        // triggering on it.
        let cost = |t: u32| 1.0 - (1.0 - sample_df[t as usize] as f64 / sampled.max(1) as f64).powi(64);
        let line_terms = |&(start, len, _): &(u32, u32, bool)| &q.terms[start as usize..(start + len) as usize];
        // Whether each line is a BOOL query evaluated in every superblock.
        let always_line = |line: &(u32, u32, bool)| {
            let (cover, n) = if line.2 { return false } else { cover(line_terms(line), cost) };
            cover[..n].iter().map(|&t| cost(t)).sum::<f64>() > ALWAYS
        };
        let always_lines = par(parts, |p| q.lines[range(p)].iter().map(always_line).collect::<Vec<_>>()).concat();
        let mut wide = vec![false; nb];
        for (line, _) in q.lines.iter().zip(&always_lines).filter(|l| *l.1) {
            line_terms(line).iter().for_each(|&word| wide[(word & TERM) as usize] = true);
        }
        let rows = wide.iter().filter(|&&w| w).count();
        // Renumber the BOOL terms, those of the `always` lines first, each part by
        // sampled frequency, most frequent first, so the presence words and
        // entries the scan uses most share cache lines.
        let mut rank: Vec<u32> = (0..sample_df.len() as u32).collect();
        let mut order: Vec<u32> = (0..nb as u32).collect();
        order.sort_unstable_by_key(|&t| (!wide[t as usize], std::cmp::Reverse(sample_df[t as usize])));
        let mut df = sample_df.to_vec();
        for (id, &old) in order.iter().enumerate() {
            rank[old as usize] = id as u32;
            df[id] = sample_df[old as usize];
        }
        let mut terms = q.terms;
        terms.iter_mut().for_each(|word| *word = rank[(*word & TERM) as usize] | *word & !TERM);
        // The costs by new id, so `cover` finds the sets `always_line` found.
        let cost = |t: u32| cost(order[t as usize]);
        let lines = |p: usize| {
          let (mut by_trigger, mut by_anchor, mut always) = (Vec::new(), Vec::new(), Vec::new());
          for line in range(p) {
            let ((start, len, phrase), is_always) = (q.lines[line], always_lines[line]);
            let words = &terms[start as usize..(start + len) as usize];
            let answer = line as u32; // the index of the line's answer in each thread's state
            if phrase {
                let (offset, &anchor) = words.iter().enumerate().min_by_key(|&(_, &t)| df[t as usize]).unwrap();
                by_anchor.push((anchor, Anchored { answer, offset: offset as u32, start, len }));
                continue;
            }
            // Padded with AND of the all-ones slot (or row).
            let pad = if is_always { rows + 1 } else { nb + 1 };
            let mut entry = Entry { terms: [pad as u32; MAX_BOOL_TERMS], answer, earlier: 0 };
            entry.terms[..words.len()].copy_from_slice(words);
            if is_always {
                always.push(entry);
                continue;
            }
            let (cover, n) = cover(words, cost);
            let cover = &cover[..n];
            for (i, &c) in cover.iter().enumerate() {
                let earlier = (words.iter().enumerate()).filter(|(_, w)| cover[..i].contains(&(*w & TERM))).fold(0, |m, (k, _)| m | 1 << k);
                by_trigger.push((c, Entry { earlier, ..entry }));
            }
          }
          (by_trigger, by_anchor, always)
        };
        let (mut by_trigger, mut by_anchor, mut always) = (Vec::new(), Vec::new(), Vec::new());
        for (t, a, e) in par(parts, lines) {
            (by_trigger.extend(t), by_anchor.extend(a), always.extend(e));
        }
        let (group, entries) = csr(by_trigger, nb + 1);
        let (anchor, anchored) = csr(by_anchor, df.len());
        Plan { group, entries, always, rows, anchor, anchored, terms, rank }
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
    /// Row r < plan.rows: the words of BOOL term r in the 4 blocks of this
    /// superblock. Row `rows` takes the other terms; row `rows + 1` is all ones.
    wide: Vec<Row>,
    /// This document's tokens to look up: their keys and home buckets (or a zero key
    /// and the info of a long token).
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
    fn new(docs: &'static [u8], vocab: &'a Vocab, plan: Option<&'a Plan>, df: &'a [AtomicU32], nb: usize, lines: usize) -> Self {
        let rows = plan.map_or(0, |p| p.rows);
        Scan {
            docs,
            vocab,
            plan,
            df,
            nb,
            w: (0..nb + 2).map(|slot| if slot == nb + 1 { !0 } else { 0 }).collect(),
            touched: vec![0; nb + 2],
            n: 0,
            wide: (0..rows + 2).map(|row| Row([if row == rows + 1 { !0 } else { 0 }; 4])).collect(),
            pending: Vec::new(),
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
        // `line`: bit 0 is set when the next chunk starts a line, as the slice does.
        let (mut bit, mut carry, mut line) = (0u64, 0u64, 1u64);
        let mut mask = token_mask(docs, start, end);
        for at in (start..end).step_by(64) {
            let next = token_mask(docs, at + 64, end);
            let mut starts = mask & !(mask << 1 | carry);
            carry = mask >> 63;
            // The id tokens: those that start a line.
            let lf = byte_mask(docs, at, b'\n');
            let mut ids = starts & (lf << 1 | line);
            line = lf >> 63;
            // A token is at most 32 bytes (longer ones match no term), so it
            // ends within this chunk and the next. `shifted(b)` is the window
            // from bit b on: `next << (64 - b)` in two steps, valid for b = 0.
            let (next1, mask0) = (next << 1, mask);
            let shifted = |b: u32| mask0 >> b | next1 << (63 - b);
            // A chunk has at most 32 tokens.
            self.pending.reserve(32);
            loop {
                // The tokens before the next id token, if any.
                let mut text = starts & (ids & ids.wrapping_neg()).wrapping_sub(1);
                starts ^= text;
                let (buf, mut n) = (self.pending.as_mut_ptr(), self.pending.len());
                let (buckets, shift) = (self.vocab.table.buckets.as_ptr(), self.vocab.table.shift);
                while text != 0 {
                    let b = text.trailing_zeros();
                    text &= text - 1;
                    // Only 64 bits: a token that runs past them is too long to be
                    // a term anyway.
                    let len = (!shifted(b)).trailing_zeros() as usize;
                    let p = at + b as usize;
                    let token = if len <= KEY {
                        let key = key_at(docs, p, len);
                        let (lo, hi) = parts(key);
                        let h = hash(lo, hi);
                        // SAFETY: a prefetch of a bucket of the table.
                        let b = (h >> shift) as usize;
                        unsafe { _mm_prefetch::<_MM_HINT_T0>(buckets.add(b) as *const i8) };
                        (key, b as u64)
                    } else {
                        // SAFETY: AVX2 is enabled for the whole build.
                        (unsafe { _mm_setzero_si128() }, self.vocab.get_long(&docs[p..p + len]) as u64)
                    };
                    // SAFETY: `reserve(32)` above made room for this chunk's tokens.
                    unsafe { buf.add(n).write(token) };
                    n += 1;
                }
                // SAFETY: the first n entries were written.
                unsafe { self.pending.set_len(n) };
                if ids == 0 {
                    break;
                }
                ids &= ids - 1;
                let b = starts.trailing_zeros();
                starts &= starts - 1;
                let len = (!shifted(b)).trailing_zeros() as usize;
                let p = at + b as usize;
                if docs[p + len] != b'\t' {
                    return Err("docs.txt: a line without an id and a TAB".into());
                }
                self.look_up(bit);
                bit = self.next_doc(&docs[p..p + len])?;
            }
            mask = next;
        }
        self.look_up(bit);
        self.end_doc();
        self.end_block(self.doc & !63);
        self.end_super(self.doc / SUPER * SUPER);
        Ok(self)
    }

    /// Looks up the tokens of this document, whose bit is `bit`, and sets their
    /// words. Each step runs over the whole document, so that the memory accesses
    /// of one step overlap: the buckets were fetched when the tokens were
    /// found, and the words are fetched while the tokens are probed.
    fn look_up(&mut self, bit: u64) {
        let (nb, table) = (self.nb, &self.vocab.table);
        self.toks.reserve(self.pending.len());
        let (toks, mut len) = (self.toks.as_mut_ptr(), self.toks.len());
        for &(key, b) in &self.pending {
            // SAFETY: SSE4.1 is enabled for the whole build (it comes with AVX2).
            let info = if unsafe { _mm_testz_si128(key, key) } == 1 { b as u32 } else { table.get(key, b as usize) };
            let id = info & MISS;
            // SAFETY: a prefetch of a word of `w`.
            unsafe { _mm_prefetch::<_MM_HINT_T0>(self.w.as_ptr().add((id as usize).min(nb)) as *const i8) };
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
            // Branch-free: a slot seen for the first time in this block is
            // kept in `touched`.
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
        // At the first id of a slice, this may evaluate an empty superblock:
        // harmless.
        if id % SUPER == 0 && id > 0 {
            self.end_super(id - SUPER);
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

    /// Evaluates the queries triggered by the terms of this block, the 64
    /// documents from `base`, then moves its words to `wide` and clears it. The
    /// evaluation has no branch: whether a query matches is too random to predict.
    fn end_block(&mut self, base: u32) {
        let w = &mut self.w;
        if let Some(plan) = self.plan {
            for t in self.touched[..self.n].iter().map(|&t| t as usize) {
                for e in &plan.entries[plan.group[t] as usize..plan.group[t + 1] as usize] {
                    let bit = |b: u32| 0u64.wrapping_sub((b & 1) as u64);
                    let r = e.terms.iter().fold(0u64, |r, &word| {
                        // SAFETY: the plan's slots are below w.len().
                        let x = unsafe { *w.get_unchecked((word & TERM) as usize) };
                        // AND: r & x. NOT: r & !x. OR: (r & !x) | x = r | x.
                        (r & (x ^ bit(word >> 30))) | (x & bit(word >> 31))
                    });
                    // A branch on the few entries with earlier cover terms.
                    let seen = e.earlier != 0 && (0..MAX_BOOL_TERMS).any(|k| w[(e.terms[k] & TERM) as usize] & bit(e.earlier >> k) != 0);
                    self.answers.add(e.answer, if seen { 0 } else { r }, base);
                }
            }
        }
        let (rows, block) = (self.wide.len() - 2, (base / 64 % (SUPER / 64)) as usize);
        for &t in &self.touched[..self.n] {
            self.wide[(t as usize).min(rows)].0[block] = w[t as usize];
            w[t as usize] = 0;
        }
        self.n = 0;
    }

    /// Evaluates the `always` queries on the superblock of 256 documents from
    /// `base`, like `end_block` but on four words at once, then clears it.
    fn end_super(&mut self, base: u32) {
        let Some(plan) = self.plan else { return };
        let wide = &self.wide;
        for e in &plan.always {
            // SAFETY: aligned 32-byte loads of rows; AVX2 is enabled for the whole build.
            let hits = unsafe {
                e.terms.iter().fold(_mm256_setzero_si256(), |r, &word| {
                    let x = _mm256_load_si256(wide[(word & TERM) as usize].0.as_ptr() as *const __m256i);
                    // The op bits spread over whole masks: bit 30 (NOT, OR), bit 31 (OR).
                    let op = _mm256_set1_epi32(word as i32);
                    let (not, or) = (_mm256_srai_epi32::<31>(_mm256_slli_epi32::<1>(op)), _mm256_srai_epi32::<31>(op));
                    _mm256_or_si256(_mm256_and_si256(r, _mm256_xor_si256(x, not)), _mm256_and_si256(x, or))
                })
            };
            self.answers.add_wide(e.answer, hits, base);
        }
        self.wide[..plan.rows].fill(Row([0; 4]));
    }
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
    fn new(answers: usize) -> Self {
        // Written now: a lazily zeroed page that is read first faults again
        // when written, and that fault flushes the TLB of every thread.
        let mut state = Vec::with_capacity(answers);
        state.resize(answers, std::hint::black_box(0));
        Answers { state, log: vec![0; answers * 10 + 1], logged: 0 }
    }

    /// Adds the documents `base + i` for each bit i of `hits`, which may be 0.
    fn add(&mut self, answer: u32, hits: u64, base: u32) {
        // SAFETY: answers index the state; an answer logs at most 10 ids, so
        // logged <= 10 * answers < log.len().
        let (s, log) = unsafe { (&mut *self.state.as_mut_ptr().add(answer as usize), self.log.as_mut_ptr()) };
        let count = *s >> SUM_BITS;
        let entry = |bits: u64| (answer as u64) << 32 | (base + bits.trailing_zeros()) as u64;
        // The first id without a branch: always written, kept only if wanted.
        // SAFETY: as above.
        unsafe { *log.add(self.logged) = entry(hits) };
        self.logged += (count < 10 && hits != 0) as usize;
        let mut rest = hits & hits.wrapping_sub(1);
        for _ in count + 1..10 {
            if rest == 0 {
                break;
            }
            // SAFETY: as above.
            unsafe { *log.add(self.logged) = entry(rest) };
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

    /// `add` for the 256 documents from `base`, one word of `hits` per 64.
    fn add_wide(&mut self, answer: u32, hits: __m256i, base: u32) {
        if self.state[answer as usize] >> SUM_BITS < 10 {
            // SAFETY: AVX2 is enabled for the whole build.
            if unsafe { _mm256_testz_si256(hits, hits) } == 1 {
                return;
            }
            // Still logging first ids: a word at a time.
            // SAFETY: __m256i and [u64; 4] have the same size, and any bits are valid.
            let words: [u64; 4] = unsafe { std::mem::transmute(hits) };
            (0..4).for_each(|i| self.add(answer, words[i], base + 64 * i as u32));
            return;
        }
        let (n, positions) = count_positions(hits);
        self.state[answer as usize] += n << SUM_BITS | (n * base as u64 + positions);
    }
}

/// The number of set bits in `v`, and the sum of their positions, from tables of
/// the 4-bit nibbles: the nibble at bits 4m..4m + 4 adds 4m per set bit, plus its
/// own bit positions.
fn count_positions(v: __m256i) -> (u64, u64) {
    // SAFETY: AVX2 is enabled for the whole build.
    unsafe {
        let table = |t: [i8; 16]| _mm256_broadcastsi128_si256(_mm_loadu_si128(t.as_ptr() as *const __m128i));
        let popcount = table([0, 1, 1, 2, 1, 2, 2, 3, 1, 2, 2, 3, 2, 3, 3, 4]);
        // The bit positions of a low nibble, and of a high one (4 more per bit).
        let low = table([0, 0, 1, 1, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6]);
        let high = table([0, 4, 5, 9, 6, 10, 11, 15, 7, 11, 12, 16, 13, 17, 18, 22]);
        let nibble = _mm256_set1_epi8(0x0F);
        let (lo, hi) = (_mm256_and_si256(v, nibble), _mm256_and_si256(_mm256_srli_epi16::<4>(v), nibble));
        let count = _mm256_add_epi8(_mm256_shuffle_epi8(popcount, lo), _mm256_shuffle_epi8(popcount, hi));
        let own = _mm256_add_epi8(_mm256_shuffle_epi8(low, lo), _mm256_shuffle_epi8(high, hi));
        // Byte b adds 8b per set bit: 16-bit sums of byte pairs, then 32-bit sums,
        // with the count added at bit 16 (the position sum is below 2^15).
        let eight_b = _mm256_setr_epi8(0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120, -128, -120, -112, -104, -96, -88, -80, -72, -64, -56, -48, -40, -32, -24, -16, -8);
        let (ones8, ones16) = (_mm256_set1_epi8(1), _mm256_set1_epi16(1));
        let pos = _mm256_add_epi16(_mm256_maddubs_epi16(eight_b, count), _mm256_maddubs_epi16(own, ones8));
        let sums = _mm256_add_epi32(_mm256_madd_epi16(pos, ones16), _mm256_slli_epi32::<16>(_mm256_madd_epi16(_mm256_maddubs_epi16(count, ones8), ones16)));
        let s = _mm_add_epi32(_mm256_castsi256_si128(sums), _mm256_extracti128_si256::<1>(sums));
        let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b01_00_11_10>(s));
        let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b10_11_00_01>(s));
        let total = _mm_cvtsi128_si32(s) as u64;
        (total >> 16, total & 0xFFFF)
    }
}

/// Bit i is set when byte `at + i` is `byte`.
fn byte_mask(docs: &[u8], at: usize, byte: u8) -> u64 {
    // SAFETY: unaligned 32-byte loads from 32-byte slices; AVX2 is enabled for the whole build.
    let half = |at: usize| movemask(unsafe { _mm256_cmpeq_epi8(_mm256_loadu_si256(docs[at..at + 32].as_ptr() as *const __m256i), _mm256_set1_epi8(byte as i8)) });
    half(at) as u64 | (half(at + 32) as u64) << 32
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

/// `if c == 0 { a } else { b }` as a cmov: LLVM turns such selects of loaded
/// values into branches, which mispredict when c is random.
fn if_zero(c: u64, a: u64, b: u64) -> u64 {
    let mut r = b;
    // SAFETY: register-only instructions.
    unsafe { std::arch::asm!("test {c}, {c}", "cmovz {r}, {a}", c = in(reg) c, a = in(reg) a, r = inout(reg) r, options(pure, nomem, nostack)) };
    r
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

/// Splits the corpus into about `parts` ranges of whole lines. A block or
/// superblock may straddle two slices: each thread sees the other's documents
/// as containing no term, so they match nothing there.
/// Returns (start, end, first id) per slice.
fn slices(text: &[u8], parts: usize) -> Result<Vec<(usize, usize, u32)>, String> {
    let mut starts: Vec<(usize, u32)> = Vec::new();
    for p in 0..parts {
        let at = if p == 0 { 0 } else { line_end(text, text.len() * p / parts) };
        if at < text.len() && starts.last().is_none_or(|&(s, _)| s < at) {
            let tab = text[at..].iter().position(|&b| b == b'\t').ok_or("docs.txt: a line without a TAB")?;
            starts.push((at, parse_id(&text[at..at + tab])?));
        }
    }
    Ok((starts.iter().enumerate()).map(|(i, &(s, id))| (s, starts.get(i + 1).map_or(text.len(), |n| n.0), id)).collect())
}

/// The offset just after the first LF at or after `at`, or the end of `text`.
fn line_end(text: &[u8], at: usize) -> usize {
    let at = at.min(text.len());
    text[at..].iter().position(|&b| b == b'\n').map_or(text.len(), |i| at + i + 1)
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
