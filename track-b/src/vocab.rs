//! The vocabulary: the queries' lines and terms, and a perfect hash of the
//! distinct terms, in which the scan looks up each token.

use super::*;

/// Per part of queries.txt (a range of its lines, in order), `(spans, terms)`:
/// per line, its span `(start, len, phrase)`: its terms `terms[start..start + len]`,
/// and whether it is a phrase. The terms are BOOL terms as `id | op << 30`, the
/// first with OR, and phrase terms as ids.
pub type Queries = Vec<(Vec<Span>, Vec<u32>)>;

pub type Span = (u32, u32, bool);

/// Splits `chunk`, whole lines of `padded` (the file and mmap::PAD zero bytes),
/// into its lines, as in `Queries`, and their terms: each as its
/// offset in `padded` and its length | op << 30 (in a BOOL query). Also the
/// hashes (attempt `seed`'s: see SEEDS) of the terms' table keys (the zero key
/// for a long term) with their offsets and key lengths, grouped by hash & 63
/// (`starts`), and the long terms. (The long terms' zero key gets an id that no
/// term uses.) The vectors are sized for the most terms a chunk can hold (a
/// 1-byte term and its space: len / 2) and the most lines (a line is at least
/// "BOOL a" and LF, 7 bytes, under most / 3), so they never grow: their untouched
/// pages cost nothing.
fn split_lines(chunk: &'static [u8], padded: &'static [u8], seed: u64) -> Split {
    let seed = seed.wrapping_mul(SEEDS);
    let most = chunk.len() / 2 + 1;
    let (mut lines, mut long) = (Vec::with_capacity(most / 3), Vec::new());
    let mut terms = Vec::with_capacity(most);
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
            } else {
                // `AND`, `NOT` and `OR` start with bytes ending in binary 01, 10 and 11.
                // queries.txt is trusted (spec 2.3): any other word reads as the
                // operator its first byte's low two bits pick, 00 as AND.
                op = [0, AND, NOT, OR][(word[0] & 3) as usize];
            }
        }
        lines.push((start as u32, (terms.len() - start) as u32, phrase));
    }
    // The terms' keys grouped by hash & 63, the hash group. Key length 0 gives
    // the zero key, which marks a long term.
    let key = |&(at, len): &Term| {
        let len = (len & TERM) * ((len & TERM) as usize <= KEY) as u32;
        let hash = hash(checked_key(padded, at as usize, len as usize), seed);
        ((hash & 63) as u32, Key { hash, at, len })
    };
    let (starts, keys) = csr(|| terms.iter().map(key), GROUPS);
    Split {
        lines,
        terms,
        starts,
        keys,
        long,
    }
}

/// One thread's share of the query lines, from `split_lines`.
struct Split {
    /// Its lines, as spans of its terms from 0.
    lines: Vec<Span>,
    terms: Vec<Term>,
    /// Its keys, those of hash group g in `keys[starts[g]..starts[g + 1]]`.
    starts: Vec<u32>,
    keys: Vec<Key>,
    /// Its terms over KEY bytes.
    long: Vec<Long>,
}

type Long = &'static [u8];

/// A term's offset, and its length | op << 30.
type Term = (u32, u32);

/// A term's table key: its hash, and the offset and length of its bytes (length
/// 0 for a long term, whose key is zero). The key bytes are reloaded from the
/// file when they are needed.
#[derive(Clone, Copy, Default)]
struct Key {
    hash: u64,
    at: u32,
    len: u32,
}

/// Splits the lines in parallel, then builds the perfect hash of their terms and
/// gives each term its id: the short ones numbered as `place_regions` places
/// their keys, the long ones after them (an id is not a slot). `padded` is the
/// file and mmap::PAD zero bytes.
pub fn parse_queries(padded: &'static [u8], threads: usize) -> (Queries, Vocab) {
    let text = &padded[..padded.len() - mmap::PAD];
    // Spec 2.3: one query per line, each ending with LF, so an empty file has none.
    assert!(text.ends_with(b"\n"), "queries.txt: empty, or no last LF");
    // The start of the p-th of `threads` ranges of whole lines.
    let cut = |p: usize| line_after(text, text.len() * p / threads);
    let chunk = |p| &text[cut(p)..cut(p + 1)];
    let split = |seed| par(threads, &|p| split_lines(chunk(p), padded, seed));
    // Ends with probability 1: each seed gives unrelated hashes (two keys clash
    // with probability about 2^-64), and every third seed halves the load
    // (Vocab::new), so the pilot search finds free slots. Seed 0 succeeds on
    // the full seed-1, holdout seed-99, sample seed 1 to 4 and 18 fresh full
    // inputs; over 300 seeds on each of those 24, none needed a second seed.
    let (vocab, ids, splits) = (0..).find_map(|s| Vocab::new(split(s), s, padded)).unwrap();
    // Each split is a part of the queries, as it is: no copy into one array.
    let parts = splits.into_iter().map(|s| s.lines).zip(ids).collect();
    (parts, vocab)
}

/// The info of `slot` if its key is `key`, else `miss`. A slot is a term's key
/// (12 bytes, zero-padded) under its info word (the top 4 bytes): the key bytes
/// are compared in one vptest (`mask` selects them), and the cmov keeps it
/// branch-free (LLVM would branch).
///
/// # Safety
///
/// `slot` points to a slot of the table: 16 readable bytes, 16-byte aligned.
#[inline(always)]
pub unsafe fn probe(slot: *const u128, key: __m128i, miss: u32) -> u32 {
    // SAFETY: `slot` is valid for an aligned 16-byte read (the caller's contract;
    // u128 is 16-byte aligned); the asm reads registers only. AVX2 is enabled for
    // the whole build.
    unsafe {
        let x = _mm_xor_si128(_mm_load_si128(slot.cast()), key);
        // The info word on its own: a load, where vpextrd would be two uops
        // (volatile, or LLVM extracts it from the vector load above).
        let mut info = slot.cast::<u32>().add(3).read_volatile();
        asm!("vptest {x}, {mask}", "cmovnz {i:e}, {miss:e}",
             x = in(xmm_reg) x, mask = in(xmm_reg) _mm_set_epi32(0, -1, -1, -1),
             i = inout(reg) info, miss = in(reg) miss, options(pure, nomem, nostack));
        info
    }
}

/// The table key of the token `bytes[at..at + len]`, len <= KEY: its bytes,
/// ASCII-lowercased and zero-padded to 16. OR-ing 0x20 lowercases A-Z and keeps
/// digits; KEEP keeps only the token's bytes.
///
/// # Safety
///
/// `at + 16 <= bytes.len()` and `len <= 16`: the key is one 16-byte load from
/// `at`, masked by the 16 bytes of KEEP from `16 - len`.
#[inline(always)]
pub unsafe fn key_at(bytes: &[u8], at: usize, len: usize) -> __m128i {
    debug_assert!(at + 16 <= bytes.len() && len <= 16);
    // SAFETY: both loads are in bounds by the caller's contract (KEEP has 32
    // bytes, and 16 - len + 16 <= 32); AVX2 is enabled for the whole build.
    unsafe {
        let raw = _mm_loadu_si128(bytes.as_ptr().add(at).cast());
        let keep = _mm_loadu_si128(KEEP.as_flattened().as_ptr().add(16 - len).cast());
        _mm_and_si128(_mm_or_si128(raw, _mm_set1_epi8(0x20)), keep)
    }
}

/// `key_at` with its contract checked, for the setup's term records.
fn checked_key(bytes: &[u8], at: usize, len: usize) -> __m128i {
    assert!(len <= KEY, "a term key of {len} bytes, over KEY");
    // SAFETY: the slice is exactly the 16 bytes key_at loads, and len <= KEY < 16.
    unsafe { key_at(&bytes[at..at + 16], 0, len) }
}

/// The table key of the term record `e` of `file`, as the table stores it.
fn key_of(file: Long, e: Key) -> u128 {
    // SAFETY: __m128i and u128 have the same size, and any bits are valid.
    unsafe { transmute(checked_key(file, e.at as usize, e.len as usize)) }
}

/// Two AES rounds of the key, its low 64 bits as the hash. AESENC does
/// ShiftRows, SubBytes and MixColumns, then XORs the round key: after two rounds
/// every output byte depends on every key byte, non-linearly, and a different
/// first round key (the seed: see SEEDS) gives an unrelated hash. Three
/// instructions, one cycle of throughput each. The round keys come from the hex
/// digits of pi (any constants would do); a loop that hashes keeps them in
/// registers.
#[inline(always)]
fn hash(key: __m128i, seed: u64) -> u64 {
    let first = 0x243f6a8885a308d3_13198a2e03707344 ^ seed as u128;
    // SAFETY: u128 and __m128i have the same size, and any bits are valid.
    let [a, b]: [__m128i; 2] =
        unsafe { transmute([first, 0x24093822299f31d0_082efa98ec4e6c89u128]) };
    // SAFETY: AES-NI and SSE2 are enabled for the whole build (checked above).
    unsafe { _mm_cvtsi128_si64(_mm_aesenc_si128(_mm_aesenc_si128(key, a), b)) as u64 }
}

/// Attempt a's hash seed is a * SEEDS (an odd constant), which changes all 8 bytes
/// it is XORed into. (With the seed a, only bytes 0 and 1 changed, and the second
/// AES round carries them into hash bits 0-31 alone: bits 32-63, which decide the
/// slots a key can reach, stayed the same at every attempt.)
const SEEDS: u64 = 0x9e37_79b9_7f4a_7c15;

/// A window into it at 16 - len keeps the first len bytes of 16.
static KEEP: [[u8; 16]; 2] = [[0xFF; 16], [0; 16]];

/// Every query term, with its id and, after `finish`, its info.
///
/// The terms of up to 12 bytes are in a perfect hash (PTHash style): a term
/// with hash h is in bucket h & bmask, and the bucket's pilot puts it in slot
/// `(h * pilot).rotate_right(6) >> shift`. Every term has its own slot, so a
/// token is compared with one slot only. Two terms of a bucket share the low
/// bits of h but differ above, so different multipliers move them apart. A pilot
/// is 1 mod GROUPS, so the low 6 bits of h * pilot are those of h (its group):
/// rotated to the top, they make each group's slots a region of its own, and the
/// groups are placed in parallel.
pub struct Vocab {
    pub phf: Phf,
    /// 2^(64 - shift) slots of the hash, one per long term, then one with the
    /// info `count`. Empty slots have the zero key, which no token has (token bytes
    /// are never zero).
    pub table: Vec<u128>,
    /// The ids of the terms of 13 to 32 bytes, the last ones: about 0.2% of the
    /// corpus tokens are that long.
    long: std::collections::HashMap<&'static [u8], u32>,
    /// The number of terms: their ids are 0..count.
    pub count: u32,
}

/// The hash function of the table. Copy, so that the scan keeps it in registers.
#[derive(Clone, Copy)]
pub struct Phf {
    /// XORed into the hash's first round key.
    seed: u64,
    /// Per bucket (a power of two of them, at least 64), its pilot. Leaked (they
    /// live until the process exits), and atomic so that the threads of the build
    /// can write them (Phf::slot reads them as plain u16s, after the build).
    pilot: &'static [AtomicU16],
    /// 64 minus the table's bits: the slot is the top bits of the rotated product.
    shift: u32,
}

impl Phf {
    /// The slot of a short key (a term's if the key is one).
    #[inline(always)]
    pub fn slot(self, key: __m128i) -> usize {
        let h = hash(key, self.seed);
        // SAFETY: BMI2 is enabled for the whole build. The bucket, the low bits
        // of h, is below len (a power of two, at least GROUPS). The pilot is read
        // as a plain u16 (an atomic load would cost a second zero extension):
        // AtomicU16 has the layout of u16, and pilots are only read once the build
        // is over, which the build's barrier and the end of its threads order
        // before the reads.
        let pilot = unsafe {
            let bucket = _bzhi_u64(h, self.pilot.len().trailing_zeros()) as usize;
            self.pilot.get_unchecked(bucket).as_ptr().read()
        };
        place(h, pilot as u64, self.shift)
    }
}

/// The slot of the hash h with the pilot `pilot`: below 2^(64 - shift).
#[inline(always)]
fn place(h: u64, pilot: u64, shift: u32) -> usize {
    (h.wrapping_mul(pilot).rotate_right(GROUPS.ilog2()) >> shift) as usize
}

/// Thread q's keys for `Vocab::new`: those of its range of the hash groups,
/// from every split, sorted by bucket and then hash, one per hash. A bucket's
/// hashes share their low bits, so it lies in one group (hash & 63).
fn dedup_keys(splits: &[Split], q: usize, bmask: u64) -> Vec<Key> {
    let g = part(GROUPS, q, splits.len());
    let range = |s: &Split| s.starts[g.start] as usize..s.starts[g.end] as usize;
    let mine = || splits.iter().flat_map(|s| &s.keys[range(s)]);
    // The buckets of these groups numbered in order from 0: the bucket's bits
    // above the group's, then its group.
    let bucket =
        |e: &Key| ((e.hash & bmask) >> 6) as usize * g.len() + (e.hash & 63) as usize - g.start;
    let buckets = (bmask as usize + 1) / GROUPS * g.len();
    let (start, mut keys) = csr(|| mine().map(|e| (bucket(e) as u32, *e)), buckets);
    for b in 0..buckets {
        keys[start[b] as usize..start[b + 1] as usize].sort_unstable_by_key(|e| e.hash);
    }
    keys.dedup_by_key(|e| e.hash);
    keys
}

/// The first pilot whose slots for `bucket` are all free, claimed one at a time
/// with `set(key, pilot, true)` (which returns whether the slot was taken) and
/// given back at the first taken one; or None. The candidates are 1, 65, 129,
/// ...: 1 mod GROUPS keeps the group bits, and there are 1,024 of them.
fn find_pilot(bucket: &[Key], mut set: impl FnMut(&Key, u64, bool) -> bool) -> Option<u64> {
    (1..1 << 16).step_by(GROUPS).find(|&pilot| {
        let free = bucket.iter().take_while(|e| !set(e, pilot, true)).count();
        // Gives the claimed slots back unless all were free.
        let all = free == bucket.len();
        bucket[..free].iter().for_each(|e| _ = set(e, pilot, all));
        all
    })
}

/// Places the buckets of `keys` (a thread's distinct keys, by bucket) in `region`,
/// the table's slots from `base`, and writes their keys and ids, numbered from
/// `first`: largest buckets first, each with the first pilot whose slots are all
/// free (`find_pilot`). A key can reach few slots (find_pilot's candidates move it
/// by one fixed step each), which may all be taken: its bucket then goes first in
/// another attempt. (A failed attempt's keys left in the table are harmless: a
/// token only looks up the final slot of its own term.) False if 4 attempts fail.
fn place_regions(
    keys: &[Key],
    region: &mut [u128],
    base: usize,
    first: u128,
    phf: Phf,
    file: Long,
) -> bool {
    let bmask = phf.pilot.len() as u64 - 1;
    let same_bucket = |a: &Key, b: &Key| (a.hash ^ b.hash) & bmask == 0;
    let mut order: Vec<_> = keys.chunk_by(same_bucket).collect();
    order.sort_unstable_by_key(|b| Reverse(b.len()));
    let slot = |e: &Key, pilot| place(e.hash, pilot, phf.shift) - base;
    (0..4).any(|_| {
        let (mut taken, mut next) = (vec![false; region.len()], first);
        let mut set = |e: &Key, pilot, v| replace(&mut taken[slot(e, pilot)], v);
        let failed = order.iter().position(|bucket| {
            let Some(pilot) = find_pilot(bucket, &mut set) else {
                return true;
            };
            phf.pilot[(bucket[0].hash & bmask) as usize].store(pilot as u16, Relaxed);
            for e in *bucket {
                region[slot(e, pilot)] = key_of(file, *e) | next << 96;
                next += 1;
            }
            false
        });
        failed.inspect(|&i| order[..=i].rotate_right(1)).is_none()
    })
}

impl Vocab {
    /// The vocabulary of the query terms in `splits`, with attempt `seed`'s hash,
    /// the term words of each split with their ids, and `splits`; or None when
    /// the build needs another seed: two different keys with one hash, or a
    /// thread whose regions four placements could not fill (`place_regions`).
    /// Every third seed also doubles the table
    /// (parse_queries says why the seeds end).
    ///
    /// Each thread takes a range of groups and deduplicates their keys
    /// (`dedup_keys`). Then, in its own region of the table, it places their
    /// buckets and writes their keys and ids, and once every thread has, it gives
    /// its split's terms their ids. About 4.6 distinct terms per bucket and at
    /// least 1.5 slots per term (a load of 0.58 on seed 1). `file` is the query
    /// file and mmap::PAD zero bytes.
    fn new(splits: Vec<Split>, seed: u64, file: Long) -> Option<(Self, Vec<Vec<u32>>, Vec<Split>)> {
        let n = splits.len();
        let total: usize = splits.iter().map(|s| s.keys.len()).sum();
        // One bucket per 16 term occurrences (about 4.6 distinct terms each on
        // seed 1), and at least one per group.
        let bmask = (total / 16).max(GROUPS).next_power_of_two() as u64 - 1;
        let groups = par(n, &|q| dedup_keys(&splits, q, bmask));
        let distinct: usize = groups.iter().map(Vec::len).sum();
        // At least 1.5 slots per term and 1,024 in all (16 per group); every third
        // seed doubles the table.
        let bits = (distinct * 3 / 2).max(1024).next_power_of_two().ilog2() + seed as u32 / 3;
        let (slots, shift, region) = (1 << bits, 64 - bits, bits - GROUPS.ilog2());
        let mut long = std::collections::HashMap::new();
        for &term in splits.iter().flat_map(|s| &s.long) {
            let id = (distinct + long.len()) as u32;
            long.entry(term).or_insert(id);
        }
        // The table is allocated zeroed, so empty slots cost nothing until they
        // are read. Long term `id` has the slot `slots + id - distinct`.
        let count = distinct + long.len();
        let mut table = vec![0u128; slots + long.len() + 1];
        (distinct..count).for_each(|id| table[slots + id - distinct] = (id as u128) << 96);
        table[slots + long.len()] = (count as u128) << 96;
        let pilot: &[_] = Vec::leak((0..=bmask).map(|_| AtomicU16::new(0)).collect());
        let seed = seed.wrapping_mul(SEEDS);
        let phf = Phf { seed, pilot, shift };
        // The table's address as a usize, so that the closure is Sync; each thread
        // writes only its own regions (below). n = threads, the n of par's first call,
        // which made the pool: every part runs at once, as the barrier needs.
        let (at, barrier) = (table.as_mut_ptr().expose_provenance(), Barrier::new(n));
        let ids = par(n, &|q| {
            let table = std::ptr::with_exposed_provenance_mut::<u128>(at);
            // Thread q's regions, from `base`; it numbers its keys from the
            // number of keys before its groups.
            let regions = part(GROUPS, q, n);
            let (base, len) = (regions.start << region, regions.len() << region);
            let first = groups[..q].iter().map(Vec::len).sum::<usize>() as u128;
            // SAFETY: table[base..base + len] are thread q's regions, which no other
            // thread touches until the barrier below; base + len <= slots.
            let mine = unsafe { std::slice::from_raw_parts_mut(table.add(base), len) };
            let placed = place_regions(&groups[q], mine, base, first, phf, file);
            // The ids need every thread's pilots and slots.
            barrier.wait();
            // The info of a key's slot. A slot that holds another key means that
            // two keys have the same hash (dedup_keys keeps one key per hash): the
            // build then needs another seed.
            let mut clash = false;
            let mut info = |key| {
                // SAFETY: Phf::slot is below 2^(64 - shift) = slots, so a slot of the
                // table, which no thread writes after the barrier.
                let slot = unsafe { *table.add(phf.slot(key)) };
                // SAFETY: __m128i and u128 have the same size, and any bits are valid.
                clash |= slot & ((1 << 96) - 1) != unsafe { transmute::<__m128i, u128>(key) };
                (slot >> 96) as u32
            };
            // Only long terms are in `long`: a lookup there hashes all the bytes.
            // Every long term is in it, so the index does not fail.
            let id = |&(at, len): &Term| {
                let (at, op, len) = (at as usize, len & !TERM, (len & TERM) as usize);
                op | match len {
                    0..=KEY => info(checked_key(file, at, len)),
                    _ => long[&file[at..at + len]],
                }
            };
            let ids = placed.then(|| splits[q].terms.iter().map(id).collect());
            ids.filter(|_| !clash)
        });
        let vocab = Vocab {
            phf,
            table,
            long,
            count: count as u32,
        };
        Some((vocab, ids.into_iter().collect::<Option<_>>()?, splits))
    }

    /// Gives each slot the info the plan gives its id: its new id, ANCHOR and TRIG.
    pub fn finish(&mut self, plan: &Plan) {
        // Empty slots read as id 0 and get id 0's info: harmless, their zero key
        // matches no token. The last slot keeps `count` (the table always has it).
        for s in self.table.split_last_mut().unwrap().1 {
            *s = *s & ((1 << 96) - 1) | (plan.info[(*s >> 96) as usize] as u128) << 96;
        }
    }

    /// A long token's key and the byte offset of its slot: the zero key, and
    /// the slot of its term or the last one, which the zero key matches.
    #[cold]
    #[inline(never)]
    pub fn get_long(&self, token: &[u8]) -> (__m128i, u64) {
        // A token is at most 64 bytes (its length comes from a 64-bit mask).
        let lower = &mut [0u8; 64][..token.len()];
        lower.copy_from_slice(token);
        lower.make_ascii_lowercase();
        let id = self.long.get(&*lower).map_or(self.count, |&id| id);
        let slot = self.table.len() + id as usize - 1 - self.count as usize;
        let offset = (slot * size_of::<u128>()) as u64;
        // SAFETY: SSE2 is enabled for the whole build.
        (unsafe { _mm_setzero_si128() }, offset)
    }
}
