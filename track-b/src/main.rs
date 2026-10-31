//! Track B (Index): answer 50,000 BOOL and PHRASE queries over a 254 MB corpus.
//!
//! How it is fast:
//! 1. The queries come first. Only terms that some query uses are looked up, so
//!    the term table is small enough to stay in cache, and every other token is
//!    just a failed lookup.
//! 2. The corpus is mmap'd and scanned once, in parallel slices of whole documents.
//!    For BOOL terms each thread emits (term, doc) pairs, once per document; a
//!    parallel counting sort turns them into posting lists already sorted by doc id,
//!    because slices are in doc order.
//! 3. Phrases need no positions and no second look at the text: all distinct
//!    phrases are compiled into one Aho-Corasick automaton over term ids, and each
//!    document's token stream runs through it during the same scan. A phrase is
//!    reported when its last term arrives, so a phrase of common words such as
//!    "the the" costs nothing extra.
//! 4. BOOL queries run strictly left to right over sorted doc-id lists, as the spec
//!    requires; only count, sum and the ten smallest ids are printed.

mod mmap;

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::thread;

/// Query terms are 1 to 32 bytes; a longer corpus token can never match one.
const MAX_TERM: usize = 32;
const NOT_A_TERM: u32 = u32::MAX;
const NONE: u32 = u32::MAX;

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
    let automaton = Automaton::build(&phrases);
    let index = Index::build(docs, &terms, &automaton, phrases.len(), threads)?;

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
                                Query::Bool { first, rest } => write_result(&mut out, &index.run_bool(*first, rest)),
                                Query::Phrase(p) => write_result(&mut out, index.phrase(*p)),
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

/// The query terms: an open-addressing table over slices of queries.txt.
struct Terms {
    slots: Vec<u32>, // 0 = empty, else id + 1
    names: Vec<&'static [u8]>,
    /// Whether a BOOL query uses the term, so it needs a posting list.
    in_bool: Vec<bool>,
}

impl Terms {
    fn new() -> Self {
        Terms { slots: vec![0; 1 << 16], names: Vec::new(), in_bool: Vec::new() }
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

/// Calls `f` with each token of `text` (ASCII-lowercased, maximal runs of
/// [a-z0-9]), mapped to its query-term id or NOT_A_TERM.
fn for_each_token(text: &[u8], terms: &Terms, mut f: impl FnMut(u32)) {
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
            f(if len <= MAX_TERM { terms.get(&buf[..len]).unwrap_or(NOT_A_TERM) } else { NOT_A_TERM });
            len = 0;
        }
    }
}

/// Aho-Corasick over term ids: a trie of all distinct phrases, with failure links.
/// State 0 is the root. A non-query token can be in no phrase, so it resets to 0.
struct Automaton {
    /// (state << 32 | term) -> next state, open addressing; key u64::MAX = empty.
    keys: Vec<u64>,
    values: Vec<u32>,
    fail: Vec<u32>,
    /// The phrase that ends exactly at this state, or NONE.
    phrase_at: Vec<u32>,
    /// The nearest state on the failure chain that ends a phrase, or NONE.
    output_link: Vec<u32>,
}

impl Automaton {
    fn build(phrases: &[Vec<u32>]) -> Self {
        let edges: usize = phrases.iter().map(Vec::len).sum();
        let slots = (edges * 2).next_power_of_two().max(16);
        let mut a = Automaton { keys: vec![u64::MAX; slots], values: vec![0; slots], fail: vec![0], phrase_at: vec![NONE], output_link: vec![NONE] };
        let mut children: Vec<Vec<(u32, u32)>> = vec![Vec::new()];
        for (p, phrase) in phrases.iter().enumerate() {
            let mut state = 0;
            for &term in phrase {
                state = match a.goto(state, term) {
                    Some(next) => next,
                    None => {
                        let next = a.fail.len() as u32;
                        a.fail.push(0);
                        a.phrase_at.push(NONE);
                        a.output_link.push(NONE);
                        children.push(Vec::new());
                        a.insert(state, term, next);
                        children[state as usize].push((term, next));
                        next
                    }
                };
            }
            a.phrase_at[state as usize] = p as u32;
        }
        // Breadth-first, so every failure target is finished before it is used.
        let mut queue: Vec<u32> = children[0].iter().map(|&(_, child)| child).collect();
        let mut head = 0;
        while head < queue.len() {
            let u = queue[head];
            head += 1;
            for &(term, v) in &children[u as usize] {
                let mut f = a.fail[u as usize];
                let target = loop {
                    if let Some(next) = a.goto(f, term) {
                        break next;
                    }
                    if f == 0 {
                        break 0;
                    }
                    f = a.fail[f as usize];
                };
                a.fail[v as usize] = target;
                a.output_link[v as usize] =
                    if a.phrase_at[target as usize] != NONE { target } else { a.output_link[target as usize] };
                queue.push(v);
            }
        }
        a
    }

    fn slot(&self, key: u64) -> usize {
        let mask = self.keys.len() - 1;
        let mut i = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & mask;
        while self.keys[i] != key && self.keys[i] != u64::MAX {
            i = (i + 1) & mask;
        }
        i
    }

    fn insert(&mut self, state: u32, term: u32, next: u32) {
        let key = (state as u64) << 32 | term as u64;
        let i = self.slot(key);
        self.keys[i] = key;
        self.values[i] = next;
    }

    fn goto(&self, state: u32, term: u32) -> Option<u32> {
        let i = self.slot((state as u64) << 32 | term as u64);
        (self.keys[i] != u64::MAX).then(|| self.values[i])
    }

    fn step(&self, mut state: u32, term: u32) -> u32 {
        if term == NOT_A_TERM {
            return 0;
        }
        loop {
            if let Some(next) = self.goto(state, term) {
                return next;
            }
            if state == 0 {
                return 0;
            }
            state = self.fail[state as usize];
        }
    }
}

struct Index {
    /// Posting list of term t: `postings[start[t]..start[t + 1]]`, ascending doc ids.
    start: Vec<u32>,
    postings: Vec<u32>,
    /// Matching documents of phrase p: `phrase_docs[phrase_start[p]..phrase_start[p + 1]]`.
    phrase_start: Vec<u32>,
    phrase_docs: Vec<u32>,
}

/// One thread's scan of its slice of documents.
struct Scan {
    /// `term << 32 | doc`, for BOOL terms, once per document.
    pairs: Vec<u64>,
    /// How many pairs each term has in this slice.
    counts: Vec<u32>,
    /// `phrase << 32 | doc`, once per document.
    phrase_hits: Vec<u64>,
}

impl Index {
    fn build(docs: &'static [u8], terms: &Terms, automaton: &Automaton, phrase_count: usize, threads: usize) -> Result<Self, String> {
        let term_count = terms.names.len();
        let slices = split_at_lines(docs, threads);
        let scans: Vec<Scan> = thread::scope(|s| {
            let handles: Vec<_> = slices
                .iter()
                .map(|&slice| s.spawn(move || scan(slice, terms, automaton, phrase_count)))
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
        let mut phrase_start = vec![0u32; phrase_count + 1];
        for &hit in &hits {
            phrase_start[(hit >> 32) as usize + 1] += 1;
        }
        for p in 0..phrase_count {
            phrase_start[p + 1] += phrase_start[p];
        }
        let phrase_docs = hits.iter().map(|&hit| hit as u32).collect();
        Ok(Index { start, postings, phrase_start, phrase_docs })
    }

    fn list(&self, term: u32) -> &[u32] {
        &self.postings[self.start[term as usize] as usize..self.start[term as usize + 1] as usize]
    }

    fn phrase(&self, p: u32) -> &[u32] {
        &self.phrase_docs[self.phrase_start[p as usize] as usize..self.phrase_start[p as usize + 1] as usize]
    }

    fn run_bool(&self, first: u32, rest: &[(Op, u32)]) -> Vec<u32> {
        let mut result = self.list(first).to_vec();
        for (op, term) in rest {
            let other = self.list(*term);
            result = match op {
                Op::And => intersect(&result, other),
                Op::Or => union(&result, other),
                Op::Not => difference(&result, other),
            };
        }
        result
    }
}

fn scan(slice: &[u8], terms: &Terms, automaton: &Automaton, phrase_count: usize) -> Result<Scan, String> {
    let mut out = Scan { pairs: Vec::new(), counts: vec![0; terms.names.len()], phrase_hits: Vec::new() };
    // The last document that emitted each term and each phrase, to emit once per doc.
    let mut term_doc = vec![u32::MAX; terms.names.len()];
    let mut phrase_doc = vec![u32::MAX; phrase_count];
    let mut i = 0;
    while i < slice.len() {
        let tab = i + slice[i..].iter().position(|&b| b == b'\t').ok_or("docs.txt: a line without a TAB")?;
        let end = tab + slice[tab..].iter().position(|&b| b == b'\n').ok_or("docs.txt: no final LF")?;
        let doc = parse_doc_id(&slice[i..tab])?;
        let mut state = 0;
        for_each_token(&slice[tab + 1..end], terms, |t| {
            if t != NOT_A_TERM && terms.in_bool[t as usize] && term_doc[t as usize] != doc {
                term_doc[t as usize] = doc;
                out.pairs.push((t as u64) << 32 | doc as u64);
                out.counts[t as usize] += 1;
            }
            state = automaton.step(state, t);
            // Report every phrase that ends here: this state's own, then the
            // shorter ones along the output links.
            let mut s = if automaton.phrase_at[state as usize] != NONE { state } else { automaton.output_link[state as usize] };
            while s != NONE {
                let p = automaton.phrase_at[s as usize] as usize;
                if phrase_doc[p] != doc {
                    phrase_doc[p] = doc;
                    out.phrase_hits.push((p as u64) << 32 | doc as u64);
                }
                s = automaton.output_link[s as usize];
            }
        });
        i = end + 1;
    }
    Ok(out)
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

/// "count sum id_1 ... id_m" with the m = min(10, count) smallest ids.
fn write_result(out: &mut Vec<u8>, matches: &[u32]) {
    push_decimal(out, matches.len() as u64);
    out.push(b' ');
    push_decimal(out, matches.iter().map(|&d| d as u64).sum());
    for &doc in matches.iter().take(10) {
        out.push(b' ');
        push_decimal(out, doc as u64);
    }
    out.push(b'\n');
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
