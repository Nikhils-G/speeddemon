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

use std::arch::asm;
use std::arch::x86_64::*;
use std::cmp::Reverse;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

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
    mmap::no_huge_pages();
    let open = |name: &str| mmap::map(&format!("{data_dir}/{name}"));
    let docs = open("docs.txt");
    let text = &docs[..docs.len() - mmap::PAD];
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let (queries, mut vocab) = parse_queries(open("queries.txt"), threads);
    let slices = slices(text, threads);

    // Sample the start of every slice for each term's document frequency.
    let df: Vec<AtomicU32> = (0..vocab.count).map(|_| AtomicU32::new(0)).collect();
    let sampled = par(slices.len(), |i| {
        let (start, end, first) = slices[i];
        let stop = line_after(text, start + SAMPLE_BYTES / slices.len()).min(end);
        let scan = Scan::new(&vocab, None, &df).run(docs, start, stop, first);
        scan.doc + 1 - first
    });
    let df: Vec<u32> = df.into_iter().map(AtomicU32::into_inner).collect();
    let plan = Plan::new(queries, &df, sampled.iter().sum(), threads);
    vocab.finish(&plan);

    let results = par(slices.len(), |i| {
        let (start, end, first) = slices[i];
        let scan = Scan::new(&vocab, Some(&plan), &[]).run(docs, start, end, first);
        // Drop the slice's pages now, in parallel, instead of the kernel
        // unmapping all 62,000 at exit on one core.
        mmap::release(&docs[start..end]);
        let mut log = scan.answers.log;
        // Back to line numbers; sorted, the log holds each line's first ids in
        // order.
        log.truncate(scan.answers.logged);
        for e in &mut log {
            *e = (plan.line_of[(*e >> 32) as usize] as u64) << 32 | *e & u32::MAX as u64;
        }
        log.sort_unstable();
        // The ids go on in the next slice. Every per-thread sum of ids is below
        // N^2 / 2, which must fit in SUM_BITS.
        let next = slices.get(i + 1).map_or(scan.doc + 1, |s| s.2);
        let ok = slices[0].2 == 0 && next == scan.doc + 1 && scan.doc < 1 << 20;
        assert!(ok, "docs.txt: ids must be 0, 1, 2, ... below 2^20");
        (scan.answers.state, log)
    });

    // Format in parallel, one range of lines per thread.
    let (lines, parts) = (plan.lines, slices.len().max(1));
    let chunks = par(parts, |p| format_lines(&results, &plan.answer_of, part(lines, p, parts)));
    let path = format!("{out_dir}/results.txt");
    let mut file = std::fs::File::create(&path).expect(&path);
    for chunk in chunks {
        std::io::Write::write_all(&mut file, &chunk).expect(&path);
    }
    // Done: exit without freeing the tables and logs, one free at a time.
    std::process::exit(0)
}

/// f(0), f(1), ... f(n - 1), each on its own thread.
fn par<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let f = &f;
    std::thread::scope(|s| {
        let threads: Vec<_> = (0..n).map(|i| s.spawn(move || f(i))).collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    })
}

/// The p-th of `parts` near-equal ranges of 0..len.
fn part(len: usize, p: usize, parts: usize) -> std::ops::Range<usize> {
    len * p / parts..len * (p + 1) / parts
}

/// The output for `lines`: per line, the threads' counts and sums added up (from
/// the state of the line's answer), and the first ten ids taken from the threads
/// in slice order.
fn format_lines(results: &[(Vec<u64>, Vec<u64>)], answer_of: &[u32], lines: std::ops::Range<usize>) -> Vec<u8> {
    // Per line: the count, the sum, then the first ids.
    let (mut out, mut fields) = (Vec::with_capacity(lines.len() * 32), Vec::new());
    let first = |log: &Vec<u64>| log.partition_point(|&e| e >> 32 < lines.start as u64);
    let mut cursors: Vec<usize> = results.iter().map(|r| first(&r.1)).collect();
    for line in lines {
        fields.clear();
        fields.extend([0, 0]);
        for ((state, log), cursor) in results.iter().zip(&mut cursors) {
            let s = state[answer_of[line] as usize];
            fields[0] += s >> SUM_BITS;
            fields[1] += s & ((1 << SUM_BITS) - 1);
            while let Some(&entry) = log.get(*cursor).filter(|&&e| e >> 32 == line as u64) {
                fields.push(entry & u32::MAX as u64);
                *cursor += 1;
            }
        }
        for &value in fields.iter().take(12) {
            push_decimal(&mut out, value);
            out.push(b' ');
        }
        *out.last_mut().unwrap() = b'\n';
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

/// Splits `chunk`, whole lines of `padded` (the file and mmap::PAD zero bytes),
/// into its lines, as in `Queries` but from 0, and their terms: each with its
/// table key (zero for a long term) and, in a BOOL query, its op << 30.
fn split_lines(chunk: &'static [u8], padded: &'static [u8]) -> Split {
    let (mut lines, mut terms) = (Vec::new(), Vec::new());
    for line in chunk.split_inclusive(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let bad = || -> ! { panic!("queries.txt: bad query {:?}", String::from_utf8_lossy(line)) };
        let start = terms.len();
        let mut term = |term: &'static [u8], op: u32| {
            let ok = term.iter().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9'));
            if !ok || !(1..=MAX_TERM).contains(&term.len()) {
                bad();
            }
            let at = term.as_ptr() as usize - padded.as_ptr() as usize;
            // Length 0 gives the zero key, which marks a long term.
            let key = key_at(padded, at, if term.len() > KEY { 0 } else { term.len() });
            terms.push((term, key, op << 30));
        };
        if let Some(rest) = line.strip_prefix(b"BOOL ") {
            let mut words = rest.split(|&b| b == b' ');
            let mut op = OR;
            loop {
                term(words.next().unwrap_or_else(|| bad()), op);
                let Some(word) = words.next() else { break };
                op = match word {
                    b"AND" => AND,
                    b"OR" => OR,
                    b"NOT" => NOT,
                    _ => bad(),
                };
            }
        } else {
            let inner = line.strip_prefix(b"PHRASE \"").unwrap_or_else(|| bad());
            let inner = inner.strip_suffix(b"\"").unwrap_or_else(|| bad());
            for word in inner.split(|&b| b == b' ') {
                term(word, 0);
            }
        }
        let (len, phrase) = (terms.len() - start, !line.starts_with(b"BOOL "));
        if !phrase && len > MAX_BOOL_TERMS {
            bad();
        }
        lines.push((start as u32, len as u32, phrase));
    }
    (lines, terms)
}

type Split = (Vec<(u32, u32, bool)>, Vec<(&'static [u8], __m128i, u32)>);

/// Splits the lines in parallel, then gives the terms ids in order. `padded` is
/// the file and mmap::PAD zero bytes.
fn parse_queries(padded: &'static [u8], threads: usize) -> (Queries, Vocab) {
    let text = &padded[..padded.len() - mmap::PAD];
    let cut = |p: usize| cut(text, p, threads);
    let splits = par(threads, |p| split_lines(&text[cut(p)..cut(p + 1)], padded));
    // The term table is sized for about 1.5 distinct terms per query, about 2.3
    // per bucket (2 MB for 50,000 queries): twice that is 1% faster but 2 MB more
    // memory. It still grows if there are more terms.
    let lines: usize = splits.iter().map(|s| s.0.len()).sum();
    let mut vocab = Vocab::default();
    vocab.rebuild((lines * 3 / 5).max(1024).next_power_of_two());
    let (mut lines, mut terms) = (Vec::new(), Vec::new());
    // Each term's bucket is fetched 16 terms in advance: the table is in L3.
    let mut ahead = splits.iter().flat_map(|s| &s.1).skip(16);
    for (chunk_lines, chunk_terms) in &splits {
        let base = terms.len() as u32;
        for &(start, len, phrase) in chunk_lines {
            lines.push((base + start, len, phrase));
        }
        for &(term, key, op) in chunk_terms {
            if let Some(&(_, key, _)) = ahead.next() {
                // SAFETY: a prefetch of a bucket of the table (a 32-bit CRC, so
                // hash >> shift < buckets.len()); a prefetch never faults anyway.
                let bucket = vocab.buckets.as_ptr().wrapping_add(vocab.home(parts(key)));
                unsafe { _mm_prefetch::<_MM_HINT_T0>(bucket as *const i8) };
            }
            terms.push(vocab.intern(term, key) | op);
        }
    }
    (Queries { lines, terms }, vocab)
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

/// The info of `key` in the buckets from b on, or MISS: a full bucket continues
/// into the next one, as `Vocab::insert` does. A raw pointer and mask, so that
/// a loop that calls it keeps them in registers.
#[inline(always)]
fn find(buckets: *const Bucket, mask: usize, mut b: usize, key: __m128i) -> u32 {
    // SAFETY: AVX2 is enabled for the whole build.
    let wide = unsafe { _mm256_broadcastsi128_si256(key) };
    loop {
        // SAFETY: b <= mask, and the table has mask + 1 buckets.
        let slots = unsafe { &(*buckets.add(b)).0 };
        // SAFETY: two 32-byte loads that cover the 64-byte bucket.
        let ptr = slots.as_ptr() as *const __m256i;
        let half = |i| unsafe { _mm256_cmpeq_epi32(_mm256_loadu_si256(ptr.add(i)), wide) };
        let m = movemask_ps(half(0)) | movemask_ps(half(1)) << 8;
        // Slot j matches when its 3 key dwords do: bits 4j..4j + 3 of m.
        // Adding 1 to such a run of ones carries into bit 4j + 3.
        let hit = (((m & 0x7777) + 0x1111) & 0x8888) as u64;
        let info = slots[hit.trailing_zeros() as usize / 4 & 3].info;
        // Without a branch (whether and where a key hits is random), except on
        // a miss in a full bucket: slots fill in order and are never removed,
        // so a free last slot ends the probe.
        if if_zero(hit, slots[3].lo, 0) != 0 {
            b = (b + 1) & mask;
            continue;
        }
        return if_zero(hit, MISS as u64, info as u64) as u32;
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

/// CRC32 of the key: the table indexes by its top bits.
fn hash(lo: u64, hi: u32) -> u64 {
    // SAFETY: SSE4.2 is enabled for the whole build (it comes with AVX2).
    unsafe { _mm_crc32_u64(hi as u64, lo) }
}

/// The first 8 and the next 4 bytes of a key.
fn parts(key: __m128i) -> (u64, u32) {
    // SAFETY: SSE4.1 is enabled for the whole build (it comes with AVX2).
    let (lo, hi) = unsafe { (_mm_cvtsi128_si64(key), _mm_extract_epi32::<2>(key)) };
    (lo as u64, hi as u32)
}

/// A window into it at 16 - len keeps the first len bytes of 16.
static KEEP: [[u8; 16]; 2] = [[0xFF; 16], [0; 16]];

/// Every query term, with its id and (after `finish`) its ANCHOR flag.
#[derive(Default)]
struct Vocab {
    /// The terms of up to 12 bytes, keyed by their bytes (zero-padded: token
    /// bytes are never zero, so the key is exact and lo = 0 marks an empty slot),
    /// kept under 2.5 per bucket so that most buckets have a free slot: a word no
    /// query uses is then turned away by one probe.
    buckets: Vec<Bucket>,
    /// The home bucket of a key is its hash >> shift.
    shift: u32,
    /// A one-word Bloom filter in front of the table, 1 KB per 512 buckets
    /// (64 KB for 50,000 queries): `filter_bits(h)` are set in word
    /// `h & (filter.len() - 1)` for every key's hash h. A fifth of the corpus
    /// tokens are words no query uses, each a different random bucket of the
    /// 2 MB table, nearly always a cache miss; the filter, which stays in L2,
    /// turns away most of them without that miss: they are looked up in the
    /// extra empty bucket at the end, which no key reaches by its hash and
    /// whose probe ends at once (an empty slot matches no token's key).
    filter: Vec<u64>,
    /// Terms of 13 to 32 bytes: about 0.2% of the corpus tokens are that long.
    long: std::collections::HashMap<&'static [u8], u32>,
    count: u32,
}

/// The two bits of a hash's filter word, from bits above the word's index.
fn filter_bits(h: u64) -> u64 {
    1 << (h >> 13 & 63) | 1 << (h >> 19 & 63)
}

impl Vocab {
    /// The mask of a bucket index: the buckets but the extra empty one.
    fn mask(&self) -> usize {
        self.buckets.len() - 2
    }

    fn home(&self, (lo, hi): (u64, u32)) -> usize {
        (hash(lo, hi) >> self.shift) as usize
    }

    /// Moves the terms to a table of `buckets` buckets, a power of two.
    fn rebuild(&mut self, buckets: usize) {
        let old = std::mem::replace(&mut self.buckets, vec![Bucket::default(); buckets + 1]);
        self.shift = 32 - buckets.trailing_zeros();
        self.filter = vec![0; buckets / 4];
        for slot in old.iter().flat_map(|b| b.0).filter(|s| s.lo != 0) {
            self.insert((slot.lo, slot.hi), slot.info);
        }
    }

    /// Adds a key that is not in the table yet, in the first free slot from its
    /// home bucket on (slots fill in order, so a bucket is full when its last is).
    fn insert(&mut self, (lo, hi): (u64, u32), info: u32) {
        let h = hash(lo, hi);
        let word = h as usize & (self.filter.len() - 1);
        self.filter[word] |= filter_bits(h);
        let mut b = self.home((lo, hi));
        while self.buckets[b].0[3].lo != 0 {
            b = (b + 1) & self.mask();
        }
        *self.buckets[b].0.iter_mut().find(|s| s.lo == 0).unwrap() = Slot { lo, hi, info };
    }

    /// The id of `term`, whose table key is `key`.
    fn intern(&mut self, term: &'static [u8], key: __m128i) -> u32 {
        let id = if term.len() > KEY {
            *self.long.entry(term).or_insert(self.count)
        } else {
            let id = find(self.buckets.as_ptr(), self.mask(), self.home(parts(key)), key);
            if id == MISS {
                if (self.count as usize + 1) * 2 > (self.mask() + 1) * 5 {
                    self.rebuild(2 * (self.mask() + 1));
                }
                self.insert(parts(key), self.count);
            }
            id.min(self.count)
        };
        self.count += (id == self.count) as u32;
        id
    }

    /// Renumbers the terms as the plan does and flags the anchors.
    fn finish(&mut self, plan: &Plan) {
        for slot in self.buckets.iter_mut().flat_map(|b| &mut b.0) {
            if slot.lo != 0 {
                slot.info = plan.info[slot.info as usize];
            }
        }
        for info in self.long.values_mut() {
            *info = plan.info[*info as usize];
        }
    }

    #[cold]
    #[inline(never)]
    fn get_long(&self, token: &[u8]) -> u32 {
        let mut lower = [0u8; MAX_TERM];
        let Some(lower) = lower.get_mut(..token.len()) else {
            return MISS;
        };
        lower.copy_from_slice(token);
        lower.make_ascii_lowercase();
        self.long.get(&*lower).copied().unwrap_or(MISS)
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
    /// The info word of each term, by its old id: its new id, and ANCHOR.
    info: Vec<u32>,
    /// The BOOL terms have the new ids 0..nb.
    nb: usize,
    /// The number of lines, which is also the number of answers.
    lines: usize,
    /// Each line's answer, the index of its state in each thread, and back.
    answer_of: Vec<u32>,
    line_of: Vec<u32>,
}

impl Plan {
    /// The per-line work runs on `parts` threads, a range of lines each.
    fn new(q: Queries, sample_df: &[u32], sampled: u32, parts: usize) -> Self {
        let lines = q.lines.len();
        let line_terms = |start: u32, len: u32| &q.terms[start as usize..(start + len) as usize];
        // The expected share of 64-document blocks that contain t: the cost of
        // triggering on it.
        let cost =
            |t: u32| 1.0 - (1.0 - sample_df[t as usize] as f64 / sampled.max(1) as f64).powi(64);
        // Whether each line is a BOOL query evaluated in every superblock.
        let always_line = |line: usize| {
            let (start, len, phrase) = q.lines[line];
            !phrase && {
                let (cover, n) = cover(line_terms(start, len), cost);
                cover[..n].iter().map(|&t| cost(t)).sum::<f64>() > ALWAYS
            }
        };
        let always_lines = par(parts, |p| {
            part(lines, p, parts).map(always_line).collect::<Vec<_>>()
        });
        let always_lines = always_lines.concat();
        // Per old id: 2 for a term of an `always` query, 1 for another BOOL term.
        let mut kind = vec![0u8; sample_df.len()];
        for (&(start, len, _), &always) in q.lines.iter().zip(&always_lines).filter(|l| !l.0.2) {
            for &word in line_terms(start, len) {
                let k = &mut kind[(word & TERM) as usize];
                *k = (*k).max(1 + always as u8);
            }
        }
        // Renumber the terms: BOOL terms first, those of the `always` lines
        // first among them, each part by sampled frequency, most frequent first,
        // so the presence words and entries the scan uses most share cache lines.
        let all = 0..sample_df.len() as u32;
        let (mut order, phrase_only): (Vec<_>, Vec<_>) = all.partition(|&t| kind[t as usize] > 0);
        order.sort_unstable_by_key(|&t| Reverse((kind[t as usize], sample_df[t as usize])));
        let (nb, rows) = (order.len(), kind.iter().filter(|&&k| k == 2).count());
        order.extend(phrase_only);
        let mut rank = vec![0; order.len()];
        for (id, &old) in order.iter().enumerate() {
            rank[old as usize] = id as u32;
        }
        let mut terms = q.terms;
        for word in &mut terms {
            *word = rank[(*word & TERM) as usize] | *word & !TERM;
        }
        // The costs by new id, so `cover` finds the sets `always_line` found.
        let cost = |t: u32| cost(order[t as usize]);
        let lines_part = |p: usize| {
            let (mut by_trigger, mut by_anchor, mut always) = (Vec::new(), Vec::new(), Vec::new());
            for line in part(lines, p, parts) {
                let (start, len, phrase) = q.lines[line];
                let words = &terms[start as usize..(start + len) as usize];
                let answer = line as u32; // renumbered below
                if phrase {
                    // The first of the rarest terms.
                    let rarest = |&k: &usize| sample_df[order[words[k] as usize] as usize];
                    let offset = (0..words.len()).min_by_key(rarest).unwrap();
                    let anchored = Anchored {
                        answer,
                        offset: offset as u32,
                        start,
                        len,
                    };
                    by_anchor.push((words[offset], anchored));
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
                    entry.earlier = 0;
                    for (k, &word) in words.iter().enumerate() {
                        entry.earlier |= (cover[..i].contains(&(word & TERM)) as u32) << k;
                    }
                    by_trigger.push((c, entry));
                }
            }
            (by_trigger, by_anchor, always)
        };
        let (mut by_trigger, mut by_anchor, mut always) = (Vec::new(), Vec::new(), Vec::new());
        for (t, a, e) in par(parts, lines_part) {
            (by_trigger.extend(t), by_anchor.extend(a), always.extend(e));
        }
        // By length and operators, so that `end_super` predicts its branches.
        always.sort_unstable_by_key(|e| (e.earlier, e.terms.map(|word| word >> 30)));
        let (group, mut entries) = csr(by_trigger, nb + 1);
        let (anchor, mut anchored) = csr(by_anchor, order.len());
        // Answers numbered in the order the scan visits them: the `always` ones
        // first, so that each superblock reads and writes their state as one
        // stream (47 KB instead of 4,000 scattered lines), then those of the
        // entries in their order, then the phrases.
        let (mut answer_of, mut line_of) = (vec![u32::MAX; lines], Vec::with_capacity(lines));
        let mut number = |line: &mut u32| {
            if answer_of[*line as usize] == u32::MAX {
                answer_of[*line as usize] = line_of.len() as u32;
                line_of.push(*line);
            }
            *line = answer_of[*line as usize];
        };
        always.iter_mut().for_each(|e| number(&mut e.answer));
        entries.iter_mut().for_each(|e| number(&mut e.answer));
        anchored.iter_mut().for_each(|a| number(&mut a.answer));
        (0..lines as u32).for_each(|mut line| number(&mut line)); // none should be left
        let flag = |id: usize| (anchor[id] != anchor[id + 1]) as u32 * ANCHOR;
        let info = rank.iter().map(|&id| id | flag(id as usize)).collect();
        Plan {
            group,
            entries,
            always,
            rows,
            anchor,
            anchored,
            terms,
            info,
            nb,
            lines,
            answer_of,
            line_of,
        }
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
    pairs.iter().for_each(|&(k, _)| start[k as usize + 1] += 1);
    (0..keys).for_each(|k| start[k + 1] += start[k]);
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
    vocab: &'a Vocab,
    plan: Option<&'a Plan>,
    df: &'a [AtomicU32],
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
    fn new(vocab: &'a Vocab, plan: Option<&'a Plan>, df: &'a [AtomicU32]) -> Self {
        let (nb, rows) = plan.map_or((0, 0), |p| (p.nb, p.rows));
        let (mut w, mut wide) = (written_zeros(nb + 2), vec![Row([0; 4]); rows + 2]);
        (w[nb + 1], wide[rows + 1]) = (!0, Row([!0; 4]));
        Scan {
            vocab,
            plan,
            df,
            w,
            touched: vec![0; nb + 2],
            n: 0,
            wide,
            pending: Vec::new(),
            toks: Vec::new(),
            anchors: Vec::new(),
            doc: 0,
            answers: Answers::new(plan.map_or(0, |p| p.lines)),
            last: vec![u32::MAX; plan.map_or(0, |p| p.anchored.len())],
        }
    }

    /// Scans the whole lines in `docs[start..end]`, whose first id is `first`.
    fn run(mut self, docs: &'static [u8], start: usize, end: usize, first: u32) -> Self {
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
                let (buckets, shift) = (self.vocab.buckets.as_ptr(), self.vocab.shift);
                let (filter, words) = (self.vocab.filter.as_ptr(), self.vocab.filter.len() - 1);
                let empty = self.vocab.mask() as u64 + 1;
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
                        // A token the filter turns away is looked up, without a
                        // branch, in the extra empty bucket.
                        // SAFETY: h & words < filter.len().
                        let bits = filter_bits(h);
                        let absent = unsafe { *filter.add(h as usize & words) } & bits ^ bits;
                        let b = if_zero(absent, h >> shift, empty) as usize;
                        // SAFETY: a prefetch of a bucket of the table.
                        unsafe { _mm_prefetch::<_MM_HINT_T0>(buckets.add(b) as *const i8) };
                        (key, b as u64)
                    } else {
                        let info = self.vocab.get_long(&docs[p..p + len]) as u64;
                        // SAFETY: AVX2 is enabled for the whole build.
                        (unsafe { _mm_setzero_si128() }, info)
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
                assert!(docs[p + len] == b'\t', "docs.txt: no TAB after an id");
                self.look_up(bit);
                bit = self.next_doc(&docs[p..p + len]);
            }
            mask = next;
        }
        self.look_up(bit);
        self.end_doc();
        self.end_block(self.doc & !63);
        self.end_super(self.doc / SUPER * SUPER);
        self
    }

    /// Looks up the tokens of this document, whose bit is `bit`, and sets their
    /// words. Each step runs over the whole document, so that the memory accesses
    /// of one step overlap: the buckets were fetched when the tokens were found.
    /// (Fetching the words too cost more instructions than it saved.)
    fn look_up(&mut self, bit: u64) {
        // The number of BOOL terms.
        let (nb, table) = (self.w.len() - 2, self.vocab);
        let (buckets, mask) = (table.buckets.as_ptr(), table.mask());
        self.toks.reserve(self.pending.len());
        let (toks, mut len) = (self.toks.as_mut_ptr(), self.toks.len());
        for &(key, b) in &self.pending {
            // SAFETY: SSE4.1 is enabled for the whole build (it comes with AVX2).
            let info = if unsafe { _mm_testz_si128(key, key) } == 1 {
                b as u32
            } else {
                find(buckets, mask, b as usize, key)
            };
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
    fn next_doc(&mut self, digits: &[u8]) -> u64 {
        let id = parse_id(digits);
        let ok = id == self.doc.wrapping_add(1);
        assert!(ok, "docs.txt: ids must be 0, 1, 2, ... in order");
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
        1 << (id % 64)
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
                        let Some(from) = (at as usize).checked_sub(a.offset as usize) else {
                            continue;
                        };
                        let phrase = &plan.terms[a.start as usize..(a.start + a.len) as usize];
                        let hit = self.toks.get(from..from + phrase.len()) == Some(phrase);
                        if hit && self.last[i] != doc {
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
            let (touched, group) = (&self.touched[..self.n], plan.group.as_ptr());
            for (i, t) in touched.iter().map(|&t| t as usize).enumerate() {
                // Fetched in advance: the group 16 slots ahead, the entries of 8 ahead.
                // SAFETY: prefetches of the plan's arrays (touched slots are at most
                // nb, and group has nb + 2 entries); a prefetch never faults anyway.
                unsafe {
                    let ahead = |k: usize| *touched.get(i + k).unwrap_or(&0) as usize;
                    _mm_prefetch::<_MM_HINT_T0>(group.add(ahead(16)) as *const i8);
                    let entries = plan.entries.as_ptr().add(*group.add(ahead(8)) as usize);
                    _mm_prefetch::<_MM_HINT_T0>(entries as *const i8);
                }
                // SAFETY: touched slots are at most nb, and group has nb + 2 entries.
                let (from, to) = unsafe { (*group.add(t) as usize, *group.add(t + 1) as usize) };
                // SAFETY: group holds offsets into entries.
                for e in unsafe { plan.entries.get_unchecked(from..to) } {
                    // All ones when bit 0 of b is set: bit(op) for NOT and OR,
                    // bit(op >> 1) only for OR.
                    let bit = |b: u32| 0u64.wrapping_sub((b & 1) as u64);
                    let r = e.terms.iter().fold(0u64, |r, &word| {
                        // SAFETY: every term word's slot is below nb or the pad
                        // slot nb + 1, and w has nb + 2 words.
                        let x = unsafe { *w.get_unchecked((word & TERM) as usize) };
                        // AND: r & x. NOT: r & !x. OR: (r & !x) | x = r | x.
                        (r & (x ^ bit(word >> 30))) | (x & bit(word >> 31))
                    });
                    // A branch on the few entries with earlier cover terms.
                    let present = |k| w[(e.terms[k] & TERM) as usize] & bit(e.earlier >> k) != 0;
                    let seen = e.earlier != 0 && (0..MAX_BOOL_TERMS).any(present);
                    self.answers.add(e.answer, if seen { 0 } else { r }, base);
                }
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
    let mut zeros = Vec::with_capacity(len);
    zeros.resize(len, std::hint::black_box(0));
    zeros
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
        Answers {
            state: written_zeros(answers),
            // One more: `add` always writes the slot after the logged ids.
            log: vec![0; answers * 10 + 1],
            logged: 0,
        }
    }

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
        // the positions that `digit(j)` covers (0xAAAA..., 0xCCCC..., 0xF0F0...,
        // ..., 0xFFFF_FFFF_0000_0000), constants once the loop is unrolled.
        let digit = |j: u32| !0u64 / ((1 << (1 << j)) + 1) << (1 << j);
        let positions = (0..6).map(|j| ((hits & digit(j)).count_ones() as u64) << j);
        let n = hits.count_ones() as u64;
        *s += n << SUM_BITS | (n * base as u64 + positions.sum::<u64>());
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
        let s = _mm256_castsi256_si128(sums);
        let s = _mm_add_epi32(s, _mm256_extracti128_si256::<1>(sums));
        let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b01_00_11_10>(s));
        let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b10_11_00_01>(s));
        let total = _mm_cvtsi128_si32(s) as u64;
        (total >> 16, total & 0xFFFF)
    }
}

/// 8b at byte b.
static EIGHT_B: [u8; 32] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120, 128, 136, 144, 152, 160, 168,
    176, 184, 192, 200, 208, 216, 224, 232, 240, 248,
];

/// Bit i is set when byte `at + i` is `byte`.
fn byte_mask(docs: &[u8], at: usize, byte: u8) -> u64 {
    // SAFETY: unaligned 32-byte loads from 32-byte slices; AVX2 is enabled for the whole build.
    let half = |at: usize| unsafe {
        let v = _mm256_loadu_si256(docs[at..at + 32].as_ptr() as *const __m256i);
        movemask(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(byte as i8)))
    };
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
            let at_least = |x, lo: u8| _mm256_cmpgt_epi8(x, _mm256_set1_epi8(lo as i8 - 1));
            let at_most = |x, hi: u8| _mm256_cmpgt_epi8(_mm256_set1_epi8(hi as i8 + 1), x);
            let between = |x, lo, hi| _mm256_and_si256(at_least(x, lo), at_most(x, hi));
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

/// `vmovmskps`, hidden from LLVM like `movemask`.
fn movemask_ps(v: __m256i) -> u32 {
    let m: u32;
    // SAFETY: AVX2 is enabled for this build; the instruction reads one register.
    unsafe { asm!("vmovmskps {:e}, {}", out(reg) m, in(ymm_reg) v, options(pure, nomem, nostack)) };
    m
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
    let mut starts: Vec<usize> = (0..parts).map(|p| cut(text, p, parts)).collect();
    starts.dedup();
    starts.retain(|&start| start < text.len());
    let ends = starts.iter().skip(1).copied().chain([text.len()]);
    let id = |at: usize| {
        let tab = text[at..].iter().position(|&b| b == b'\t');
        parse_id(&text[at..at + tab.expect("docs.txt: a line without a TAB")])
    };
    let slice = |(&start, end): (&usize, usize)| (start, end, id(start));
    starts.iter().zip(ends).map(slice).collect()
}

/// The start of the p-th of `parts` ranges of whole lines of `text`.
fn cut(text: &[u8], p: usize, parts: usize) -> usize {
    line_after(text, text.len() * p / parts)
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

fn push_decimal(out: &mut Vec<u8>, mut value: u64) {
    let digits = value.checked_ilog10().unwrap_or(0) as usize + 1;
    out.resize(out.len() + digits, 0);
    for digit in out.iter_mut().rev().take(digits) {
        *digit = b'0' + (value % 10) as u8;
        value /= 10;
    }
}
