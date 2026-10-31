//! Track B (Index): answer 50,000 BOOL and PHRASE queries over a 254 MB corpus.
//!
//! How it is fast:
//! 1. The queries are known before the corpus is read, so the corpus is scanned
//!    once, and what is kept per term depends on how the queries use it. A sample
//!    of the first 1,000 documents estimates every query term's frequency first.
//! 2. Most BOOL queries are answered while the corpus streams past, with no index
//!    at all: a query is evaluated, strictly left to right as the spec requires,
//!    only for documents that contain its trigger term, and only its count, id sum
//!    and ten smallest ids are kept. The trigger is the rarest term a match must
//!    contain (every term AND-ed after the last OR, plus the first term when there
//!    is no OR); a query with nothing necessary is triggered by its first term and
//!    every OR-ed term, since AND and NOT can only remove documents.
//! 3. A query triggered by a common word (in more than 1 document in 32) would be
//!    evaluated for most of the corpus. Those queries are instead answered after
//!    the scan as set operations: common terms get one bit per document (31 KB at
//!    full scale), and the rarer terms of just those queries get posting lists.
//! 4. Each thread scans its own run of documents, tokenizing 32 bytes at a time
//!    (AVX2) and mapping tokens to query terms through a direct-mapped cache, a
//!    Bloom filter and a packed hash table. Phrases need no positions: each is
//!    anchored on its rarest term and compared against the document's token buffer.

mod mmap;

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::thread;

/// Query terms are 1 to 32 bytes; a longer corpus token can never match one.
const MAX_TERM: usize = 32;
const NOT_A_TERM: u32 = u32::MAX;
const TOP: usize = 10;

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
    let open = |name: &str| -> Result<&'static [u8], String> {
        let path = format!("{data_dir}/{name}");
        let file = File::open(&path).map_err(|e| format!("open {path}: {e}"))?;
        mmap::map(&file).map_err(|e| format!("mmap {path}: {e}"))
    };
    let docs = open("docs.txt")?;
    let queries_txt = open("queries.txt")?;
    let threads = thread::available_parallelism().map_or(1, |n| n.get());

    let mut terms = Terms::new();
    let queries = Queries::parse(queries_txt, &mut terms)?;
    terms.finish();
    let plan = Plan::new(docs, &terms, &queries)?;

    // Every thread scans its own run of whole documents and keeps its own totals.
    let words = plan.docs.div_ceil(64);
    let mut bits = vec![0u64; plan.dense_terms * words];
    // SAFETY: AtomicU64 has the size, alignment and bit validity of u64, and this
    // atomic view ends before `bits` is read again.
    let shared_bits: &[AtomicU64] = unsafe { std::slice::from_raw_parts(bits.as_mut_ptr() as *const AtomicU64, bits.len()) };
    let slices = split_at_lines(docs, threads);
    let parts: Vec<Part> = thread::scope(|s| {
        let (terms, plan, queries) = (&terms, &plan, &queries);
        let handles: Vec<_> = slices.iter().map(|&slice| s.spawn(move || scan(slice, terms, plan, queries, shared_bits, words))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, String>>()
    })?;
    let index = Index::build(&parts, &plan, bits, words);

    // The set queries, in parallel.
    let set_queries = plan.set_queries.len();
    let set_results: Vec<Summary> = {
        let results: Vec<std::sync::Mutex<Summary>> = (0..set_queries).map(|_| Default::default()).collect();
        let next = AtomicUsize::new(0);
        thread::scope(|s| {
            for _ in 0..threads.min(set_queries.max(1)) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Relaxed);
                    let Some(&q) = plan.set_queries.get(i) else { break };
                    *results[i].lock().unwrap() = index.run_bool(&queries, q as usize).summary();
                });
            }
        });
        results.into_iter().map(|m| m.into_inner().unwrap()).collect()
    };

    // Phrase matches are few: sort them by (phrase, doc) and cut them into lists.
    let mut hits: Vec<u64> = parts.iter().flat_map(|part| part.phrase_hits.iter().copied()).collect();
    hits.sort_unstable();
    let mut phrase_start = vec![0usize; queries.phrases.len() + 1];
    for &hit in &hits {
        phrase_start[(hit >> 32) as usize + 1] += 1;
    }
    for p in 0..queries.phrases.len() {
        phrase_start[p + 1] += phrase_start[p];
    }

    let mut out = Vec::with_capacity(queries.order.len() * 32);
    for query in &queries.order {
        let summary = match *query {
            Query::Bool(q) => match plan.kind[q as usize] {
                k if k & SET_FLAG != 0 => set_results[(k & !SET_FLAG) as usize].clone(),
                k => {
                    let k = k as usize;
                    let running = parts.iter().map(|part| &part.running[k]);
                    Summary {
                        count: running.clone().map(|r| r.count as u64).sum(),
                        sum: running.clone().map(|r| r.sum).sum(),
                        first: parts
                            .iter()
                            .flat_map(|part| &part.tops[k * TOP..k * TOP + (part.running[k].count as usize).min(TOP)])
                            .take(TOP)
                            .map(|&d| d as u64)
                            .collect(),
                    }
                }
            },
            Query::Phrase(p) => {
                let docs = &hits[phrase_start[p as usize]..phrase_start[p as usize + 1]];
                Summary { count: docs.len() as u64, sum: docs.iter().map(|&h| h as u32 as u64).sum(), first: docs.iter().take(TOP).map(|&h| h as u32 as u64).collect() }
            }
        };
        summary.write(&mut out);
    }
    let output_path = format!("{out_dir}/results.txt");
    File::create(&output_path)
        .and_then(|mut f| f.write_all(&out))
        .map_err(|e| format!("write {output_path}: {e}"))
}

/// A query's answer: match count, sum of matching ids, the ten smallest ids.
#[derive(Clone, Default)]
struct Summary {
    count: u64,
    sum: u64,
    first: Vec<u64>,
}

impl Summary {
    /// "count sum id_1 ... id_m" with the m = min(10, count) smallest ids.
    fn write(&self, out: &mut Vec<u8>) {
        push_decimal(out, self.count);
        out.push(b' ');
        push_decimal(out, self.sum);
        for &doc in &self.first {
            out.push(b' ');
            push_decimal(out, doc);
        }
        out.push(b'\n');
    }
}

#[derive(Clone, Copy)]
enum Query {
    /// Index of a distinct BOOL query.
    Bool(u32),
    /// Index of a distinct phrase.
    Phrase(u32),
}

const AND: u8 = 0;
const OR: u8 = 1;
const NOT: u8 = 2;

/// The queries, with repeated lines folded into one distinct query each.
struct Queries {
    /// One entry per line of queries.txt, in order.
    order: Vec<Query>,
    /// Distinct BOOL query q is `terms[start[q]..start[q + 1]]`, with `ops[k]` the
    /// operator in front of `terms[k]` (unused for a query's first term).
    start: Vec<u32>,
    terms: Vec<u32>,
    ops: Vec<u8>,
    /// Offset of the last OR in each query (0 when it has none): once the running
    /// result is false past it, nothing can make it true again.
    last_or: Vec<u32>,
    phrases: Vec<Vec<u32>>,
}

impl Queries {
    fn parse(text: &'static [u8], terms: &mut Terms) -> Result<Self, String> {
        let mut queries = Queries { order: Vec::new(), start: vec![0], terms: Vec::new(), ops: Vec::new(), last_or: Vec::new(), phrases: Vec::new() };
        let mut bool_ids: HashMap<Vec<u64>, u32> = HashMap::new();
        let mut phrase_ids: HashMap<Vec<u32>, u32> = HashMap::new();
        for line in text.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let bad = || format!("bad query {:?}", String::from_utf8_lossy(line));
            if let Some(rest) = line.strip_prefix(b"BOOL ") {
                let mut words = rest.split(|&b| b == b' ');
                let mut parts = vec![terms.intern(words.next().ok_or_else(bad)?, Use::Bool) as u64];
                while let Some(op) = words.next() {
                    let op = match op {
                        b"AND" => AND,
                        b"OR" => OR,
                        b"NOT" => NOT,
                        _ => return Err(bad()),
                    };
                    parts.push((op as u64) << 32 | terms.intern(words.next().ok_or_else(bad)?, Use::Bool) as u64);
                }
                let next = queries.start.len() as u32 - 1;
                let id = *bool_ids.entry(parts.clone()).or_insert_with(|| {
                    let mut last_or = 0;
                    for (k, &part) in parts.iter().enumerate() {
                        queries.terms.push(part as u32);
                        queries.ops.push((part >> 32) as u8);
                        if k > 0 && (part >> 32) as u8 == OR {
                            last_or = k as u32;
                        }
                    }
                    queries.last_or.push(last_or);
                    queries.start.push(queries.terms.len() as u32);
                    next
                });
                queries.order.push(Query::Bool(id));
            } else if let Some(rest) = line.strip_prefix(b"PHRASE \"").and_then(|r| r.strip_suffix(b"\"")) {
                let phrase: Vec<u32> = rest.split(|&b| b == b' ').map(|t| terms.intern(t, Use::Phrase)).collect();
                let id = *phrase_ids.entry(phrase.clone()).or_insert_with(|| {
                    queries.phrases.push(phrase);
                    queries.phrases.len() as u32 - 1
                });
                queries.order.push(Query::Phrase(id));
            } else {
                return Err(bad());
            }
        }
        Ok(queries)
    }

    fn bool_count(&self) -> usize {
        self.start.len() - 1
    }

    /// The term offsets of distinct BOOL query q.
    fn span(&self, q: usize) -> std::ops::Range<usize> {
        self.start[q] as usize..self.start[q + 1] as usize
    }

}

#[derive(Clone, Copy)]
enum Use {
    Bool,
    Phrase,
}

/// The query terms: an open-addressing table over slices of queries.txt, used
/// while the queries are read, then `short`, a flat copy for the corpus scan.
struct Terms {
    slots: Vec<u32>, // 0 = empty, else id + 1
    names: Vec<&'static [u8]>,
    /// Whether a BOOL query uses the term, so the scan must track its presence.
    in_bool: Vec<bool>,
    /// Terms of up to 16 bytes, keyed by `key16`. The key is the term itself, so a
    /// probe is one 128-bit compare with no pointer to chase.
    short: ShortTable,
}

/// A token of up to 16 bytes packed into a u128, zero-padded. Token bytes are never
/// zero, so the padding also encodes the length.
fn key16(bytes: &[u8]) -> u128 {
    let mut word = [0u8; 16];
    word[..bytes.len()].copy_from_slice(bytes);
    u128::from_le_bytes(word)
}

fn hash16(key: u128) -> u64 {
    let h = (key as u64 ^ ((key >> 64) as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^ (h >> 32)
}

/// Open addressing over (key, id) pairs; key 0 = empty. Key and id share a slot,
/// so a probe touches one cache line, not one in a key array and one in an id array.
/// Half of the tokens that reach this table are words no query uses; a 256 KB Bloom
/// filter in front, which stays in L2, turns most of those away without a probe.
#[derive(Default)]
struct ShortTable {
    slots: Vec<(u128, u32)>,
    bloom: Vec<u64>,
}

const BLOOM_BITS: u32 = 21;

impl ShortTable {
    fn new(entries: impl Iterator<Item = (u128, u32)> + Clone) -> Self {
        let size = (entries.clone().count() * 2).next_power_of_two().max(16);
        let mut table = ShortTable { slots: vec![(0, NOT_A_TERM); size], bloom: vec![0; 1 << (BLOOM_BITS - 6)] };
        for (key, id) in entries {
            let i = table.slot(key);
            table.slots[i] = (key, id);
            for bit in bloom_bits(key) {
                table.bloom[bit / 64] |= 1 << (bit % 64);
            }
        }
        table
    }

    fn maybe_contains(&self, key: u128) -> bool {
        bloom_bits(key).iter().all(|&bit| self.bloom[bit / 64] >> (bit % 64) & 1 == 1)
    }

    fn home(&self, key: u128) -> usize {
        (hash16(key) >> 20) as usize & (self.slots.len() - 1)
    }

    fn slot(&self, key: u128) -> usize {
        let mask = self.slots.len() - 1;
        let mut i = self.home(key);
        while self.slots[i].0 != key && self.slots[i].0 != 0 {
            i = (i + 1) & mask;
        }
        i
    }

}

/// Two positions in the Bloom filter, from independent bits of the key's hash.
fn bloom_bits(key: u128) -> [usize; 2] {
    let h = hash16(key);
    let mask = (1 << BLOOM_BITS) - 1;
    [(h & mask) as usize, ((h >> 40) & mask) as usize]
}

impl Terms {
    fn new() -> Self {
        Terms { slots: vec![0; 1 << 16], names: Vec::new(), in_bool: Vec::new(), short: ShortTable::default() }
    }

    /// Builds the scan-time table once every query has been read.
    fn finish(&mut self) {
        let names = &self.names;
        self.short = ShortTable::new(
            names.iter().enumerate().filter(|(_, n)| n.len() <= 16).map(|(id, n)| (key16(n), id as u32)),
        );
    }

    fn intern(&mut self, term: &'static [u8], usage: Use) -> u32 {
        let id = match self.get(term) {
            Some(id) => id,
            None => {
                self.names.push(term);
                self.in_bool.push(false);
                if self.names.len() * 2 > self.slots.len() {
                    self.slots = vec![0; self.slots.len() * 2];
                    for id in 0..self.names.len() {
                        self.place(id);
                    }
                } else {
                    self.place(self.names.len() - 1);
                }
                self.names.len() as u32 - 1
            }
        };
        if let Use::Bool = usage {
            self.in_bool[id as usize] = true;
        }
        id
    }

    fn place(&mut self, id: usize) {
        let mask = self.slots.len() - 1;
        let mut i = hash(self.names[id]) as usize & mask;
        while self.slots[i] != 0 {
            i = (i + 1) & mask;
        }
        self.slots[i] = id as u32 + 1;
    }

    fn get(&self, token: &[u8]) -> Option<u32> {
        let mask = self.slots.len() - 1;
        let mut i = hash(token) as usize & mask;
        loop {
            match self.slots[i] {
                0 => return None,
                id if self.names[id as usize - 1] == token => return Some(id - 1),
                _ => i = (i + 1) & mask,
            }
        }
    }
}

/// One thread's tokenizer. Word frequencies are Zipfian, so a direct-mapped cache
/// of recent token -> term id answers most lookups from L1/L2.
struct Tokenizer<'a> {
    terms: &'a Terms,
    /// Per term id, how the scan uses it (see `Plan::info`); empty while sampling.
    info: &'a [u32],
    /// (key, term id, info): a hit answers everything about the token at once.
    hot: Vec<(u128, u32, u32)>,
    /// The current document, ASCII-lowercased, with 16 bytes of zero padding so a
    /// 16-byte key load at any token start stays in bounds.
    lower: Vec<u8>,
    /// Token boundaries in `lower`: token k is `starts[k]..ends[k]`.
    starts: Vec<u32>,
    ends: Vec<u32>,
    /// Per token of the current document: its key, then its (term id, info).
    keys: Vec<u128>,
    found: Vec<(u32, u32)>,
}

const HOT_SLOTS: usize = 1 << 14;
/// Marks a token longer than 16 bytes in `keys`; a real key is never 0.
const LONG: u128 = 0;
/// Marks a token in `found` whose lookup is not finished yet.
const PENDING: u32 = NOT_A_TERM - 1;

impl<'a> Tokenizer<'a> {
    fn new(terms: &'a Terms, info: &'a [u32]) -> Self {
        Tokenizer {
            terms,
            info,
            hot: vec![(0, NOT_A_TERM, 0); HOT_SLOTS],
            lower: Vec::new(),
            starts: Vec::new(),
            ends: Vec::new(),
            keys: Vec::new(),
            found: Vec::new(),
        }
    }

    fn info_of(&self, id: u32) -> u32 {
        self.info.get(id as usize).copied().unwrap_or(0)
    }

    fn hot_slot(key: u128) -> usize {
        hash16(key) as usize & (HOT_SLOTS - 1)
    }

    /// Calls `f(id, info)` with each token of `text` (ASCII-lowercased, maximal runs
    /// of [a-z0-9]), mapped to its query-term id or NOT_A_TERM (info 0).
    ///
    /// A document's lookups are independent, so they go in rounds: every round
    /// prefetches what the next one will read, for all the document's tokens, so
    /// the cache misses of rare words overlap instead of stalling one at a time.
    fn for_each_token(&mut self, text: &[u8], mut f: impl FnMut(u32, u32)) {
        self.split(text);
        let count = self.starts.len();
        self.keys.clear();
        self.found.clear();
        // Round 1: the keys, and a prefetch of each one's cache slot.
        for k in 0..count {
            let (start, len) = (self.starts[k] as usize, (self.ends[k] - self.starts[k]) as usize);
            let key = if len <= 16 {
                let bytes: [u8; 16] = self.lower[start..start + 16].try_into().unwrap();
                let keep = if len == 16 { u128::MAX } else { (1u128 << (8 * len)) - 1 };
                u128::from_le_bytes(bytes) & keep
            } else {
                LONG
            };
            prefetch(&self.hot[Self::hot_slot(key)]);
            self.keys.push(key);
        }
        // Round 2: cache hits are done; misses prefetch their Bloom filter words.
        for k in 0..count {
            let key = self.keys[k];
            let entry = self.hot[Self::hot_slot(key)];
            self.found.push(if key == LONG {
                let (start, end) = (self.starts[k] as usize, self.ends[k] as usize);
                let id = if end - start <= MAX_TERM { self.terms.get(&self.lower[start..end]).unwrap_or(NOT_A_TERM) } else { NOT_A_TERM };
                (id, self.info_of(id))
            } else if entry.0 == key {
                (entry.1, entry.2)
            } else {
                for bit in bloom_bits(key) {
                    prefetch(&self.terms.short.bloom[bit / 64]);
                }
                (PENDING, 0)
            });
        }
        // Round 3: the Bloom filter answers most misses; the rest prefetch their
        // table slot.
        for k in 0..count {
            if self.found[k].0 == PENDING {
                let key = self.keys[k];
                if self.terms.short.maybe_contains(key) {
                    prefetch(&self.terms.short.slots[self.terms.short.home(key)]);
                } else {
                    self.found[k] = (NOT_A_TERM, 0);
                    self.hot[Self::hot_slot(key)] = (key, NOT_A_TERM, 0);
                }
            }
        }
        // Round 4: the table probes, now that their lines are on the way.
        for k in 0..count {
            if self.found[k].0 == PENDING {
                let key = self.keys[k];
                let id = self.terms.short.slots[self.terms.short.slot(key)].1;
                self.found[k] = (id, self.info_of(id));
                self.hot[Self::hot_slot(key)] = (key, id, self.info_of(id));
            }
        }
        for k in 0..count {
            let (id, info) = self.found[k];
            f(id, info);
        }
    }

    /// Lowercases `text` into `lower` and records where every token starts and
    /// ends, 32 bytes at a time: one vector pass classifies the bytes, and the
    /// token boundaries are the edges of the resulting bitmask.
    fn split(&mut self, text: &[u8]) {
        let blocks = text.len().div_ceil(32);
        self.lower.clear();
        self.lower.resize(blocks * 32 + 16, 0);
        self.starts.clear();
        self.ends.clear();
        let mut carry = 0u32; // 1 if the byte before this block is a token byte
        for b in 0..blocks {
            let at = b * 32;
            let mut block = [0u8; 32]; // zero is a separator, so padding ends tokens
            let take = (text.len() - at).min(32);
            block[..take].copy_from_slice(&text[at..at + take]);
            let mask = classify(&block, (&mut self.lower[at..at + 32]).try_into().unwrap());
            let before = mask << 1 | carry;
            push_bits(&mut self.starts, mask & !before, at);
            push_bits(&mut self.ends, !mask & before, at);
            carry = mask >> 31;
        }
        if carry == 1 {
            self.ends.push((blocks * 32) as u32);
        }
    }
}

fn prefetch<T>(item: &T) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: prefetching is only a hint and never faults.
    unsafe {
        std::arch::x86_64::_mm_prefetch(item as *const T as *const i8, std::arch::x86_64::_MM_HINT_T0)
    };
}

/// Appends `at + i` for every set bit i of `bits`, lowest first.
fn push_bits(out: &mut Vec<u32>, mut bits: u32, at: usize) {
    while bits != 0 {
        out.push(at as u32 + bits.trailing_zeros());
        bits &= bits - 1;
    }
}

/// Writes the ASCII-lowercased block to `lower` and returns a mask with bit i set
/// when byte i is a token byte ([a-z0-9] after lowercasing).
#[cfg(target_feature = "avx2")]
fn classify(block: &[u8; 32], lower: &mut [u8; 32]) -> u32 {
    use std::arch::x86_64::*;
    // SAFETY: unaligned 32-byte load and store of 32-byte arrays.
    unsafe {
        let v = _mm256_loadu_si256(block.as_ptr() as *const __m256i);
        let between = |x, lo: u8, hi: u8| {
            _mm256_and_si256(_mm256_cmpgt_epi8(x, _mm256_set1_epi8(lo as i8 - 1)), _mm256_cmpgt_epi8(_mm256_set1_epi8(hi as i8 + 1), x))
        };
        // Signed compares: bytes 0x80 and up are negative, so never letters or digits.
        let low = _mm256_or_si256(v, _mm256_and_si256(between(v, b'A', b'Z'), _mm256_set1_epi8(0x20)));
        _mm256_storeu_si256(lower.as_mut_ptr() as *mut __m256i, low);
        _mm256_movemask_epi8(_mm256_or_si256(between(low, b'a', b'z'), between(v, b'0', b'9'))) as u32
    }
}

#[cfg(not(target_feature = "avx2"))]
fn classify(block: &[u8; 32], lower: &mut [u8; 32]) -> u32 {
    let mut mask = 0;
    for (i, (&byte, out)) in block.iter().zip(lower.iter_mut()).enumerate() {
        *out = byte.to_ascii_lowercase();
        mask |= ((out.is_ascii_lowercase() || out.is_ascii_digit()) as u32) << i;
    }
    mask
}

/// What the scan does with each term, decided from the queries and a sample.
struct Plan {
    /// Per term: flag bits (ANCHOR, TRIGGER, STAMP, COLLECT, DENSE) and, for a
    /// dense term, its bitset index in the low bits.
    info: Vec<u32>,
    dense_terms: usize,
    /// Documents in the corpus: the last line's id + 1 (ids are dense and in order).
    docs: usize,
    /// Streaming BOOL queries triggered by term t, as stream ids:
    /// `triggered[trigger_start[t]..trigger_start[t + 1]]`.
    trigger_start: Vec<u32>,
    triggered: Vec<u32>,
    /// The streaming queries by stream id, numbered in trigger order so that the
    /// queries one term triggers sit next to each other in memory.
    clauses: Vec<Clause>,
    /// Each BOOL query's stream id, or its index in `set_queries` with SET_FLAG.
    kind: Vec<u32>,
    /// The BOOL queries answered after the scan.
    set_queries: Vec<u32>,
    /// Phrases anchored on term t: `anchored[anchor_start[t]..anchor_start[t + 1]]`,
    /// as (phrase, position of t in the phrase).
    anchor_start: Vec<u32>,
    anchored: Vec<(u32, u32)>,
}

const ANCHOR: u32 = 1 << 31;
const TRIGGER: u32 = 1 << 30;
/// The scan must know whether the document contains this term.
const STAMP: u32 = 1 << 29;
/// A sparse term that a set query uses: its documents are collected into a list.
const COLLECT: u32 = 1 << 28;
const DENSE: u32 = 1 << 27;
const DENSE_INDEX: u32 = DENSE - 1;
const SET_FLAG: u32 = 1 << 31;
const SAMPLE_DOCS: u32 = 1000;

/// A streaming query laid out for the evaluation loop: at most 6 terms (the
/// grammar's limit), with the operator in front of each. Two fit a cache line.
#[derive(Clone, Copy, Default)]
struct Clause {
    len: u8,
    /// Offset of the last OR (0 when none): once the running result is false past
    /// it, nothing can make it true again.
    last_or: u8,
    ops: [u8; 6],
    terms: [u32; 6],
}

impl Clause {
    /// Whether the query matches a document, given `present(term)`, evaluated
    /// strictly left to right.
    fn matches(&self, present: impl Fn(u32) -> bool) -> bool {
        let mut result = present(self.terms[0]);
        for k in 1..self.len as usize {
            if !result && k > self.last_or as usize {
                return false;
            }
            let p = present(self.terms[k]);
            result = match self.ops[k] {
                AND => result && p,
                OR => result || p,
                _ => result && !p,
            };
        }
        result
    }
}

/// One streaming query's running answer in a thread: its evaluation stamp, match
/// count and id sum, 16 bytes so the whole table stays in L2. The first (smallest)
/// matching ids live apart, in `Part::tops`, touched only while there are few.
#[derive(Clone, Copy)]
struct Running {
    doc: u32,
    count: u32,
    sum: u64,
}

const FRESH: Running = Running { doc: u32::MAX, count: 0, sum: 0 };

impl Plan {
    fn new(docs: &[u8], terms: &Terms, queries: &Queries) -> Result<Self, String> {
        let term_count = terms.names.len();

        // Document frequency of every query term in the first SAMPLE_DOCS documents.
        // Documents are drawn independently, so the head of the file is a fair
        // sample, and a poor guess only costs time, never correctness.
        let mut df = vec![0u32; term_count];
        let mut seen_in = vec![u32::MAX; term_count];
        let mut tokenizer = Tokenizer::new(terms, &[]);
        let (mut sampled, mut i) = (0u32, 0);
        while i < docs.len() && sampled < SAMPLE_DOCS {
            let (text, end) = document(docs, i)?;
            tokenizer.for_each_token(text, |t, _| {
                if t != NOT_A_TERM && seen_in[t as usize] != sampled {
                    seen_in[t as usize] = sampled;
                    df[t as usize] += 1;
                }
            });
            sampled += 1;
            i = end + 1;
        }
        let dense = |t: u32| terms.in_bool[t as usize] && df[t as usize] * 32 > sampled;

        // Each BOOL query's triggers, and whether it streams or becomes a set query.
        let mut info = vec![0u32; term_count];
        let mut by_trigger: Vec<(u32, u32)> = Vec::new();
        let mut set_queries = Vec::new();
        let mut kind = vec![0; queries.bool_count()];
        for q in 0..queries.bool_count() {
            let span = queries.span(q);
            let last_or = span.start + queries.last_or[q] as usize;
            let necessary = (last_or == span.start).then_some(span.start).into_iter().chain((last_or + 1..span.end).filter(|&k| queries.ops[k] == AND));
            let triggers: Vec<u32> = match necessary.min_by_key(|&k| df[queries.terms[k] as usize]) {
                Some(k) => vec![queries.terms[k]],
                None => span.clone().filter(|&k| k == span.start || queries.ops[k] == OR).map(|k| queries.terms[k]).collect(),
            };
            if triggers.iter().any(|&t| dense(t)) {
                kind[q] = SET_FLAG | set_queries.len() as u32;
                set_queries.push(q as u32);
                for k in span {
                    info[queries.terms[k] as usize] |= if dense(queries.terms[k]) { DENSE } else { COLLECT };
                }
            } else {
                for k in span {
                    info[queries.terms[k] as usize] |= STAMP;
                }
                by_trigger.extend(triggers.iter().map(|&t| (t, q as u32)));
            }
        }
        by_trigger.sort_unstable();
        by_trigger.dedup();
        // Number the streaming queries in trigger order.
        let mut clauses = Vec::new();
        let mut stream_id = vec![u32::MAX; queries.bool_count()];
        for &(_, q) in &by_trigger {
            if stream_id[q as usize] == u32::MAX {
                stream_id[q as usize] = clauses.len() as u32;
                let span = queries.span(q as usize);
                let mut clause = Clause { len: span.len() as u8, last_or: queries.last_or[q as usize] as u8, ..Default::default() };
                for (k, at) in span.enumerate() {
                    clause.terms[k] = queries.terms[at];
                    clause.ops[k] = queries.ops[at];
                }
                clauses.push(clause);
                kind[q as usize] = stream_id[q as usize];
            }
        }
        let by_trigger = by_trigger.into_iter().map(|(t, q)| (t, stream_id[q as usize])).collect();
        let (trigger_start, triggered) = group(by_trigger, term_count);

        // Anchor each phrase on its rarest term.
        let mut by_anchor: Vec<(u32, (u32, u32))> = queries
            .phrases
            .iter()
            .enumerate()
            .map(|(p, phrase)| {
                let (at, &term) = phrase.iter().enumerate().min_by_key(|&(_, &t)| df[t as usize]).unwrap();
                (term, (p as u32, at as u32))
            })
            .collect();
        by_anchor.sort_unstable();
        let (anchor_start, anchored) = group(by_anchor, term_count);

        let mut dense_terms = 0;
        for t in 0..term_count {
            if info[t] & DENSE != 0 {
                info[t] |= dense_terms;
                dense_terms += 1;
            }
            if anchor_start[t] != anchor_start[t + 1] {
                info[t] |= ANCHOR;
            }
            if trigger_start[t] != trigger_start[t + 1] {
                info[t] |= TRIGGER;
            }
        }
        Ok(Plan { info, dense_terms: dense_terms as usize, docs: doc_count(docs)?, trigger_start, triggered, clauses, kind, set_queries, anchor_start, anchored })
    }

    fn triggered(&self, term: u32) -> &[u32] {
        &self.triggered[self.trigger_start[term as usize] as usize..self.trigger_start[term as usize + 1] as usize]
    }

    fn anchored(&self, term: u32) -> &[(u32, u32)] {
        &self.anchored[self.anchor_start[term as usize] as usize..self.anchor_start[term as usize + 1] as usize]
    }
}

/// Turns (term, value) pairs sorted by term into per-term ranges over `terms` terms.
fn group<T: Copy>(pairs: Vec<(u32, T)>, terms: usize) -> (Vec<u32>, Vec<T>) {
    let mut start = vec![0u32; terms + 1];
    for &(term, _) in &pairs {
        start[term as usize + 1] += 1;
    }
    for t in 0..terms {
        start[t + 1] += start[t];
    }
    (start, pairs.into_iter().map(|(_, value)| value).collect())
}

/// The text of the document whose line starts at `i`, and the offset of its LF.
fn document(docs: &[u8], i: usize) -> Result<(&[u8], usize), String> {
    let tab = i + docs[i..].iter().position(|&b| b == b'\t').ok_or("docs.txt: a line without a TAB")?;
    let end = tab + docs[tab..].iter().position(|&b| b == b'\n').ok_or("docs.txt: no final LF")?;
    Ok((&docs[tab + 1..end], end))
}

/// Ids are dense and ascending, so the count is the last line's id + 1.
fn doc_count(docs: &[u8]) -> Result<usize, String> {
    let Some(body) = docs.strip_suffix(b"\n") else { return Ok(0) };
    let start = body.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let tab = start + body[start..].iter().position(|&b| b == b'\t').ok_or("docs.txt: a line without a TAB")?;
    Ok(parse_doc_id(&body[start..tab])? as usize + 1)
}

/// One thread's results over its slice of documents.
struct Part {
    /// Per streaming query, by stream id; `tops[q * TOP..]` holds its first
    /// min(count, TOP) matching ids.
    running: Vec<Running>,
    tops: Vec<u32>,
    /// `term << 32 | doc` for COLLECT terms, once per document.
    pairs: Vec<u64>,
    /// `phrase << 32 | doc`, once per document.
    phrase_hits: Vec<u64>,
}

fn scan(slice: &[u8], terms: &Terms, plan: &Plan, queries: &Queries, bits: &[AtomicU64], words: usize) -> Result<Part, String> {
    let mut part = Part { running: vec![FRESH; plan.clauses.len()], tops: vec![0; plan.clauses.len() * TOP], pairs: Vec::new(), phrase_hits: Vec::new() };
    let mut tokenizer = Tokenizer::new(terms, &plan.info);
    // The last document that contained each term, and the last one that matched
    // each phrase: stamps, so nothing is reset per document.
    let mut term_doc = vec![u32::MAX; terms.names.len()];
    let mut phrase_doc = vec![u32::MAX; queries.phrases.len()];
    // This document's term ids, the positions of anchor terms among them, and the
    // trigger terms it contains.
    let (mut tokens, mut anchors, mut fired) = (Vec::new(), Vec::new(), Vec::new());
    let mut i = 0;
    while i < slice.len() {
        let tab = i + slice[i..].iter().position(|&b| b == b'\t').ok_or("docs.txt: a line without a TAB")?;
        let doc = parse_doc_id(&slice[i..tab])?;
        let (text, end) = document(slice, i)?;
        if doc as usize >= plan.docs {
            return Err("docs.txt: ids must be 0..N-1 in order".into());
        }
        tokens.clear();
        anchors.clear();
        fired.clear();
        tokenizer.for_each_token(text, |t, info| {
            if info & (STAMP | COLLECT | DENSE) != 0 && term_doc[t as usize] != doc {
                term_doc[t as usize] = doc;
                if info & TRIGGER != 0 {
                    fired.push(t);
                }
                if info & DENSE != 0 {
                    bits[(info & DENSE_INDEX) as usize * words + doc as usize / 64].fetch_or(1 << (doc % 64), Relaxed);
                } else if info & COLLECT != 0 {
                    part.pairs.push((t as u64) << 32 | doc as u64);
                }
            }
            if info & ANCHOR != 0 {
                anchors.push(tokens.len() as u32);
            }
            tokens.push(t);
        });
        for &t in &fired {
            for &q in plan.triggered(t) {
                let running = &mut part.running[q as usize];
                if running.doc == doc {
                    continue; // already evaluated through another trigger
                }
                running.doc = doc;
                if plan.clauses[q as usize].matches(|term| term_doc[term as usize] == doc) {
                    running.count += 1;
                    running.sum += doc as u64;
                    if (running.count as usize) <= TOP {
                        part.tops[q as usize * TOP + running.count as usize - 1] = doc;
                    }
                }
            }
        }
        for &at in &anchors {
            for &(p, offset) in plan.anchored(tokens[at as usize]) {
                let phrase = &queries.phrases[p as usize];
                let Some(from) = (at as usize).checked_sub(offset as usize) else { continue };
                if tokens.get(from..from + phrase.len()) == Some(phrase.as_slice()) && phrase_doc[p as usize] != doc {
                    phrase_doc[p as usize] = doc;
                    part.phrase_hits.push((p as u64) << 32 | doc as u64);
                }
            }
        }
        i = end + 1;
    }
    Ok(part)
}

/// What the set queries run on: a bitset per dense term, a posting list per
/// collected term.
struct Index {
    info: Vec<u32>,
    bits: Vec<u64>,
    words: usize,
    /// Posting list of collected term t: `postings[start[t]..start[t + 1]]`, ascending.
    start: Vec<u32>,
    postings: Vec<u32>,
}

impl Index {
    fn build(parts: &[Part], plan: &Plan, bits: Vec<u64>, words: usize) -> Self {
        // Counting sort of the collected pairs by term. Parts are in document order
        // and each part's pairs are too, so every list comes out ascending.
        let mut start = vec![0u32; plan.info.len() + 1];
        for pair in parts.iter().flat_map(|part| &part.pairs) {
            start[(pair >> 32) as usize + 1] += 1;
        }
        for t in 0..plan.info.len() {
            start[t + 1] += start[t];
        }
        let mut cursor = start.clone();
        let mut postings = vec![0u32; start[plan.info.len()] as usize];
        for &pair in parts.iter().flat_map(|part| &part.pairs) {
            let slot = &mut cursor[(pair >> 32) as usize];
            postings[*slot as usize] = pair as u32;
            *slot += 1;
        }
        Index { info: plan.info.clone(), bits, words, start, postings }
    }

    fn view(&self, term: u32) -> View<'_> {
        let info = self.info[term as usize];
        if info & DENSE != 0 {
            let d = (info & DENSE_INDEX) as usize;
            View::Bits(&self.bits[d * self.words..(d + 1) * self.words])
        } else {
            View::List(&self.postings[self.start[term as usize] as usize..self.start[term as usize + 1] as usize])
        }
    }

    /// Applies the operators strictly left to right.
    fn run_bool(&self, queries: &Queries, q: usize) -> Set {
        let span = queries.span(q);
        let mut result = Set::from(self.view(queries.terms[span.start]));
        for k in span.start + 1..span.end {
            result = apply(queries.ops[k], result.view(), self.view(queries.terms[k]));
        }
        result
    }
}

/// A query operand: a posting list or a bitset, borrowed from the index or a result.
#[derive(Clone, Copy)]
enum View<'a> {
    List(&'a [u32]),
    Bits(&'a [u64]),
}

/// An intermediate result of a set query.
enum Set {
    List(Vec<u32>),
    Bits(Vec<u64>),
}

impl From<View<'_>> for Set {
    fn from(view: View) -> Set {
        match view {
            View::List(list) => Set::List(list.to_vec()),
            View::Bits(bits) => Set::Bits(bits.to_vec()),
        }
    }
}

impl Set {
    fn view(&self) -> View<'_> {
        match self {
            Set::List(list) => View::List(list),
            Set::Bits(bits) => View::Bits(bits),
        }
    }

    fn summary(&self) -> Summary {
        let mut summary = Summary::default();
        match self.view() {
            View::List(list) => {
                summary.count = list.len() as u64;
                summary.sum = list.iter().map(|&d| d as u64).sum();
                summary.first = list.iter().take(TOP).map(|&d| d as u64).collect();
            }
            View::Bits(bits) => {
                for (w, &word) in bits.iter().enumerate() {
                    let mut rest = word;
                    while rest != 0 {
                        let doc = (w * 64) as u64 + rest.trailing_zeros() as u64;
                        summary.count += 1;
                        summary.sum += doc;
                        if summary.first.len() < TOP {
                            summary.first.push(doc);
                        }
                        rest &= rest - 1;
                    }
                }
            }
        }
        summary
    }
}

fn has(bits: &[u64], doc: u32) -> bool {
    bits[doc as usize / 64] >> (doc % 64) & 1 == 1
}

/// One operator on two operands. A list stays a list where the answer can only
/// shrink it (AND, and NOT with a list on the left); otherwise bitsets absorb lists.
fn apply(op: u8, a: View, b: View) -> Set {
    use View::{Bits, List};
    let words = |x: &[u64], y: &[u64], f: fn(u64, u64) -> u64| Set::Bits(x.iter().zip(y).map(|(&p, &q)| f(p, q)).collect());
    match (op, a, b) {
        (AND, List(x), List(y)) => Set::List(intersect(x, y)),
        (AND, List(x), Bits(y)) | (AND, Bits(y), List(x)) => Set::List(x.iter().copied().filter(|&d| has(y, d)).collect()),
        (AND, Bits(x), Bits(y)) => words(x, y, |p, q| p & q),
        (OR, List(x), List(y)) => Set::List(union(x, y)),
        (OR, List(x), Bits(y)) | (OR, Bits(y), List(x)) => {
            let mut bits = y.to_vec();
            for &d in x {
                bits[d as usize / 64] |= 1 << (d % 64);
            }
            Set::Bits(bits)
        }
        (OR, Bits(x), Bits(y)) => words(x, y, |p, q| p | q),
        (_, List(x), List(y)) => Set::List(difference(x, y)),
        (_, List(x), Bits(y)) => Set::List(x.iter().copied().filter(|&d| !has(y, d)).collect()),
        (_, Bits(x), List(y)) => {
            let mut bits = x.to_vec();
            for &d in y {
                bits[d as usize / 64] &= !(1 << (d % 64));
            }
            Set::Bits(bits)
        }
        (_, Bits(x), Bits(y)) => words(x, y, |p, q| p & !q),
    }
}

fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(small.len());
    // Galloping search when one side is much shorter, a linear merge otherwise.
    if small.len() * 16 < large.len() {
        let mut rest = large;
        for &x in small {
            let at = rest.partition_point(|&y| y < x);
            if at < rest.len() && rest[at] == x {
                out.push(x);
            }
            rest = &rest[at..];
        }
    } else {
        let (mut i, mut j) = (0, 0);
        while i < small.len() && j < large.len() {
            match small[i].cmp(&large[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    out.push(small[i]);
                    i += 1;
                    j += 1;
                }
            }
        }
    }
    out
}

fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

fn difference(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut j = 0;
    for &x in a {
        j += b[j..].partition_point(|&y| y < x);
        if j >= b.len() || b[j] != x {
            out.push(x);
        }
    }
    out
}

fn parse_doc_id(digits: &[u8]) -> Result<u32, String> {
    if digits.is_empty() || digits.len() > 9 || !digits.iter().all(u8::is_ascii_digit) {
        return Err(format!("docs.txt: bad id {:?}", String::from_utf8_lossy(digits)));
    }
    Ok(digits.iter().fold(0, |acc, &b| acc * 10 + (b - b'0') as u32))
}

/// A fast multiply-rotate hash over 8-byte words.
fn hash(bytes: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h = bytes.len() as u64;
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        h = (h ^ u64::from_le_bytes(word.try_into().unwrap())).wrapping_mul(K).rotate_left(31);
    }
    let mut tail = [0u8; 8];
    tail[..words.remainder().len()].copy_from_slice(words.remainder());
    h = (h ^ u64::from_le_bytes(tail)).wrapping_mul(K);
    h ^ (h >> 32)
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
