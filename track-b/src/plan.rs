//! The plan: what the scan does with each query, decided from the sample (BOOL
//! entries triggered by cover terms or evaluated per superblock, phrases by
//! anchor, one answer per line).

use super::*;

/// A BOOL query evaluated in every superblock.
#[derive(Clone, Copy, Default)]
pub struct Entry {
    /// `row | op << 30` per term, then zeros: `end_super` reads only the first `len`.
    pub terms: [u32; MAX_BOOL_TERMS],
    /// The answer whose state the evaluation updates.
    pub answer: u32,
    /// The number of terms.
    pub len: u32,
}

/// A BOOL query as triggered, per document, by one of its cover terms c: the
/// query's other distinct terms, padded with term 0, its answer and its truth
/// table over their presence (c being present). The table is zero where a cover
/// term before c is present: that term's entry counts the document. Four u32
/// words, 16 bytes (repr(C, align(16))): `trigger` gathers word 4i + k of the
/// entries as word k of entry i (terms 0 and 1, terms 2 and 3, term 4 and the
/// answer, the truth table), so this layout is part of its bounds argument; 16
/// bytes also keep each entry in one cache line.
#[derive(Clone, Copy, Default)]
#[repr(C, align(16))]
pub struct DocEntry {
    terms: [u16; MAX_BOOL_TERMS - 1],
    pub answer: u16,
    /// Bit a: whether the query matches when the terms of the set bits of a are
    /// present and the others absent.
    truth: u32,
}

// The layout that `trigger` relies on (see above), checked.
const _: () = assert!(size_of::<DocEntry>() == 16 && std::mem::offset_of!(DocEntry, truth) == 12);

/// The entry of the BOOL query `words` triggered by its cover term `c`, with
/// `earlier` the cover terms before c.
fn doc_entry(words: &[u32], c: u32, earlier: &[u32], answer: u32) -> DocEntry {
    let (mut terms, mut k) = ([0; MAX_BOOL_TERMS - 1], 0);
    for t in words.iter().map(|&w| w & TERM) {
        if t != c && !terms[..k].contains(&(t as u16)) {
            (terms[k], k) = (t as u16, k + 1);
        }
    }
    // A term's presence in all 32 assignments at once, bit a for assignment a, so
    // the query is evaluated on all of them in one pass: c, the one word not in
    // `terms`, is in every document. (The table's bits from 2^k on are a copy of
    // the first 2^k, so whether the padding term is present does not matter.)
    let index = |t: u32| terms[..k].iter().position(|&o| o as u32 == t);
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
        answer: answer as u16,
        truth: earlier.iter().fold(r, |r, &e| r & !present(e)),
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
pub struct Phrase {
    pub terms: [u32; 8],
    pub answer: u32,
    pub offset: u32,
    pub len: u32,
}

/// What the scan does with each query, decided from the sample.
#[derive(Default)]
pub struct Plan {
    /// BOOL entries triggered by term t: `entries[group[t]..group[t + 1]]`; then 8
    /// padding entries that never match (truth 0), for `trigger`'s spare lanes.
    pub group: Vec<u16>,
    pub entries: Vec<DocEntry>,
    /// BOOL entries evaluated in every superblock, on rows of `Scan::wide`.
    pub always: Vec<Entry>,
    /// The terms of `always` have the ids 0..rows, which are also their rows.
    pub rows: usize,
    /// Phrases anchored on term t: `anchored[anchor[t]..anchor[t + 1]]`.
    pub anchor: Vec<u32>,
    pub anchored: Vec<Phrase>,
    /// The info word of each term, by its old id: its new id, ANCHOR and TRIG.
    pub info: Vec<u32>,
    /// Each line's answer, the index of its state in each thread, and back (one
    /// answer per line).
    pub answer_of: Vec<u32>,
    pub line_of: Vec<u32>,
}

impl Plan {
    /// `sample_df` counts the documents of each term in a sample of `sampled`
    /// documents (at least 1). The per-line work runs on one thread per part of
    /// the queries. DocEntry and `group` keep term ids, answers and entry indices in
    /// 16 bits: the spec's queries have about 53,000 BOOL terms, 50,000 lines and
    /// 36,000 entries. Past 2^16 of any of them, every BOOL query is evaluated in
    /// every superblock instead (slower, never wrong).
    pub fn new(queries: Queries, sample_df: &[u32], sampled: u32) -> Self {
        let plan = |force| Self::with(&queries, sample_df, sampled, force);
        plan(false).unwrap_or_else(|| plan(true).unwrap())
    }

    /// The plan, if it fits DocEntry and `group` (see `new`), or with `force` every
    /// BOOL query evaluated in every superblock.
    fn with(queries: &Queries, sample_df: &[u32], sampled: u32, force: bool) -> Option<Self> {
        let parts = queries.len();
        // The expected share of 64-document blocks that contain t: the cost of
        // triggering on it (plus 1 with `force`: every cover then costs over ALWAYS).
        let share = |t: u32| sample_df[t as usize] as f64 / sampled as f64;
        let cost = |t: u32| force as u8 as f64 + 1.0 - (1.0 - share(t)).powi(64);
        // Per old id: bit 0 for a BOOL term, bit 1 for a term of an `always` query.
        let kind: Vec<AtomicU8> = (0..sample_df.len()).map(|_| AtomicU8::new(0)).collect();
        // Whether each line is a BOOL query evaluated in every superblock.
        let always_lines = par(parts, &|p| classify(&queries[p], cost, &kind)).concat();
        let lines = always_lines.len();
        let kind: Vec<u8> = kind.into_iter().map(AtomicU8::into_inner).collect();
        let (order, rank) = term_order(&kind, sample_df);
        let bool_terms = kind.iter().filter(|&&k| k > 0).count();
        let rows = kind.iter().filter(|&&k| k == 3).count();
        // The costs by new id, so `cover` finds the sets `classify` found.
        let cost = |t: u32| cost(order[t as usize]);
        // The sampled document frequency of a new id: phrases anchor on their rarest term.
        let df = |t: u32| sample_df[order[t as usize] as usize];
        // The number of lines before part p's.
        let first = |p: usize| queries[..p].iter().map(|q| q.0.len()).sum();
        let records = |p| records(&queries[p], first(p), &rank, &df, &cost, &always_lines);
        // Part p's records are out[p]. The entries are grouped by their cover term,
        // the phrases by their anchor.
        let out = par(parts, &records);
        // 8 entries after the last that never match (truth 0), in a group of their
        // own: `trigger` writes up to 7 indices past a range, and pads its last 8
        // with the last of them.
        let pad = [(bool_terms as u32, DocEntry::default()); 8];
        let entry_pairs = || out.iter().flat_map(|o| &o.triggered).copied().chain(pad);
        let anchor = |p: &Phrase| (p.terms[p.offset as usize], *p);
        let anchor_pairs = || out.iter().flat_map(|o| &o.phrases).map(anchor);
        let always: Vec<_> = out.iter().flat_map(|o| o.always.iter().copied()).collect();
        let mut plan = Plan::default();
        // The anchors are grouped and the `always` entries sorted on a second
        // thread, while this one groups the entries.
        let (always, entries, anchors) = (Mutex::new(always), OnceLock::new(), OnceLock::new());
        par(2, &|i| match i {
            0 => _ = entries.set(csr(entry_pairs, bool_terms + 1)),
            _ => {
                // By length and operators, so that `end_super` predicts its branches.
                always.lock().unwrap().sort_unstable_by_key(|e| {
                    let ops = e.terms.iter().fold(e.len, |key, &w| key << 2 | w >> 30);
                    (ops, e.terms[0] & TERM)
                });
                _ = anchors.set(csr(anchor_pairs, order.len()));
            }
        });
        let (group, entries) = entries.into_inner().unwrap();
        // With `force` there are only the padding entries, with terms 0.
        let fits = bool_terms <= 1 << 16 && lines <= 1 << 16 && entries.len() < 1 << 16;
        if !fits && !force {
            return None;
        }
        // In 16 bits, so that `trigger` reads half the lines.
        (plan.group, plan.entries) = (group.iter().map(|&g| g as u16).collect(), entries);
        (plan.anchor, plan.anchored) = anchors.into_inner().unwrap();
        (plan.always, plan.rows) = (always.into_inner().unwrap(), rows);
        plan.number_answers(lines);
        let trig = |id: usize| id < bool_terms && plan.group[id] != plan.group[id + 1];
        let anchoring = |id: usize| plan.anchor[id] != plan.anchor[id + 1];
        let flag = |id| anchoring(id) as u32 * ANCHOR | trig(id) as u32 * TRIG;
        // In parallel, a range of old ids per part.
        let word = |&id: &u32| id | flag(id as usize);
        let ids = |p| &rank[part(rank.len(), p, parts)];
        let info = par(parts, &|p| ids(p).iter().map(word).collect::<Vec<_>>());
        plan.info = info.concat();
        Some(plan)
    }

    /// Numbers the answers in the order the scan visits them: the `always` ones
    /// first, so that each superblock reads and writes their state as one stream
    /// (47 KB instead of 4,000 scattered lines), then those of the entries in
    /// their order, then the phrases. Every line gets a number: it has an entry
    /// (a cover has at least one term), is an `always` one or is a phrase.
    fn number_answers(&mut self, lines: usize) {
        (self.answer_of, self.line_of) = (vec![u32::MAX; lines], Vec::with_capacity(lines));
        let mut number = |line: &mut u32| {
            if self.answer_of[*line as usize] == u32::MAX {
                self.answer_of[*line as usize] = self.line_of.len() as u32;
                self.line_of.push(*line);
            }
            *line = self.answer_of[*line as usize];
        };
        self.always.iter_mut().for_each(|e| number(&mut e.answer));
        for e in &mut self.entries {
            let mut answer = e.answer as u32;
            number(&mut answer);
            e.answer = answer as u16;
        }
        self.anchored.iter_mut().for_each(|a| number(&mut a.answer));
    }
}

/// A part of the queries, as in `Queries`: its lines' spans and their terms.
type Part = (Vec<Span>, Vec<u32>);

/// What the plan takes from a range of the lines (`records`): its per-document
/// BOOL entries, one per cover term, its phrases and its `always` entries.
struct Records {
    /// Each with its cover term.
    triggered: Vec<(u32, DocEntry)>,
    phrases: Vec<Phrase>,
    always: Vec<Entry>,
}

/// The records of a part of the queries (`spans`, `terms`), whose lines are numbered
/// from `first`, their terms renumbered by `rank`. By new id, `df` is a term's
/// sampled document frequency and `cost` its cost (see `cover`); `always_lines`
/// marks the lines evaluated in every superblock.
fn records(
    (spans, terms): &Part,
    first: usize,
    rank: &[u32],
    df: impl Fn(u32) -> u32,
    cost: impl Fn(u32) -> f64,
    always_lines: &[bool],
) -> Records {
    let mut triggered = Vec::with_capacity(MAX_BOOL_TERMS * spans.len());
    let (mut phrases, mut always) = (Vec::with_capacity(spans.len()), Vec::new());
    let renumber = |&word: &u32| rank[(word & TERM) as usize] | word & !TERM;
    let renumbered: Vec<u32> = terms.iter().map(renumber).collect();
    for (line, &(start, len, phrase)) in (first..).zip(spans) {
        let words = &renumbered[start as usize..(start + len) as usize];
        let answer = line as u32; // until number_answers
        if phrase {
            // The first of the rarest terms (a phrase has at least one).
            let offset = (0..words.len()).min_by_key(|&k| df(words[k])).unwrap();
            assert!(words.len() <= 8, "queries.txt: a phrase of over 8 terms");
            phrases.push(Phrase {
                terms: std::array::from_fn(|k| *words.get(k).unwrap_or(&0)),
                answer,
                offset: offset as u32,
                len,
            });
            continue;
        }
        if always_lines[line] {
            // end_super reads only the first len terms: the rest stay 0.
            let terms = std::array::from_fn(|k| *words.get(k).unwrap_or(&0));
            always.push(Entry { terms, answer, len });
            continue;
        }
        let (cover, n, _) = cover(words, &cost);
        for (i, &c) in cover[..n].iter().enumerate() {
            triggered.push((c, doc_entry(words, c, &cover[..i], answer)));
        }
    }
    Records {
        triggered,
        phrases,
        always,
    }
}

/// Marks the kinds of the terms of a part of the queries and returns, per line,
/// whether it is a BOOL query evaluated in every superblock (its cover terms are
/// expected in more than ALWAYS of the blocks, summed). A BOOL line's terms get
/// kind bit 0, and bit 1 too when it is such a query; a phrase's terms get nothing
/// here.
fn classify((spans, terms): &Part, cost: impl Fn(u32) -> f64, kind: &[AtomicU8]) -> Vec<bool> {
    let line = |&(start, len, phrase): &Span| {
        let words = &terms[start as usize..(start + len) as usize];
        let always = !phrase && cover(words, &cost).2 > ALWAYS;
        for &word in words.iter().filter(|_| !phrase) {
            kind[(word & TERM) as usize].fetch_or(1 | (always as u8) << 1, Relaxed);
        }
        always
    };
    spans.iter().map(line).collect()
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
    let keys: Vec<u32> = (0..sample_df.len()).map(key).collect();
    let keyed = || keys.iter().enumerate().map(|(t, &k)| (k, t as u32));
    let (_, order) = csr(keyed, 2 * most as usize + 1);
    // Freed first, so that `rank` reuses its pages instead of faulting in fresh ones.
    drop(keys);
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
    assert!(terms.len() <= 6, "queries.txt: a BOOL of over 6 terms");
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
pub fn csr<T, I: DoubleEndedIterator<Item = (u32, T)>>(
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
    // SAFETY: `pairs` made the same pairs both times (each caller's closure only
    // reads data that nothing changes in between), so each key's run was counted,
    // then filled down from its end: each of the start[keys] slots was written once.
    unsafe { values.set_len(start[keys] as usize) };
    (start, values)
}
