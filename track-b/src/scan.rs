//! The scan of one slice of the corpus: tokens found 64 bytes at a time, looked
//! up in the vocabulary, each document's queries evaluated when it ends, and
//! the thread's answers.

use super::*;

/// One thread's pass over a slice of the corpus. Without a plan it only counts
/// document frequencies into `df`, for the sample.
pub struct Scan<'a> {
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
    /// entries of its trigger terms, each term once, as indices, and the answers
    /// of those that match (see `trigger`; both as long as the entries).
    trig: Vec<u32>,
    evals: Vec<u32>,
    hits: Vec<u32>,
    /// Per row, its four words, one per block of this superblock: word b of row
    /// r < plan.rows, `wide[4 * r + b]`, is BOOL term r's in block b. Row `rows`
    /// takes the other terms (its bits are never read). A row's 256 bits are one
    /// 32-byte load in end_super. (Kept block by block from step 56, a term took
    /// four loads of four lines there; since the presence stores are prefetched
    /// and made 8 at a time, rows of 4 words are 1.0-1.1 ms faster.)
    wide: Vec<u64>,
    /// This document's tokens to look up: per token its key (12 bytes and zero
    /// bytes), with the byte offset of its slot stored over its top 4 bytes,
    /// which the probe does not compare (16 bytes, not 32: half the lines).
    pending: Vec<__m128i>,
    /// This document's tokens' info words (their ids once look_up has recorded
    /// them), and the positions of anchors among them.
    toks: Vec<u32>,
    anchors: Vec<u32>,
    /// The current document.
    pub doc: u32,
    pub answers: Answers,
    /// Per anchored phrase, the last document it matched, to count each once.
    last: Vec<u32>,
}

impl<'a> Scan<'a> {
    pub fn new(vocab: &'a Vocab, plan: Option<&'a Plan>, df: &'a [AtomicU32]) -> Self {
        let rows = plan.map_or(0, |p| p.rows);
        let answers = plan.map_or(0, |p| p.line_of.len());
        let wide = written_zeros(4 * (rows + 1));
        Scan {
            vocab,
            plan,
            df,
            // 3 bytes more: `trigger` gathers 4 bytes from a stamp.
            lastdoc: vec![EMPTY; vocab.count as usize + 5],
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
    pub unsafe fn run(mut self, docs: &'static [u8], start: usize, end: usize, first: u32) -> Self {
        self.doc = first.wrapping_sub(1);
        let mut bit = 0u64;
        // The scan goes by windows of 64 bytes from `at`, where byte `at` is
        // never inside a token: it starts the slice (and so a line), or it is
        // the byte that ended the window before. A window is scanned up to its
        // last byte that is not a token byte (`cut`), so every token it scans
        // ends inside it, and a token's length is the distance to its last byte.
        // The table and the hash are read once: read in the loop, LLVM loaded them
        // again after every run of tokens and kept the table's address on the stack.
        let (vocab, phf, table) = (self.vocab, self.vocab.phf, self.vocab.table.as_ptr());
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
                // Keys are loaded from the window's start plus b, with no token
                // offset at + b to add per token.
                let window = &docs[at..];
                while text != 0 {
                    let b = text.trailing_zeros() as usize;
                    let e = ends.trailing_zeros() as usize;
                    (text, ends) = (text & (text - 1), ends & (ends - 1));
                    // 1 - len, so that the key's KEEP window is a fixed offset from it.
                    let back = b as isize - e as isize;
                    if back > -(KEY as isize) {
                        // SAFETY: at + b < end (masks clips the token bits at end), so
                        // b + 16 <= window.len() (see # Safety); 1 - back = len <= KEY.
                        let key = unsafe { key_at(window, b, (1 - back) as usize) };
                        let off = phf.slot(key) * size_of::<u128>();
                        prefetch(table.cast::<u8>(), off);
                        // SAFETY: `reserve(32)` above made room for this window's tokens.
                        unsafe { out.write(key) };
                        // The offset is a second, 4-byte store (volatile, so that it stays
                        // one): inserted into the key by vpinsrd, 2 uops, it ran 0.4 ms
                        // slower. Offsets fit 32 bits: the table is far under 4 GB.
                        // SAFETY: as above, bytes 12 to 15 of the entry.
                        unsafe { out.cast::<u32>().add(3).write_volatile(off as u32) };
                    } else {
                        let (key, off) = vocab.get_long(&window[b..=e]);
                        // SAFETY: as above; SSE4.1 is enabled for the whole build.
                        unsafe { out.write(_mm_insert_epi32::<3>(key, off as i32)) };
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
        let (key, off) = self.vocab.get_long(&docs[p..p + len.min(64)]);
        // SAFETY: SSE4.1 is enabled for the whole build.
        let token = unsafe { _mm_insert_epi32::<3>(key, off as i32) };
        self.pending.push(token);
        p + len
    }

    /// Looks up the tokens of this document, whose bit is `bit`, then records
    /// their presence and lists its trigger occurrences and anchors. Each step runs
    /// over the whole document, so that the memory accesses of one step overlap:
    /// the slots were fetched when the tokens were found. (In the earlier block
    /// design, fetching the presence words too cost more than it saved, and one
    /// loop for the probes and the presence was slower; it still is, by 2.2 ms.)
    fn look_up(&mut self, bit: u64) {
        let (miss, table) = (self.vocab.count, self.vocab.table.as_ptr());
        let (doc, rows) = (self.doc, self.wide.len() / 4 - 1);
        // This document's block in the superblock: word `block` of each row.
        let block = (doc / 64 % (SUPER / 64)) as usize;
        let wide = self.wide.as_mut_ptr().wrapping_add(block);
        // 8 more each: end_doc's phrase compares read 8 ids from any token, and the
        // second step reads 8 ids and stores 8 lanes at a time.
        let more = self.pending.len() + 8;
        self.toks.reserve(more);
        self.trig.reserve(more);
        self.anchors.reserve(more);
        let (toks, start) = (self.toks.as_mut_ptr(), self.toks.len());
        let mut len = start;
        for key in &self.pending {
            // The slot offset, a 4-byte load (volatile, or LLVM extracts it from
            // the key's 16-byte load, 2 uops).
            // SAFETY: the top 4 of the 16 bytes of a pending token.
            let off = unsafe { (key as *const __m128i).cast::<u32>().add(3).read_volatile() };
            // SAFETY: off is the byte offset of a slot of the table: Phf::slot gives
            // one below 2^(64 - shift), and get_long a long term's or the last one.
            let info = unsafe { probe(table.byte_add(off as usize), *key, miss) };
            // SAFETY: room for every pending token was reserved above.
            unsafe { toks.add(len).write(info) };
            len += 1;
        }
        // Then 8 ids `miss`, which no phrase term has, with no flag, no row and a
        // stamp no evaluation reads (the token of no term).
        // SAFETY: the first len ids are written.
        unsafe { self.toks.set_len(len) };
        // SAFETY: room for 8 more ids was reserved.
        unsafe { toks.add(len).cast::<[u32; 8]>().write([miss; 8]) };
        self.pending.clear();
        // Then 8 ids at a time, those from `at`: their presence (with the plan: the
        // sample only counts documents per term, in end_doc), and without a branch,
        // as which tokens have them is too random to predict, the trigger
        // occurrences (their info words; end_doc takes each term once) and the
        // anchors' positions. The last 8 may take some of the `miss` ids after the
        // tokens: their stamp and the row sink's bit are written, and no list keeps
        // them.
        let (trig, mut nt) = (self.trig.as_mut_ptr(), self.trig.len());
        let (anchors, mut na) = (self.anchors.as_mut_ptr(), self.anchors.len());
        let (lastdoc, stamp) = (self.lastdoc.as_mut_ptr(), self.stamp);
        let sample = self.plan.is_none();
        let mut row = [0u32; 8];
        for at in (start..len).step_by(8) {
            // SAFETY: AVX2 is enabled for the whole build; the 8 ids from at < len
            // are written (above).
            let infos = unsafe { _mm256_loadu_si256(toks.add(at).cast()) };
            // The 8 tokens' ids replace their info words (end_doc masks them anyway),
            // and their rows' offsets, 4 * id.min(rows), go to `row`: 5 uops for the
            // 8, where the scalar `& ID` and cmp/cmov took 3 a token (replica,
            // non-PGO: -0.72 ms).
            // SAFETY: as above; `row` holds 8 u32s.
            unsafe {
                let ids = _mm256_and_si256(infos, _mm256_set1_epi32(ID as i32));
                _mm256_storeu_si256(toks.add(at).cast(), ids);
                let rows8 = _mm256_min_epu32(ids, _mm256_set1_epi32(rows as i32));
                _mm256_storeu_si256(row.as_mut_ptr().cast(), _mm256_slli_epi32::<2>(rows8));
            }
            if !sample {
                for k in 0..8 {
                    // SAFETY: at + k < len + 8 (at < len): ids written above.
                    let id = unsafe { *toks.add(at + k) } as usize;
                    // SAFETY: id <= count < lastdoc.len(), and row[k] = 4 * id.min(rows)
                    // starts one of the rows + 1 rows, whose word `block` is this one.
                    unsafe {
                        // A store that misses L1 holds up the stores after it, which
                        // commit in order; fetched for writing first, the line arrives
                        // out of order (replica: store-buffer stalls 161M -> 50M
                        // cycles, -1.7 ms; ahead in the probe loop, +1.4 ms).
                        _mm_prefetch::<_MM_HINT_ET0>(lastdoc.add(id).cast());
                        *lastdoc.add(id) = stamp;
                        *wide.add(row[k] as usize) |= bit;
                    }
                }
            }
            // SAFETY: AVX2 is enabled for the whole build. nt and na grow by at most
            // one per token before at, at most at - start <= len - 1 - start past
            // their first values, so the 8 lanes stored from them end inside the room
            // reserved for them, more = len - start + 8 past those first values.
            unsafe {
                let positions = _mm256_add_epi32(_mm256_set1_epi32(at as i32), LANES);
                // TRIG is bit 30, shifted to the sign bit; ANCHOR is the sign bit.
                nt += left_pack(infos, _mm256_slli_epi32::<1>(infos), trig.add(nt));
                na += left_pack(positions, infos, anchors.add(na));
            }
        }
        // SAFETY: the first nt and na entries are written.
        unsafe { (self.trig.set_len(nt), self.anchors.set_len(na)) };
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
            // The sample: count the document's distinct terms, each at its first
            // token, the one that finds its stamp older (the table has no flags
            // yet, so the info words are the ids).
            for &t in self.toks.iter().filter(|&&t| t != self.vocab.count) {
                if replace(&mut self.lastdoc[t as usize], self.stamp) != self.stamp {
                    self.df[t as usize].fetch_add(1, Relaxed);
                }
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
        // SAFETY: trigger terms are below bool_terms <= count, so t indexes lastdoc
        // (count + 5 bytes), and group has bool_terms + 2 entries. Fresh terms are
        // distinct, so their ranges are disjoint, and n stays at most the number of
        // real entries, entries.len() - 8: a store of 8 indices from n + j (j < the
        // range's length; 0 for a repeat) ends inside evals, which is as long as the
        // entries. AVX2 is enabled for the build.
        unsafe {
            for t in self.trig.iter().map(|&info| (info & ID) as usize) {
                // Branch-free: a repeated trigger term is written again and not kept.
                let fresh = *lastdoc.add(t) == stamp;
                *lastdoc.add(t) = stamp | DONE;
                let (from, to) = (*group.add(t) as u32, *group.add(t + 1) as u32);
                prefetch(entries, from as usize);
                // Its entries' indices, 8 per store: the next range overwrites the
                // spare ones, and a range of over 8 (rare) goes on.
                let store = |f: u32| {
                    let at = evals.add(n + (f - from) as usize).cast();
                    _mm256_storeu_si256(at, _mm256_add_epi32(_mm256_set1_epi32(f as i32), LANES));
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
        let stamps = lastdoc.cast::<i32>().cast_const();
        let mut nh = 0;
        for b in (0..n).step_by(8) {
            // SAFETY: AVX2 and BMI2 are enabled for the whole build. evals[b..b + 8]
            // (b < n) were written above: lanes below n by the ranges' stores (a
            // store's spare lanes are overwritten by the next), the rest by the
            // padding store, so every index is below entries.len(): each 16-byte load
            // is one aligned entry (DocEntry). Entry terms are BOOL terms or 0, below
            // count + 1, and lastdoc has count + 5 bytes, so the 4 bytes gathered from
            // each stamp are inside it. nh <= b < n, so the 8 lanes stored at hits[nh..]
            // are inside hits, as long as evals.
            unsafe {
                // Entries j and j + 4 in one vector, then transposed: lane j of word k
                // is word k of entry j. 8 loads and 8 shuffles instead of 4 gathers of
                // 8 lanes (replica: 0.4-0.7 ms faster).
                let entry = |j| _mm_load_si128(entries.add(*evals.add(b + j) as usize).cast());
                let pair = |j| {
                    _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(entry(j)), entry(j + 4))
                };
                let (p0, p1, p2, p3) = (pair(0), pair(1), pair(2), pair(3));
                let (t0, t1) = (_mm256_unpacklo_epi32(p0, p1), _mm256_unpackhi_epi32(p0, p1));
                let (t2, t3) = (_mm256_unpacklo_epi32(p2, p3), _mm256_unpackhi_epi32(p2, p3));
                let (w01, w23) = (_mm256_unpacklo_epi64(t0, t2), _mm256_unpackhi_epi64(t0, t2));
                let (w4a, truth) = (_mm256_unpacklo_epi64(t1, t3), _mm256_unpackhi_epi64(t1, t3));
                let low = |w| _mm256_and_si256(w, _mm256_set1_epi32(0xFFFF));
                let high = |w| _mm256_srli_epi32::<16>(w);
                let terms = [low(w01), high(w01), low(w23), high(w23), low(w4a)];
                // Bit k: term k is in this document, from the stamp byte without
                // DONE (EMPTY's 0x7F is no stamp): has is -1 if so, and the bits
                // are shifted in from the last term (present * 2 - has).
                let mut present = _mm256_setzero_si256();
                for &t in terms.iter().rev() {
                    let s = _mm256_i32gather_epi32::<1>(stamps, t);
                    let s = _mm256_and_si256(s, _mm256_set1_epi32(!DONE as i32));
                    let has = _mm256_cmpeq_epi32(s, _mm256_set1_epi32(stamp as i32));
                    present = _mm256_sub_epi32(_mm256_add_epi32(present, present), has);
                }
                // Bit 31 of a lane: bit `present` of its truth table, whether it matches.
                let hit = _mm256_slli_epi32::<31>(_mm256_srlv_epi32(truth, present));
                // The matches' answers to the front.
                nh += left_pack(high(w4a), hit, hits.add(nh));
            }
        }
        for &answer in &self.hits[..nh] {
            // SAFETY: an entry's answer is an answer number, below state.len().
            unsafe { self.answers.add_doc(answer, doc) };
        }
    }

    /// Evaluates the `always` queries on the superblock of 256 documents from
    /// `base`, on four words at once (one bit per document), then clears it.
    fn end_super(&mut self, base: u32) {
        let Some(plan) = self.plan else { return };
        let wide = self.wide.as_ptr();
        for entry in &plan.always {
            let terms = &entry.terms[..entry.len as usize];
            // SAFETY: the terms of `always` entries are rows below plan.rows, so
            // their 4 words are in wide (an unaligned load: the Vec is 8-aligned).
            // AVX2 is enabled for the whole build.
            let hits = unsafe {
                terms.iter().fold(_mm256_setzero_si256(), |r, &word| {
                    let x = _mm256_loadu_si256(wide.add(4 * (word & TERM) as usize).cast());
                    match word >> 30 {
                        AND => _mm256_and_si256(r, x),
                        NOT => _mm256_andnot_si256(x, r),
                        _ => _mm256_or_si256(r, x),
                    }
                })
            };
            self.answers.add_wide(entry.answer, hits, base);
        }
        // The sink row is never read, so never cleared.
        self.wide[..4 * plan.rows].fill(0);
    }
}

/// `len` zeros, written now: a lazily zeroed page that is read first faults
/// again when written, and that fault flushes the TLB of every thread.
/// `black_box` keeps LLVM from turning the writes into a lazily zeroed
/// allocation (calloc).
fn written_zeros(len: usize) -> Vec<u64> {
    std::iter::repeat_n(std::hint::black_box(0), len).collect()
}

/// One thread's answers.
pub struct Answers {
    /// Per answer: count << SUM_BITS | sum of the matching ids.
    pub state: Vec<u64>,
    /// `answer << 16 | id & 0xFFFF` for the first FIRST_IDS matches of each answer
    /// (see MIN_SLICES), in `log[..logged]`. Its zero pages cost no memory until
    /// they are written.
    pub log: Vec<u32>,
    pub logged: usize,
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
            self.state[answer as usize] += count_positions(hits, base);
        } else if has_hits {
            // Still logging first ids: a word at a time.
            // SAFETY: __m256i and [u64; 4] have the same size, and any bits are valid.
            let words: [u64; 4] = unsafe { std::mem::transmute(hits) };
            // SAFETY: the state index above checked answer < state.len().
            (0..4).for_each(|i| unsafe { self.add(answer, words[i], base + 64 * i as u32) });
        }
    }
}

/// The state increment of the set bits of `v`, bit i for document `base + i`:
/// their number << SUM_BITS, plus the sum of their ids. Per 64-bit word, psadbw
/// adds up its bytes' counts and their bits' positions in the byte (from tables of
/// the 4-bit nibbles), and the byte's place in the word times its count.
fn count_positions(v: __m256i, base: u32) -> u64 {
    // SAFETY: AVX2 is enabled for the whole build; the loads read the 16-byte tables.
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
        // Per word: its count n, and its bits' positions in it: own, plus 8 per set
        // bit times the byte's place in the word (maddubs pairs those: at most 104).
        let sum = |x| _mm256_sad_epu8(x, _mm256_setzero_si256());
        let n = sum(count);
        let places = _mm256_maddubs_epi16(count, _mm256_set1_epi64x(0x0706_0504_0302_0100));
        let pos = _mm256_add_epi64(sum(own), _mm256_slli_epi64::<3>(sum(places)));
        // Word w's documents start at base + 64w: n << SUM_BITS, n (base + 64w) and
        // pos (vpmuludq multiplies the low 32 bits: n <= 64, and ids are below 2^20).
        let firsts = _mm256_set_epi64x(192, 128, 64, 0);
        let firsts = _mm256_add_epi64(_mm256_set1_epi64x(base as i64), firsts);
        let inc = _mm256_add_epi64(_mm256_slli_epi64::<{ SUM_BITS as i32 }>(n), pos);
        let inc = _mm256_add_epi64(inc, _mm256_mul_epu32(n, firsts));
        let half = _mm256_extracti128_si256::<1>(inc);
        let half = _mm_add_epi64(_mm256_castsi256_si128(inc), half);
        _mm_cvtsi128_si64(_mm_add_epi64(half, _mm_unpackhi_epi64(half, half))) as u64
    }
}

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

/// Stores to `out` the lanes of `values` whose lane of `signs` is negative, in
/// order, and returns how many. All 8 lanes are stored, the rest copies of lane
/// 0: their lane numbers are PACK's entry for the 8-bit mask, one load (pdep,
/// imul and pext made them in 4 uops: replica, PGO, 0.32 ms slower on average
/// over seeds 1, 99, 105 and 110).
///
/// # Safety
///
/// `out` is valid for an unaligned 32-byte store.
#[inline(always)]
unsafe fn left_pack(values: __m256i, signs: __m256i, out: *mut u32) -> usize {
    // SAFETY: AVX2 is enabled for the whole build; PACK has an entry for every
    // u8, 8 bytes each; the store is valid (the caller's contract).
    unsafe {
        let mask = _mm256_movemask_ps(_mm256_castsi256_ps(signs)) as u8;
        let lanes = _mm_loadl_epi64(PACK.as_ptr().add(mask as usize).cast());
        let packed = _mm256_permutevar8x32_epi32(values, _mm256_cvtepu8_epi32(lanes));
        _mm256_storeu_si256(out.cast(), packed);
        mask.count_ones() as usize
    }
}

/// Per 8-bit mask, the numbers of its set bits in order, a byte each (then 0s).
static PACK: [u64; 256] = {
    // Mask m's lowest set bit, then the numbers of m without it, a byte higher.
    let (mut table, mut m) = ([0; 256], 1);
    while m < 256 {
        table[m] = m.trailing_zeros() as u64 | table[m & (m - 1)] << 8;
        m += 1;
    }
    table
};

/// Lane i holds i.
// SAFETY: __m256i and [u32; 8] have the same size, and any bits are valid.
const LANES: __m256i = unsafe { transmute([0u32, 1, 2, 3, 4, 5, 6, 7]) };

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
