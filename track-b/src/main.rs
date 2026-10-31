//! Track B (Index): answer 50,000 BOOL and PHRASE queries over a 254 MB corpus.
//!
//! How it is fast:
//! 1. The queries come first. Only terms that some query uses are looked up, so
//!    the term table is small enough to stay in cache, and every other token is
//!    just a failed lookup.
//! 2. A sample of the first 1,000 documents estimates how common each query term
//!    is, and that decides how it is stored (see `Plan`). A term found in more than
//!    1 document in 32 gets a bitset with one bit per document (31 KB at full
//!    scale): cheaper than its posting list, and AND/OR/NOT become word operations.
//!    Rarer BOOL terms get sorted posting lists.
//! 3. The corpus is mmap'd and scanned once, in parallel slices of whole documents.
//!    Dense terms set their bit with an atomic OR; sparse terms emit (term, doc)
//!    pairs, and a parallel counting sort turns those into posting lists that are
//!    already sorted, because slices are in doc order.
//! 4. Phrases need no positions and no second look at the text. Each phrase is
//!    anchored on its rarest term. While a document is scanned its term ids sit in
//!    a small buffer, and when the document ends each anchor occurrence is checked
//!    against its phrases right there, in cache.
//! 5. BOOL queries run strictly left to right, as the spec requires, over lists and
//!    bitsets; only count, sum and the ten smallest ids are printed.

mod mmap;

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::thread;

/// Query terms are 1 to 32 bytes; a longer corpus token can never match one.
const MAX_TERM: usize = 32;
const NOT_A_TERM: u32 = u32::MAX;

fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    if let Err(message) = run(&data_dir, &out_dir) {
        eprintln!("track-b: {message}");
        std::process::exit(1);
    }
}

enum Op {
    And,
    Or,
    Not,
}

enum Query {
    Bool { first: u32, rest: Vec<(Op, u32)> },
    /// Index into the distinct phrases.
    Phrase(u32),
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
    let mut phrases = Vec::new();
    let queries = parse_queries(queries_txt, &mut terms, &mut phrases)?;
    terms.finish();
    let plan = Plan::new(docs, &terms, &phrases)?;
    let index = Index::build(docs, &terms, &plan, &phrases, threads)?;

    // Answer in parallel, in blocks of queries, keeping each block's lines in order.
    const BLOCK: usize = 64;
    let blocks = queries.len().div_ceil(BLOCK);
    let next = AtomicUsize::new(0);
    let mut answered: Vec<(usize, Vec<u8>)> = thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let b = next.fetch_add(1, Relaxed);
                        if b >= blocks {
                            break done;
                        }
                        let mut out = Vec::new();
                        for query in &queries[b * BLOCK..((b + 1) * BLOCK).min(queries.len())] {
                            match query {
                                Query::Bool { first, rest } => index.run_bool(*first, rest).write(&mut out),
                                Query::Phrase(p) => View::List(index.phrase(*p)).write(&mut out),
                            }
                        }
                        done.push((b, out));
                    }
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    answered.sort_unstable_by_key(|&(b, _)| b);

    let output_path = format!("{out_dir}/results.txt");
    let mut output = File::create(&output_path).map_err(|e| format!("create {output_path}: {e}"))?;
    for (_, lines) in &answered {
        output.write_all(lines).map_err(|e| format!("write {output_path}: {e}"))?;
    }
    Ok(())
}

fn parse_queries(text: &'static [u8], terms: &mut Terms, phrases: &mut Vec<Vec<u32>>) -> Result<Vec<Query>, String> {
    let mut phrase_ids: HashMap<Vec<u32>, u32> = HashMap::new();
    let mut queries = Vec::new();
    for line in text.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let bad = || format!("bad query {:?}", String::from_utf8_lossy(line));
        if let Some(rest) = line.strip_prefix(b"BOOL ") {
            let mut words = rest.split(|&b| b == b' ');
            let first = terms.intern(words.next().ok_or_else(bad)?, Use::Bool);
            let mut ops = Vec::new();
            while let Some(op) = words.next() {
                let op = match op {
                    b"AND" => Op::And,
                    b"OR" => Op::Or,
                    b"NOT" => Op::Not,
                    _ => return Err(bad()),
                };
                ops.push((op, terms.intern(words.next().ok_or_else(bad)?, Use::Bool)));
            }
            queries.push(Query::Bool { first, rest: ops });
        } else if let Some(rest) = line.strip_prefix(b"PHRASE \"").and_then(|r| r.strip_suffix(b"\"")) {
            let phrase: Vec<u32> = rest.split(|&b| b == b' ').map(|t| terms.intern(t, Use::Phrase)).collect();
            let id = *phrase_ids.entry(phrase.clone()).or_insert_with(|| {
                phrases.push(phrase);
                phrases.len() as u32 - 1
            });
            queries.push(Query::Phrase(id));
        } else {
            return Err(bad());
        }
    }
    Ok(queries)
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
    /// Whether a BOOL query uses the term, so it needs a posting list.
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

#[derive(Default)]
struct ShortTable {
    keys: Vec<u128>, // 0 = empty
    ids: Vec<u32>,
}

impl ShortTable {
    fn new(entries: impl Iterator<Item = (u128, u32)> + Clone) -> Self {
        let slots = (entries.clone().count() * 2).next_power_of_two().max(16);
        let mut table = ShortTable { keys: vec![0; slots], ids: vec![NOT_A_TERM; slots] };
        for (key, id) in entries {
            let i = table.slot(key);
            table.keys[i] = key;
            table.ids[i] = id;
        }
        table
    }

    fn slot(&self, key: u128) -> usize {
        let mask = self.keys.len() - 1;
        let mut i = (hash16(key) >> 20) as usize & mask;
        while self.keys[i] != key && self.keys[i] != 0 {
            i = (i + 1) & mask;
        }
        i
    }

    fn get(&self, key: u128) -> u32 {
        self.ids[self.slot(key)]
    }
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

/// One thread's tokenizer. Word frequencies are Zipfian, so a small direct-mapped
/// cache of recent token -> term id answers most lookups from L1/L2.
struct Tokenizer<'a> {
    terms: &'a Terms,
    hot_keys: Vec<u128>,
    hot_ids: Vec<u32>,
}

const HOT_SLOTS: usize = 1 << 13;

impl<'a> Tokenizer<'a> {
    fn new(terms: &'a Terms) -> Self {
        Tokenizer { terms, hot_keys: vec![0; HOT_SLOTS], hot_ids: vec![0; HOT_SLOTS] }
    }

    fn lookup(&mut self, key: u128) -> u32 {
        let i = hash16(key) as usize & (HOT_SLOTS - 1);
        if self.hot_keys[i] != key {
            self.hot_keys[i] = key;
            self.hot_ids[i] = self.terms.short.get(key);
        }
        self.hot_ids[i]
    }

    /// Calls `f` with each token of `text` (ASCII-lowercased, maximal runs of
    /// [a-z0-9]), mapped to its query-term id or NOT_A_TERM.
    fn for_each_token(&mut self, text: &[u8], mut f: impl FnMut(u32)) {
        let mut buf = [0u8; MAX_TERM];
        let mut len = 0usize; // length of the current token; it may exceed MAX_TERM
        for &byte in text.iter().chain(std::iter::once(&b' ')) {
            let lower = byte | ((byte.is_ascii_uppercase() as u8) << 5);
            if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
                if len < MAX_TERM {
                    buf[len] = lower;
                }
                len += 1;
            } else if len > 0 {
                f(match len {
                    ..=16 => self.lookup(key16(&buf[..len])),
                    ..=MAX_TERM => self.terms.get(&buf[..len]).unwrap_or(NOT_A_TERM),
                    _ => NOT_A_TERM,
                });
                len = 0;
            }
        }
    }
}

/// How the scan treats each query term, decided from a sample before it starts.
struct Plan {
    /// Per term: NOT_BOOL, SPARSE (a posting list), or the index of its bitset.
    storage: Vec<u32>,
    dense_terms: usize,
    /// Documents in the corpus: the last line's id + 1 (ids are dense and in order).
    docs: usize,
    /// Phrases anchored on term t: `anchored[anchor_start[t]..anchor_start[t + 1]]`,
    /// as (phrase, position of t in the phrase).
    anchor_start: Vec<u32>,
    anchored: Vec<(u32, u32)>,
}

const NOT_BOOL: u32 = u32::MAX;
const SPARSE: u32 = u32::MAX - 1;
const SAMPLE_DOCS: u32 = 1000;

impl Plan {
    fn new(docs: &[u8], terms: &Terms, phrases: &[Vec<u32>]) -> Result<Self, String> {
        // Document frequency of every query term in the first SAMPLE_DOCS documents.
        // Documents are drawn independently, so the head of the file is a fair sample.
        let term_count = terms.names.len();
        let mut df = vec![0u32; term_count];
        let mut seen_in = vec![u32::MAX; term_count];
        let mut tokenizer = Tokenizer::new(terms);
        let (mut sampled, mut i) = (0u32, 0);
        while i < docs.len() && sampled < SAMPLE_DOCS {
            let (text, end) = document(docs, i)?;
            tokenizer.for_each_token(text, |t| {
                if t != NOT_A_TERM && seen_in[t as usize] != sampled {
                    seen_in[t as usize] = sampled;
                    df[t as usize] += 1;
                }
            });
            sampled += 1;
            i = end + 1;
        }

        let mut dense_terms = 0;
        let storage = (0..term_count)
            .map(|t| match terms.in_bool[t] {
                false => NOT_BOOL,
                true if df[t] * 32 > sampled => {
                    dense_terms += 1;
                    dense_terms as u32 - 1
                }
                true => SPARSE,
            })
            .collect();

        // Anchor each phrase on its rarest term in the sample (the first, on a tie).
        let mut by_term: Vec<(u32, u32, u32)> = phrases
            .iter()
            .enumerate()
            .map(|(p, phrase)| {
                let (at, &term) = phrase.iter().enumerate().min_by_key(|&(_, &t)| df[t as usize]).unwrap();
                (term, p as u32, at as u32)
            })
            .collect();
        by_term.sort_unstable();
        let mut anchor_start = vec![0u32; term_count + 1];
        for &(term, _, _) in &by_term {
            anchor_start[term as usize + 1] += 1;
        }
        for t in 0..term_count {
            anchor_start[t + 1] += anchor_start[t];
        }
        let anchored = by_term.into_iter().map(|(_, p, at)| (p, at)).collect();
        Ok(Plan { storage, dense_terms, docs: doc_count(docs)?, anchor_start, anchored })
    }

    fn anchored(&self, term: u32) -> &[(u32, u32)] {
        &self.anchored[self.anchor_start[term as usize] as usize..self.anchor_start[term as usize + 1] as usize]
    }
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

struct Index {
    storage: Vec<u32>,
    /// Posting list of a sparse term t: `postings[start[t]..start[t + 1]]`, ascending.
    start: Vec<u32>,
    postings: Vec<u32>,
    /// Bitset of dense term d: `bits[d * words..(d + 1) * words]`, bit i = doc i.
    bits: Vec<u64>,
    words: usize,
    /// Matching documents of phrase p: `phrase_docs[phrase_start[p]..phrase_start[p + 1]]`.
    phrase_start: Vec<u32>,
    phrase_docs: Vec<u32>,
}

/// One thread's scan of its slice of documents.
struct Scan {
    /// `term << 32 | doc` for sparse BOOL terms, once per document.
    pairs: Vec<u64>,
    /// How many pairs each term has in this slice.
    counts: Vec<u32>,
    /// `phrase << 32 | doc`, once per document.
    phrase_hits: Vec<u64>,
}

impl Index {
    fn build(docs: &'static [u8], terms: &Terms, plan: &Plan, phrases: &[Vec<u32>], threads: usize) -> Result<Self, String> {
        let term_count = terms.names.len();
        let words = plan.docs.div_ceil(64);
        let mut bits = vec![0u64; plan.dense_terms * words];
        // SAFETY: AtomicU64 has the size, alignment and bit validity of u64, and the
        // atomic view is dropped before `bits` is used again.
        let shared_bits: &[AtomicU64] = unsafe { std::slice::from_raw_parts(bits.as_mut_ptr() as *const AtomicU64, bits.len()) };
        let slices = split_at_lines(docs, threads);
        let scans: Vec<Scan> = thread::scope(|s| {
            let handles: Vec<_> = slices
                .iter()
                .map(|&slice| s.spawn(move || scan(slice, terms, plan, phrases, shared_bits, words)))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, String>>()
        })?;

        // Counting sort by term, in parallel: every (term, slice) pair gets its own
        // range, and slices are in doc order, so every posting list comes out sorted.
        let mut start = vec![0u32; term_count + 1];
        let mut cursors: Vec<Vec<u32>> = vec![vec![0; term_count]; scans.len()];
        let mut total = 0u32;
        for t in 0..term_count {
            start[t] = total;
            for (scan, cursor) in scans.iter().zip(cursors.iter_mut()) {
                cursor[t] = total;
                total += scan.counts[t];
            }
        }
        start[term_count] = total;
        let mut postings = vec![0u32; total as usize];
        let target = SharedSlice(postings.as_mut_ptr());
        thread::scope(|s| {
            for (scan, cursor) in scans.iter().zip(cursors.iter_mut()) {
                let target = &target;
                s.spawn(move || {
                    for &pair in &scan.pairs {
                        let slot = &mut cursor[(pair >> 32) as usize];
                        // SAFETY: the loop above gave each (term, slice) a disjoint
                        // range of `postings`, sized by this slice's own counts.
                        unsafe { *target.0.add(*slot as usize) = pair as u32 };
                        *slot += 1;
                    }
                });
            }
        });

        // Phrase hits are few; sort them by (phrase, doc) and cut them into lists.
        let mut hits: Vec<u64> = scans.iter().flat_map(|scan| scan.phrase_hits.iter().copied()).collect();
        hits.sort_unstable();
        let mut phrase_start = vec![0u32; phrases.len() + 1];
        for &hit in &hits {
            phrase_start[(hit >> 32) as usize + 1] += 1;
        }
        for p in 0..phrases.len() {
            phrase_start[p + 1] += phrase_start[p];
        }
        let phrase_docs = hits.iter().map(|&hit| hit as u32).collect();
        Ok(Index { storage: plan.storage.clone(), start, postings, bits, words, phrase_start, phrase_docs })
    }

    fn view(&self, term: u32) -> View<'_> {
        match self.storage[term as usize] {
            SPARSE | NOT_BOOL => View::List(&self.postings[self.start[term as usize] as usize..self.start[term as usize + 1] as usize]),
            d => View::Bits(&self.bits[d as usize * self.words..(d as usize + 1) * self.words]),
        }
    }

    fn phrase(&self, p: u32) -> &[u32] {
        &self.phrase_docs[self.phrase_start[p as usize] as usize..self.phrase_start[p as usize + 1] as usize]
    }

    /// Applies the operators strictly left to right.
    fn run_bool(&self, first: u32, rest: &[(Op, u32)]) -> Set {
        let mut result = Set::from(self.view(first));
        for (op, term) in rest {
            result = apply(op, result.view(), self.view(*term));
        }
        result
    }
}

fn scan(slice: &[u8], terms: &Terms, plan: &Plan, phrases: &[Vec<u32>], bits: &[AtomicU64], words: usize) -> Result<Scan, String> {
    let mut out = Scan { pairs: Vec::new(), counts: vec![0; terms.names.len()], phrase_hits: Vec::new() };
    let mut tokenizer = Tokenizer::new(terms);
    // The last document that emitted each term and each phrase, to emit once per doc.
    let mut term_doc = vec![u32::MAX; terms.names.len()];
    let mut phrase_doc = vec![u32::MAX; phrases.len()];
    // This document's term ids, and the positions of anchor terms among them.
    let (mut tokens, mut anchors) = (Vec::new(), Vec::new());
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
        tokenizer.for_each_token(text, |t| {
            if t != NOT_A_TERM {
                let storage = plan.storage[t as usize];
                if storage != NOT_BOOL && term_doc[t as usize] != doc {
                    term_doc[t as usize] = doc;
                    if storage == SPARSE {
                        out.pairs.push((t as u64) << 32 | doc as u64);
                        out.counts[t as usize] += 1;
                    } else {
                        bits[storage as usize * words + doc as usize / 64].fetch_or(1 << (doc % 64), Relaxed);
                    }
                }
                if plan.anchor_start[t as usize] != plan.anchor_start[t as usize + 1] {
                    anchors.push(tokens.len() as u32);
                }
            }
            tokens.push(t);
        });
        for &at in &anchors {
            for &(p, offset) in plan.anchored(tokens[at as usize]) {
                let phrase = &phrases[p as usize];
                let Some(from) = (at as usize).checked_sub(offset as usize) else { continue };
                if tokens.get(from..from + phrase.len()) == Some(phrase.as_slice()) && phrase_doc[p as usize] != doc {
                    phrase_doc[p as usize] = doc;
                    out.phrase_hits.push((p as u64) << 32 | doc as u64);
                }
            }
        }
        i = end + 1;
    }
    Ok(out)
}

/// A query operand: a posting list or a bitset, borrowed from the index or a result.
#[derive(Clone, Copy)]
enum View<'a> {
    List(&'a [u32]),
    Bits(&'a [u64]),
}

/// An intermediate BOOL result.
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

    fn write(&self, out: &mut Vec<u8>) {
        self.view().write(out)
    }
}

fn has(bits: &[u64], doc: u32) -> bool {
    bits[doc as usize / 64] >> (doc % 64) & 1 == 1
}

/// One operator on two operands. A list stays a list where the answer can only
/// shrink it (AND, and NOT with a list on the left); otherwise bitsets absorb lists.
fn apply(op: &Op, a: View, b: View) -> Set {
    use View::{Bits, List};
    let words = |x: &[u64], y: &[u64], f: fn(u64, u64) -> u64| Set::Bits(x.iter().zip(y).map(|(&p, &q)| f(p, q)).collect());
    match (op, a, b) {
        (Op::And, List(x), List(y)) => Set::List(intersect(x, y)),
        (Op::And, List(x), Bits(y)) | (Op::And, Bits(y), List(x)) => Set::List(x.iter().copied().filter(|&d| has(y, d)).collect()),
        (Op::And, Bits(x), Bits(y)) => words(x, y, |p, q| p & q),
        (Op::Or, List(x), List(y)) => Set::List(union(x, y)),
        (Op::Or, List(x), Bits(y)) | (Op::Or, Bits(y), List(x)) => {
            let mut bits = y.to_vec();
            for &d in x {
                bits[d as usize / 64] |= 1 << (d % 64);
            }
            Set::Bits(bits)
        }
        (Op::Or, Bits(x), Bits(y)) => words(x, y, |p, q| p | q),
        (Op::Not, List(x), List(y)) => Set::List(difference(x, y)),
        (Op::Not, List(x), Bits(y)) => Set::List(x.iter().copied().filter(|&d| !has(y, d)).collect()),
        (Op::Not, Bits(x), List(y)) => {
            let mut bits = x.to_vec();
            for &d in y {
                bits[d as usize / 64] &= !(1 << (d % 64));
            }
            Set::Bits(bits)
        }
        (Op::Not, Bits(x), Bits(y)) => words(x, y, |p, q| p & !q),
    }
}

impl View<'_> {
    /// "count sum id_1 ... id_m" with the m = min(10, count) smallest ids.
    fn write(&self, out: &mut Vec<u8>) {
        let (mut count, mut sum, mut first) = (0u64, 0u64, Vec::with_capacity(10));
        match *self {
            View::List(list) => {
                count = list.len() as u64;
                sum = list.iter().map(|&d| d as u64).sum();
                first.extend(list.iter().take(10).map(|&d| d as u64));
            }
            View::Bits(bits) => {
                for (w, &word) in bits.iter().enumerate() {
                    let mut rest = word;
                    while rest != 0 {
                        let doc = (w * 64) as u64 + rest.trailing_zeros() as u64;
                        count += 1;
                        sum += doc;
                        if first.len() < 10 {
                            first.push(doc);
                        }
                        rest &= rest - 1;
                    }
                }
            }
        }
        push_decimal(out, count);
        out.push(b' ');
        push_decimal(out, sum);
        for doc in first {
            out.push(b' ');
            push_decimal(out, doc);
        }
        out.push(b'\n');
    }
}

struct SharedSlice(*mut u32);
// SAFETY: only used for the disjoint scatter writes in Index::build.
unsafe impl Sync for SharedSlice {}

fn parse_doc_id(digits: &[u8]) -> Result<u32, String> {
    if digits.is_empty() || digits.len() > 9 || !digits.iter().all(u8::is_ascii_digit) {
        return Err(format!("docs.txt: bad id {:?}", String::from_utf8_lossy(digits)));
    }
    Ok(digits.iter().fold(0, |acc, &b| acc * 10 + (b - b'0') as u32))
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
