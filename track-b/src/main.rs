//! Track B (Index): answer 50,000 BOOL and PHRASE queries over a 254 MB corpus.
//!
//! How it is fast:
//! 1. The queries come first. Only terms that some query uses are indexed, so the
//!    term table is small enough to stay in cache, and every other token is just a
//!    failed lookup.
//! 2. The corpus is mmap'd and scanned once, in parallel slices of whole documents.
//!    Each thread emits (term, doc) pairs, once per document; a counting sort by
//!    term turns them into posting lists that are already sorted by doc id,
//!    because slices are in doc order.
//! 3. No positions are stored. A phrase's candidates are the intersection of its
//!    terms' posting lists (shortest first, usually a rare anchor), and each
//!    candidate's text is re-tokenized from the mmap to check adjacency.
//! 4. BOOL queries run strictly left to right over sorted doc-id lists, as the spec
//!    requires; only count, sum and the ten smallest ids are printed.

mod mmap;

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
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
    Phrase(Vec<u32>),
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
    let queries = parse_queries(queries_txt, &mut terms)?;
    let index = Index::build(docs, &terms, threads)?;

    // Answer in parallel, in blocks of queries, keeping each block's lines in order.
    const BLOCK: usize = 64;
    let blocks = queries.len().div_ceil(BLOCK);
    let next = AtomicUsize::new(0);
    let mut answered: Vec<(usize, Vec<u8>)> = thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut done = Vec::new();
                    let mut tokens = Vec::new();
                    loop {
                        let b = next.fetch_add(1, Relaxed);
                        if b >= blocks {
                            break done;
                        }
                        let mut out = Vec::new();
                        for query in &queries[b * BLOCK..((b + 1) * BLOCK).min(queries.len())] {
                            let matches = match query {
                                Query::Bool { first, rest } => index.run_bool(*first, rest),
                                Query::Phrase(phrase) => index.run_phrase(phrase, &terms, &mut tokens),
                            };
                            write_result(&mut out, &matches);
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

fn parse_queries(text: &'static [u8], terms: &mut Terms) -> Result<Vec<Query>, String> {
    let mut queries = Vec::new();
    for line in text.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let bad = || format!("bad query {:?}", String::from_utf8_lossy(line));
        if let Some(rest) = line.strip_prefix(b"BOOL ") {
            let mut words = rest.split(|&b| b == b' ');
            let first = terms.intern(words.next().ok_or_else(bad)?);
            let mut ops = Vec::new();
            while let Some(op) = words.next() {
                let op = match op {
                    b"AND" => Op::And,
                    b"OR" => Op::Or,
                    b"NOT" => Op::Not,
                    _ => return Err(bad()),
                };
                ops.push((op, terms.intern(words.next().ok_or_else(bad)?)));
            }
            queries.push(Query::Bool { first, rest: ops });
        } else if let Some(rest) = line.strip_prefix(b"PHRASE \"").and_then(|r| r.strip_suffix(b"\"")) {
            queries.push(Query::Phrase(rest.split(|&b| b == b' ').map(|t| terms.intern(t)).collect()));
        } else {
            return Err(bad());
        }
    }
    Ok(queries)
}

/// The set of query terms: an open-addressing table over slices of queries.txt.
struct Terms {
    slots: Vec<u32>, // 0 = empty, else id + 1
    names: Vec<&'static [u8]>,
}

impl Terms {
    fn new() -> Self {
        Terms { slots: vec![0; 1 << 16], names: Vec::new() }
    }

    fn intern(&mut self, term: &'static [u8]) -> u32 {
        if let Some(id) = self.get(term) {
            return id;
        }
        self.names.push(term);
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

struct Index {
    /// Posting list of term t: `postings[start[t]..start[t + 1]]`, ascending doc ids.
    start: Vec<u32>,
    postings: Vec<u32>,
    /// Byte range of each document's text in docs.txt.
    text: Vec<(u32, u32)>,
    docs: &'static [u8],
}

impl Index {
    fn build(docs: &'static [u8], terms: &Terms, threads: usize) -> Result<Self, String> {
        let term_count = terms.names.len();
        // Scan, in parallel: per slice, the (term, doc) pairs and each doc's text range.
        let slices = split_at_lines(docs, threads);
        let scans: Vec<(Vec<u64>, Vec<(u32, u32)>)> = thread::scope(|s| {
            let handles: Vec<_> = slices
                .iter()
                .map(|&slice| {
                    s.spawn(move || -> Result<_, String> {
                        let base = slice.as_ptr() as usize - docs.as_ptr() as usize;
                        let mut pairs = Vec::new();
                        let mut ranges = Vec::new();
                        let mut last_doc = vec![u32::MAX; term_count];
                        let mut i = 0;
                        while i < slice.len() {
                            let tab = i + slice[i..].iter().position(|&b| b == b'\t').ok_or("docs.txt: a line without a TAB")?;
                            let end = tab + slice[tab..].iter().position(|&b| b == b'\n').ok_or("docs.txt: no final LF")?;
                            let doc = parse_doc_id(&slice[i..tab])?;
                            for_each_token(&slice[tab + 1..end], terms, |t| {
                                if t != NOT_A_TERM && last_doc[t as usize] != doc {
                                    last_doc[t as usize] = doc;
                                    pairs.push((t as u64) << 32 | doc as u64);
                                }
                            });
                            ranges.push(((base + tab + 1) as u32, (base + end) as u32));
                            i = end + 1;
                        }
                        Ok((pairs, ranges))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, String>>()
        })?;

        // Counting sort of the pairs by term. Slices are in doc order and each slice
        // emits its docs in order, so every posting list comes out ascending.
        let mut start = vec![0u32; term_count + 1];
        for (pairs, _) in &scans {
            for &pair in pairs {
                start[(pair >> 32) as usize + 1] += 1;
            }
        }
        for t in 0..term_count {
            start[t + 1] += start[t];
        }
        let mut cursor = start.clone();
        let mut postings = vec![0u32; start[term_count] as usize];
        let mut text = Vec::new();
        for (pairs, ranges) in scans {
            for pair in pairs {
                let slot = &mut cursor[(pair >> 32) as usize];
                postings[*slot as usize] = pair as u32;
                *slot += 1;
            }
            text.extend(ranges);
        }
        Ok(Index { start, postings, text, docs })
    }

    fn list(&self, term: u32) -> &[u32] {
        &self.postings[self.start[term as usize] as usize..self.start[term as usize + 1] as usize]
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

    fn run_phrase(&self, phrase: &[u32], terms: &Terms, tokens: &mut Vec<u32>) -> Vec<u32> {
        let mut lists: Vec<&[u32]> = phrase.iter().map(|&t| self.list(t)).collect();
        lists.sort_unstable_by_key(|l| l.len());
        let mut candidates = lists[0].to_vec();
        for list in &lists[1..] {
            if candidates.is_empty() {
                break;
            }
            candidates = intersect(&candidates, list);
        }
        candidates.retain(|&doc| {
            let (from, to) = self.text[doc as usize];
            tokens.clear();
            for_each_token(&self.docs[from as usize..to as usize], terms, |t| tokens.push(t));
            tokens.windows(phrase.len()).any(|w| w == phrase)
        });
        candidates
    }
}

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
