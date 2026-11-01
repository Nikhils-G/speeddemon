# Track B: Index

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).
The step-by-step history, with a board run for every step, is in [PERF.md](PERF.md).

## The problem

250,000 documents (254 MB) and 50,000 queries known up front:
- 31,000 BOOL queries of 1 to 6 terms, evaluated strictly left to right;
- 19,000 phrases of 2 to 8 terms.

Each answer is the match count, the sum of the matching ids and the ten smallest ids.

## The bottleneck

- **Memory, first.** The obvious design is an inverted index of the query terms. It holds about
  21M (term, document) pairs for the BOOL terms alone, and however it was packed (steps 1 to 5 in
  PERF.md), the run peaked at about 200 MB. The M axis gives full credit only under 44 MB (the 50x
  cap on the reference's 2,200 MB), so at 200 MB memory alone cost about 6.6 points.
- **Then the token path.** Without an index, the work is one pass over 36.3M tokens. 76% of them
  are BOOL query terms, 3% are phrase-only terms and 21% are words no query uses, and each one must
  be identified exactly.
- **Then the BOOL evaluation's random loads.** On the box, the block engine of PERF.md steps 6 to
  48 spent 16 ms setting presence words and 18.5 ms evaluating blocks, against 38 ms for the token
  path (exit-code probes, PERF.md "Box probes since step 47"). Those were loads at random
  addresses. The per-document design below removed the presence words and the block ends.

Where the time of step 49 (the per-document design, before the last changes) goes, on the replica
of the scoring box (PERF.md, "How we measured": the same CPU, the board's own timer; a build that
prints the time at each phase boundary, medians of 7 runs):

| Phase | ms | What |
|---|---|---|
| Setup | 12.5 | the query split 2.6, grouping the keys 1.1, the pilots 1.1, assembly 0.8, the sample 0.9, the plan 5.7, finish 0.4 |
| Scan | 77.0 | tokens, lookups, per-document evaluation, superblocks, phrases |
| After the scan | 2.1-2.6 per thread | the first-id logs put in line order |
| Format | 1.4 | |
| Write | 0.5 | |
| Exit | 2.0 | |

Exit-code probes of the same code on the box put the scan's token path with its per-token stores
at 49.5 ms, the per-document BOOL evaluation at 14 ms more, and the superblocks at 9 ms more
(PERF.md, "Box probes since step 47"; each level in its own dispatch).

Inside the scan of step 53, on the replica (builds that skip one part, wrong output, timing only;
PERF.md, "Where the scan's time goes"):

| Part skipped | Time saved |
|---|---|
| Phrase checks | 2.21 ms |
| The triggered per-document evaluation | 14.73 ms |
| The `always` superblocks | 9.24 ms |
| The per-token stores (stamps and row ORs) | 25.67 ms |
| The row ORs alone | 10.74 ms |
| Everything but the token path | 34.88 ms: token path, setup and tail are about 56.6 ms |

37% of the tokens are of no row term and go to one sink row, whose word every such token ORs.

The submitted code (PERF.md step 62, S 1033) adds to step 53 a persistent pool of threads for the
setup, leaner plan tables, block-major superblock rows, a lighter regrouping of the first ids in
4-byte entries, a hash build that no tested seed has failed, and one masked compare per phrase. On
the replica it runs in about 85.3 ms, against 90.6 ms for step 53 and 97.2 ms for step 49. Its
board runs took 0.087799 s (run-e734232a0ec5, 30.49 MB, 79.26 points), 0.088963 and 0.090207 s,
and 0.132044 s in a slow dispatch. The best Track B run,
run-31588c94b3ee (0.08596 s, 30.11 MB, 79.46 points), is step 59's, the same engine.

## The design

There is no index. The queries are read first, the corpus is scanned once in parallel slices of
whole lines, and every query is answered during that scan, so nothing grows with the corpus. The
board measured 39.1 to 40.32 MB for steps 52 to 55, 30.93 to 31.57 MB for step 58 and 30.11 and
30.25 MB for step 59 and 29.97 to 30.63 MB for steps 61 and 62, and the replica's max of 5 runs of step 61 was
30.29 MB, under the 44 MB
credit.

1. **Setup, in parallel.** Each phase runs on every thread, and allocates little, because what
   parallel setup mostly waits on is page faults of fresh memory.
   - *One pool.* The seven setup phases, the scan and the formatting all run on one pool of
     threads, made by the first call to `par`. Between calls a worker waits in C0.2 with
     `umonitor`/`umwait` (WAITPKG, checked by CPUID; without it, a polling loop): no call spawns a
     thread, each worker keeps its heap arena, its SMT sibling gets the core, and its CPU stays out
     of the deep idle states that take up to 1 ms to leave. On the replica this ended setup at
     11.10 ms instead of 12.39 ms.
   - *Split.* Each thread splits a range of query lines into compact records. A term is 8 bytes:
     its offset in the file, and its length | op << 30. A key is 16 bytes: its hash, offset and key
     length; the key bytes are reloaded from the file when needed. The vectors are sized for the
     most terms a chunk can hold, so they never grow and never copy, and their untouched pages
     cost nothing. The keys are grouped by hash group (`hash & 63`) with the same counting sort,
     `csr`, that the plan uses.
   - *Perfect hash* of the terms, built on every thread (design 2). The ids are then read back for
     every term. The map of long terms is consulted only for terms over 12 bytes.
   - *Sample.* The start of every slice, 1 MB in total (about 1,030 documents on seed 1), is scanned
     to estimate each term's document frequency.
   - *Plan*, a range of query lines per thread:
     - whether each line is an `always` query (design 4), with each term's kind set by an atomic
       OR;
     - the term order (BOOL terms first, so that they index dense per-thread arrays), by a
       counting sort;
     - each part renumbers its own terms and builds its entries: a truth-table entry per (query,
       cover term) (design 3), an `always` entry, or a phrase record;
     - `csr` groups the entries by trigger term, while a second worker (a `par(2)` round) sorts
       the `always` entries on one packed key and groups the phrases by anchor. `csr` counts each
       key, turns the counts into run ends, and writes each value into its run from the end, last
       value first, straight into a vector's spare capacity: no zero fill and no copy.
2. **Finding and looking up tokens.**
   - *Windows.* Each 64-byte window is classified with AVX2 into a token-byte mask and an LF mask.
     A window starts at a byte that is not inside a token and is scanned up to its last non-token
     byte, so every token it scans ends inside it. Token starts and last bytes are bit operations
     on the mask, and a token's length is the distance between the two. The next window is fetched
     before this one is scanned.
   - *Keys.* A token's key is its first 12 bytes, lowercased and zero-padded in one 16-byte
     register.
   - *Perfect hash.* The query terms of up to 12 bytes (75,735 distinct keys on seed 1) are in a
     perfect hash, so a token is compared with one 16-byte slot, never more:
     - *Hash.* Two AES rounds of the key (`aesenc` twice, the seed XORed into the first round key),
       the low 64 bits kept. After two rounds every output bit depends on every key byte. Attempt
       a's seed is a × 0x9e3779b97f4a7c15, which changes all 8 bytes it is XORed into, so every
       hash bit depends on the seed too. It costs three instructions per token.
     - *Pilots* (PTHash style). A hash h falls in bucket `h & bmask` (16,384 buckets on seed 1,
       about 4.6 terms each), and the bucket's 16-bit pilot places it in slot
       `(h * pilot).rotate_right(6) >> shift`. The builder takes the largest buckets first and gives
       each the first pilot whose slots are all free, so every term gets a slot of its own.
     - *Parallel build.* Every pilot is 1 mod 64, so the low 6 bits of `h * pilot` are those of h
       (its group), and the rotation makes them the slot's top bits. Each of the 64 groups thus owns
       one region of the table, whatever its pilots. Each of the 12 threads sorts, deduplicates and
       places a range of groups in its own regions, with no locks; then a barrier, then each thread
       reads back the ids of its share of the query lines.
     - *Slot.* 16 bytes: the term's 12 key bytes under its 4-byte info word (its id, and the flags
       ANCHOR and TRIG). The table is 2^17 slots (2 MB, a load of 0.58) plus one slot per long term
       and a final MISS slot; empty slots are left zero.
     - *Prefetch.* The slot's byte offset is computed and prefetched as soon as the token is found,
       and a document's tokens are all found before any is looked up, so the misses of one
       document overlap.
     - *Probe.* One aligned 16-byte load, an XOR with the token's key, a `vptest` of the 12 key
       bytes, and a `cmov` that selects the info word (a 4-byte load of the same slot) or MISS.
       There is no branch: whether a token is a query term is too random for the branch predictor.
     - *Re-seed.* If a bucket finds no free pilot among its 1,024 candidates, the thread that
       owns its region places that region's buckets again, that bucket first, up to four times; the
       other threads are not affected. If that fails too, or two different keys of a group have
       the same 64-bit hash, the build runs again with the next seed, and every third seed doubles
       the table. This ends: every third seed halves the load, so the pilot search finds free
       slots, and two different keys share all 64 bits of a hash with probability about 2^-64.
       Over 300 hash seeds on each of 32 datasets (9,600 builds), no build needed a second seed;
       with step 57's build 257 did (2.68%, on 20 of the 32 datasets). Seed 0 needed no second
       placement on any of the 24 datasets of the first sweep (PERF.md, step 58). A second placement costs
       about 0.4 ms on its thread; a second seed cost 3.70 ms in a forced test. The output is the
       same whichever seed succeeds.
   - *Long terms.* Terms of 13 to 32 bytes (0.2% of the tokens) go to a cold hash map. Their slots
     sit after the hash's, so the scan treats both alike.
3. **BOOL queries, per document, from truth tables.**
   - *Covers.* A query is evaluated only in documents that contain one of its *cover* terms: a set
     of terms of which every match contains at least one, chosen to be rare in the sample.
   - *Entries.* For each (query, cover term c), the plan makes one 32-byte entry: the query's other
     distinct terms (at most 5, padded with a term no document has), its answer, and a 32-bit truth
     table. Bit a of the table says whether the query matches when c is present and exactly the
     other terms of a's set bits are present, folding AND, OR and NOT strictly left to right. It is
     0 wherever a cover term before c is present: that term's entry counts the document instead.
   - *Presence.* Each thread keeps, per term, a one-byte *stamp* of the last document that contains
     it: the document's id mod 127, written for every token. Every 127 documents all stamps are
     reset to a value no document has, so an equal stamp always means this document.
   - *Evaluation.* At a document's end, each trigger term of the document is taken once (its stamp
     gets a DONE bit). Its entries are evaluated: 5 stamp compares give 5 presence bits, and the
     answer is one bit of the truth table. No branch depends on the data. On seed 1 that is 4.89M
     evaluations; every BOOL query on every document would be 7.75 billion.
   - *Two passes.* The entry ranges of the trigger terms are random reads, so the evaluation runs
     in two passes. The first pass loads each term's range and prefetches its first entry. The
     second evaluates the ranges, prefetching the answer state of the range 2 ahead.
4. **Common queries, per superblock of 256 documents.** Some queries have cover terms expected in
   more than 30% of the 64-document blocks, summed over the terms (`ALWAYS`; at 0.25, the previous
   setting, that was 5,993 queries on seed 1). They would be triggered in almost every document. They are evaluated once per 256 documents
   instead, with AVX2 on one 256-bit row per term (one bit per document), and only over their
   real terms. A row's four 64-bit words are kept by block: word r of block b at
   `b * (rows + 1) + r`, so the word a token sets lies in its block's contiguous array of 8-byte
   words, and `end_super` puts each term's four words together. They are sorted by term count and operators, so the operator branch
   predicts. Each token sets its document's bit in its term's row; a token of no row term goes to
   one sink row, chosen without a branch (a `cmov`).
5. **Answers numbered in evaluation order.** Each thread keeps one u64 of state per query (count
   and id sum) and a log of first ids, 4 bytes each (`answer << 16 | id & 0xFFFF`). Queries are numbered in the order the scan updates them:
   - the `always` queries first, so that every superblock reads and writes their state as one
     47 KB stream instead of about 4,000 scattered cache lines;
   - then the entries' answers in table order;
   - then the phrases.

   `answer_of` and `line_of` map between query lines and these numbers.
6. **Counts, sums and first ids without a loop over documents.**
   - *Per document:* `state += hit * (1 << 40 | doc)`, branch-free; the first-id log slot is
     always written and kept only when wanted.
   - *Per superblock:* the count is a popcount. The sum of the matching ids is
     `base * popcount(w) + sum over k of 2^k * popcount(w & M_k)`, with six fixed masks M_k (M_0 =
     0xAAAA..., the odd positions); for 256 documents at once, nibble lookup tables do the same in
     AVX2.
   - *First ids:* from trailing-zero counts, and only while a query has fewer than ten.
7. **Phrases.**
   - *Anchors.* Each phrase is anchored on its rarest sampled term. Its 64-byte record holds its
     first 8 term ids, its answer, the anchor's position and its length.
   - *Fetching.* When a token's info word has the ANCHOR flag, the anchor's range of records is
     prefetched; at the document's end, each anchor's first record is fetched before the BOOL
     evaluation and read after it.
   - *Check.* Every phrase is checked by one masked 32-byte compare against the document's token
     ids, which are still in L1. `look_up` writes 8 ids of `miss` after each document's tokens, so
     the 8 ids from any phrase start exist, and none equals a phrase term (`miss` is the id of every
     token no query uses). A phrase of over 8 terms, which the spec does not have, stops the program
     when the queries are planned.
   - *Counting.* A per-phrase last-document marker counts each document once.
8. **Merging.**
   - *Release.* Each thread drops its slice's pages from the page tables after its scan, in
     parallel.
   - *First ids, without a sort.* An answer logged its first min(count, 10) ids in id order, so a
     prefix sum of those counts over the lines gives every entry its place in line order, and one
     scatter puts it there, each id rebuilt from its low 16 bits and its slice's first id. The
     slices are in document order, so the logs merge in slice order into the ten smallest ids. The
     first slice's thread publishes which answers it alone fills with ten (`full`); the other
     threads skip those, which hold most of their logged ids (about 100K of 122K each on seed 1).
   - *Formatting.* Output lines are formatted in parallel. Each formatter adds up the threads'
     counts and sums by reading their states in place through `answer_of`; a copy into line order
     cost about 4.5 MB and once put the run over the 44 MB credit (PERF.md, step 26). A thread's ids for
     a line lie between that line's end and the one before (`ends`).
   - *Exit.* The process exits right after writing instead of freeing its tables.

## Techniques

What limits this track is waiting on loads whose address is random, and the uops of the token
path. One random load per corpus token goes into the term table; at each document's end come the
entry ranges, entries and answer states of the queries its terms trigger. Each technique below
names the cost it attacks, what the code does, and why the output stays the same.

| Technique | Bottleneck | What the design does | Why the output is unchanged |
|---|---|---|---|
| No index; queries first | An index of the query terms holds 21M (term, document) pairs: 85 to 200 MB, past the 44 MB credit (steps 1 to 5) | All 50,000 queries are read first and answered during one parallel scan; nothing grows with the corpus | Every query is evaluated on every document that can match it (covers, below), with the spec's set operations |
| Perfect hash, AES-hashed | One random probe per token; the old 4-way buckets compared four keys and spilled into the next bucket | PTHash-style pilots give every term of up to 12 bytes its own 16-byte slot; the hash is two `aesenc` rounds; the probe is one load, an XOR, a `vptest` and a `cmov` | The probe compares the token's whole key with the slot's; the hash only says where to look. A failed build re-seeds; it never yields a table that answers wrongly |
| Lock-free parallel hash build | Sorting, deduplicating and placing 155,000 term keys takes about 10 ms on one thread of the test server | Pilots are 1 mod 64, so each of 64 hash groups owns a region of the table, and 12 threads place their groups without locks | Groups, bucket order and ids do not depend on thread timing; each thread writes only its regions before the barrier |
| SIMD tokenizer, prefetched lookups | 36.3M tokens to find and look up | AVX2 classifies 64 bytes into token and LF bitmaps; windows end at a non-token byte, so starts and lengths are bit operations; each slot is prefetched when its token is found, and a document's tokens are all found before any is probed | A token is exactly a maximal run of `[A-Za-z0-9]`; the key is its bytes OR 0x20, masked to its length |
| Truth tables per document | A BOOL query's left-to-right formula over up to 6 terms, in 4.89M evaluations on seed 1 | The plan folds each (query, cover term) into a 32-bit table over the other terms' presence; an evaluation is 5 stamp compares and one bit of the table, branch-free | Bit a of the table is the spec's left-to-right evaluation with exactly those terms present; the earlier-cover rule makes one entry count each document |
| One-byte stamps | A per-thread record of each term's last document, written for every token | The stamp is the document's id mod 127, reset every 127 documents: 75 KB per thread instead of 303 KB of u32 | An equal stamp can only be this document's: within a window each stamp belongs to one document, and the reset value matches none |
| Cover triggers from a sample | 121M evaluations if every query ran on every 64-document block | From a 1 MB sample, each query gets a cover, a set of terms every match contains; it is evaluated only in documents with a cover term; queries with common covers run per 256 documents with AVX2 | A document without a cover term holds no match. The sample only decides where a query is evaluated, never its result |
| Two-pass trigger evaluation | Each evaluation's entry range, entries and answer state are random loads | The first pass loads the ranges and prefetches each first entry; the second evaluates them and fetches the state 2 ranges ahead | Prefetches read nothing the program uses; the second pass evaluates exactly the ranges the first kept |
| Popcount sums and first ids | Count, id sum and ten smallest ids without a loop over documents | Per document, `state += hit * (1 << 40 \| doc)`; per superblock, count = popcount and sum = `base * popcount(w) + sum_k 2^k * popcount(w & M_k)`, AVX2 nibble tables for 256 documents; first ids by trailing-zero counts while fewer than ten | Integer identities; per-thread sums below 2^39 (checked) |
| Locality by numbering | Hot stamps, rows and answer state scattered over cache lines | Terms renumbered by kind and sampled frequency; answers numbered in the order the scan updates them | Both are bijections (`answer_of`, `line_of`); they change where state lives, not what it holds |
| Phrases on their rarest term | Checking 19,000 phrases per document | Each phrase is anchored on its rarest sampled term, as a 64-byte record checked by one masked 32-byte compare against the document's token ids while they are in L1 | A phrase match contains its anchor, so every match is checked; a document is counted once |
| Setup without page faults | Once setup ran in parallel, its threads mostly waited on page faults of fresh, growing vectors (on the test server the setup spent more CPU time in the kernel than in the program, 34.6 of 58 ms) | Compact pre-sized records, grouping by counting sort, a stable counting sort for the term order | Records reload each key from the same file bytes; counting sorts are permutations, which change the order terms are numbered in, never what a term is |
| Measured on the box | Where the box spends its time cannot be profiled there | Exit-code probes, in-process A/B probes and, from Sun 13:20 UTC, a replica of the box (PERF.md, "How we measured") | Measurement only; never pushed as a scored version |
| Build | Build-to-build speed spread of 2.5 to 6% (steps 27, 32) | PGO with atomic counters, fat LTO, `crt-static`, `target-cpu=native`; `cmov` in `probe` and `vpmovmskb` as inline asm where LLVM would branch or rebuild masks bit by bit. No loop or function alignment flags (PERF.md, steps 38 and 52's experiments) | Flags and instruction choice only |

### Index

One line per technique: what, where in `src/main.rs`, and why the output stays exact. The table
above groups them by bottleneck.

1. No index: queries read first, all answered in one scan (`main`). Exact: every query is evaluated on every document that can match it.
2. Memory-mapped files with a 256-byte zero pad (`mmap::map`): full-width loads at any offset. Exact: zero is a separator, never a token byte.
3. Transparent huge pages off (`mmap::no_huge_pages`): a stable memory peak. Exact: no effect on the bytes.
4. Slices of whole lines, one per thread (`slices`, `line_after`). Exact: every cut follows an LF, so each document is in one slice, and each slice's last id must meet the next slice's first.
5. Query lines split in parallel into pre-sized 8- and 16-byte records (`split_lines`). Exact: each key is reloaded from its own bytes.
6. Operator from the low two bits of its first byte (`A`, `N`, `O`; `split_lines`). Exact: the grammar has only these three words.
7. Stable counting sort `csr` for every grouping (keys by group, term order, entries, anchors), written from each run's end into a vector's spare capacity, last value first. Exact: a permutation, and every slot is written once (the counts add up to the length).
8. PTHash-style perfect hash with per-bucket pilots (`Vocab`, `place`): one slot per term. Exact: the hash says where to look; the probe decides (16).
9. Two AES rounds as the hash, the seed (a × `SEEDS` for attempt a) in the first round key (`hash`): three instructions per token. Exact: a weak or clashing hash only fails the build, which re-seeds (13).
10. Pilots of 1 mod 64, so each of 64 groups owns a table region: a lock-free build on every thread (`Vocab::new`). Exact: no thread writes another's region; the barrier orders the read-back.
11. Largest buckets placed first, first free pilot taken (`find_pilot`). Exact: a fixed order, so the same table on every run and thread count.
12. Sort key rotated so the bucket bits lead (`dedup_keys`, `rotate_right(bmask.count_ones())`): sort by bucket, then hash, in one key. Exact: a rotation is a bijection.
13. Equal-hash check on the key bytes (`dedup_keys`); a bucket with no free pilot goes first in its thread's next try, up to four tries (`Vocab::new`); a clash or a fourth failure re-seeds, every third seed doubles the table. Exact: a failed build never answers, and a key that a failed try left in the table matches no token (its own term looks up its final slot).
14. Terms of 13 to 32 bytes in a cold `HashMap`, with their slots after the hash's (`get_long`). Exact: compared byte for byte; tokens over 32 bytes find nothing.
15. 16-byte slot: 12 key bytes under a 4-byte info word (id, ANCHOR, TRIG). Exact: 12 bytes are the whole key of every term the hash holds.
16. Branch-free probe: one aligned load, XOR, `vptest`, inline-asm `cmov` of the info word, loaded on its own (`probe`). Exact: the full key is compared; the hash only says where.
17. Token key from one 16-byte load, OR 0x20 and a sliding `KEEP` mask (`key_at`). Exact: lowercases A to Z, keeps digits, zero padding separates lengths.
18. AVX2 token and LF bitmaps per 64 bytes, signed compares, letters tested on `v | 0x20` (`masks`). Exact: a token byte is exactly `[A-Za-z0-9]`.
19. `vpmovmskb` as inline asm (`movemask`). Exact: the same instruction, without LLVM's bit-by-bit rebuild.
20. Slice end clipped with `bzhi` (`masks`). Exact: bytes past the slice are not tokens here.
21. Windows that end at their last non-token byte, so no token crosses one: token starts `mask & !(mask << 1)` clipped there with `bzhi`, no carry (`Scan::run`); tokens of 63 bytes or more on a cold path (`long_run`). Exact: every token is scanned whole, in one window.
22. Token length from the start and last-byte masks, one `tzcnt` each. Exact: the distance from a token's first byte to its last.
23. Slot's byte offset computed and prefetched when the token is found; a document's tokens all found before any is probed (`pending`, `look_up`): the misses overlap. Exact: a prefetch reads nothing the program uses.
24. Writes into reserved capacity through raw pointers, no capacity checks (`pending`, `toks`, `trig`, `work`). Exact: each loop reserves room for the most it can write (for example `reserve(32)` per 64-byte window), as its SAFETY comment argues.
25. One-byte stamp of each term's last document, written for every token (`lastdoc`, `STAMPS`). Exact: an equal stamp is always this document's (design 3).
26. Every trigger occurrence written branch-free, each term taken once per document by its DONE mark (`look_up`, `trigger`). Exact: the first occurrence keeps its range; later ones are written over.
27. Non-row tokens all land on one sink row: the row index is `id.min(rows)`, which LLVM compiles to `cmovb` (checked in the PGO binary), so there is no branch on the kind. Exact: the sink is no query's row.
28. Cover sets: a set of terms every match contains, cheapest by sampled frequency (`cover`). Exact: a document without a cover term holds no match.
29. Cost model `1 - (1 - df / n)^64`: the share of 64-document blocks that contain a term (`Plan::new`). Exact: it only chooses the path a query takes.
30. A 1 MB sample spread over the slices for document frequencies (`SAMPLE_BYTES`). Exact: it only decides where a query is evaluated.
31. Truth tables per (query, cover term), made once at planning time (`doc_entry`). Exact: bit a is the spec's left-to-right fold of AND, AND NOT and OR with exactly those terms present.
32. The earlier-cover rule inside the table: zero where a cover term before c is present. Exact: each matching document is counted by the entry of its first present cover term, once.
33. Entries padded to 5 terms with a term no document has. Exact: a padded term is always absent, and the table only reads the bits of real terms.
34. Two-pass trigger evaluation: ranges loaded and first entries prefetched, then evaluations with the state 2 ranges ahead fetched (`trigger`, `RANGES_AHEAD`). Exact: the second pass evaluates exactly the ranges the first kept.
35. Common queries per 256-document superblock, one 256-bit AVX2 row per term, its four words kept by block (`wide`, `end_super`, `ALWAYS`). Exact: bit i of a row is "document base + i contains the term", so the word operations are the spec's set operations.
36. Superblock queries sorted by length and operators so the operator branch predicts. Exact: the order of evaluation only.
37. A term's four block words gathered into one vector in `end_super` (`_mm256_set_epi64x`): each token's store goes to its block's contiguous word array, and the superblock reads four words per term. Exact: the same words, one bit per document.
38. Terms renumbered by kind and sampled frequency: hot stamps and rows share cache lines (`term_order`). Exact: a bijection.
39. Answers numbered in the order the scan updates them (`Plan::number_answers`, `answer_of`, `line_of`). Exact: a bijection.
40. Per-document count and sum as one packed add, `hit * (1 << 40 | doc)` (`Answers::add_doc`). Exact: an integer identity.
41. Superblock count by popcount; id sum as `base * popcount + sum_k 2^k * popcount(w & M_k)` (`Answers::add`). Exact: an integer identity.
42. The six masks `M_k` computed as constants by one expression (`digit`). Exact: `M_k` has bit i set exactly when bit k of i is set.
43. AVX2 nibble tables (`pshufb`) for count and position sums of 256 documents, reduced by `maddubs`/`madd` (`count_positions`). Exact: sums stay below 2^15 per lane.
44. First ids written branch-free, 4 bytes each, and kept only while fewer than ten (`add`, `add_doc`). Exact: documents are scanned in order and bits come out in increasing order, so the first ten logged are the ten smallest of the slice; an id is rebuilt from its low 16 bits and its slice's first id, exact while a slice holds fewer than 2^16 documents: at least 8 slices (`MIN_SLICES`), and an assert stops a larger input.
45. Count and id sum packed in one `u64` (`SUM_BITS`), so threads' states add as plain integers. Exact: sums below 2^39, checked by an assert.
46. Phrases anchored on their rarest sampled term, as 64-byte records (`Phrase`), checked at document end while the ids are in L1 (`end_doc`). Exact: every match contains its anchor.
47. The anchor's records prefetched when its token is looked up, and its first record fetched before the BOOL evaluation (`look_up`, `end_doc`). Exact: prefetches read nothing the program uses; the phrases update other answers than the BOOL entries, so the order does not matter.
48. One masked 32-byte compare per phrase, 8 ids from its start (`end_doc`). Exact: `bzhi` keeps exactly the phrase's terms in the compare mask, and the 8 `miss` ids that `look_up` writes after the document's tokens match no term, so a phrase never matches past the document's end.
49. Per-phrase last-document marker, so a document counts once. Exact: a second match in the same document is not counted.
50. Answer state written at allocation (`written_zeros`, `black_box`): a lazily zeroed page read first would fault twice. Exact: it writes the zeros the state starts with.
51. Each slice's pages dropped by its own thread (`mmap::release`). Exact: a slice is not read again after its scan; a page shared with a neighbour faults back in from the page cache.
52. First-id logs put in line order per thread by a prefix sum of the logged counts and one scatter, no sort (`main`); merged in slice order; the later threads skip the answers whose first slice alone has ten (`full`). Exact: each answer logs its ids in order, and slices are in document order.
53. Output formatted in parallel, states read in place through `answer_of` (`format_part`; no 4.5 MB copy). Exact: each line reads its own states.
54. Exit right after writing, no frees. Exact: results.txt is complete before the exit.
55. Slice edges need no special case. Exact: a document outside a thread's slice has no stamps and no row bits there.
56. Profile-guided build trained on generator seed 7, with atomic profile counters (Dockerfile). Items 56 and 57 change speed only: every check runs the Dockerfile's binary.
57. No alignment flags (the replica: `-align-loops=64` +0.24%, `-align-all-functions=6` 0.00%); fat LTO, one codegen unit, `panic = "abort"`, `crt-static`, `target-cpu=native`, `compile_error!` without AVX2, BMI2 or AES.
58. Exit-code and in-process timing probes on the box (PERF.md, "Box probes since step 47"). Measurement only, never a scored version.
59. No dependencies: libc calls declared by hand.
60. One persistent pool of threads behind `par` (`worker`, `JOB`, `GENERATION`, `FINISHED`, `BUSY`): the first call spawns the workers, and each later call publishes a job and waits for every worker to count it done; an assert allows one call at a time. Exact: `par` returns only after every part is done, and no worker uses the job after the call returns (its SAFETY comment).
61. Workers wait in `umwait` on the generation word (`wait`), woken by a write to its cache line or by the kernel's time limit; without WAITPKG, a polling loop. Exact: a wait ends only when the word shows the next job.

## Why the output is exact

- **Tokens.** The key keeps exactly the token's bytes (a mask of its length) and ORs 0x20 into
  them, which lowercases A to Z and leaves digits unchanged. Token bytes are never zero, so the zero
  padding makes keys of different lengths different. Longer terms are compared byte for byte; a
  token over 32 bytes cannot be a term (the grammar caps terms at 32 bytes) and finds none.
- **The perfect hash decides where to look, never whether a token matches.**
  - *Lookup.* A token's slot is computed with the same seed, pilots and shift the build used, and
    the probe compares the token's full 12-byte key with the key stored there.
  - *Hits.* A query term therefore finds its own slot, which holds its key. The builder gave every
    distinct key its own slot: a slot is claimed once, and a bucket's keys are placed only when all
    their slots are free.
  - *Misses.* Any other token lands on an empty slot, whose zero key no token has, or on another
    term's slot, whose key differs: MISS either way.
  - *Failures.* A bad hash or seed can only make a placement or the build fail. A failed placement
    is tried again by its thread, and a failed build with another seed. A key that a failed try left
    in the table matches no token: its own term looks up its final slot, and every other token's
    key differs. So a failure cannot change a lookup.
  - *Determinism.* The groups, the bucket order (by size, with a deterministic sort) and the id
    numbering do not depend on thread timing, and each thread writes only its own regions of the
    table before the barrier.
- **BOOL, per document.**
  - *The table.* An entry for (query q, cover term c) lists q's other distinct terms t_0..t_{k-1},
    k ≤ 5. For each a below 2^k, bit a of its table is q's value, folded left to right with the
    spec's AND, NOT and OR, in a document that contains c and exactly the t_i whose bit i is set in
    a. It is 0 if a cover term before c is among them.
  - *The lookup.* At a document's end, bit i of the presence index is "the stamp of t_i is this
    document's". So the looked-up bit is q's value in this document, unless an earlier cover term is
    present. Padded terms are in no document, so their bits are 0, as the table assumes.
  - *Each document once.* Every match of q contains at least one cover term, and the entry of the
    first one present (in cover order) counts it; every other entry's table is 0 there. A document
    with no cover term holds no match. A trigger term that occurs several times in a document is
    taken once: the first occurrence marks it DONE.
- **Stamps.** A document's stamp is its id mod 127, and all stamps are reset to 0xFF every 127
  documents (the slice's first window starts reset). Within a window each stamp value belongs to
  one document, so a term's stamp equals this document's only if the term occurred in it. 0xFF,
  with the DONE bit masked off, is 127, which no stamp equals.
- **Covers.** A document without any cover term of a query holds no match of it, so skipping it is
  exact.
- **Superblocks.** Bit i of a row word is "document base + i contains the term", so the word
  operations are the spec's set operations, applied in the query's order. The first term is ORed
  into zero, so a document of another slice, whose bits are 0 in this thread's rows, matches
  nothing here.
- **Two passes and prefetches.** A prefetch reads nothing the program uses, and the second pass
  evaluates exactly the ranges the first pass kept, in the same order.
- **First ids in 4 bytes.** A log entry is `answer << 16 | id & 0xFFFF`. An id is rebuilt as
  `first + ((entry − first) & 0xFFFF)`, where `first` is its slice's first id: exact whenever
  0 ≤ id − first < 2^16. The scan only ever logs its own slice's documents (other slices' bits are
  0 in its rows and stamps), and after each scan an assert checks that the slice holds fewer than
  2^16 documents and that there are at most 2^16 answers. At least 8 slices (`MIN_SLICES`) keep a
  slice at most near 31,000 of the spec's 250,000 documents. A larger input stops the program; it
  never gets a wrong id.
- **Phrases past the document's end.** The compare always reads 8 ids from the phrase's start.
  Past the document's last token they are the 8 `miss` ids `look_up` writes there, which match no
  phrase term.
- **The pool.** `par` returns only after every part has stored its result, and an assert allows
  one call at a time (`BUSY`), since its job slot and counters serve one job. The later scans' skip
  of `full` answers waits for the first slice's scan, which runs first on the calling thread, so
  the wait always ends. Every result is the same as with one thread per part (the gate runs 1, 3,
  7 and 12 CPUs).
- **Answer numbers** are a bijection. Every query line is a phrase, an `always` query, or has at
  least one entry (a cover has at least one term), and each line gets a number the first time it
  is met. So `answer_of` and `line_of` are inverse, and every output line reads its own state and
  log entries.
- **The sample** only decides where each query is evaluated (per document or per superblock, and
  which anchor), never its result. Its size (1 MB) and ALWAYS are speed choices.
- **Setup records.** A key record holds the offset and length of the term's bytes, and the key is
  loaded again from those bytes whenever it is needed, so the hash, the equal-hash check and the
  stored key are those of the term's own bytes. `csr` groups and the counting sorts are stable
  permutations; they decide the order in which terms and entries are numbered, which only moves
  state around.
- **Sums fit.** Each thread packs its count and its id sum (below N^2 / 2 < 2^39 for N < 2^20,
  which the program checks) into one u64.
- **Checks** before every push, listed in PERF.md ("How the final number was verified"):
  - the seed-1 hash at several thread counts;
  - byte comparison with the reference on sample seeds 2 to 4 and holdout seed 99;
  - 144 tiny, fresh and adversarial runs: token lengths 1 to 70, 1-term phrases, empty answers,
    and the tiny inputs (1,000 queries).

## Robust on unseen seeds

Nothing is fitted to seed 1's data except the ALWAYS threshold (PERF.md, steps 40, 44 and 53):
the plan is made per input, from a 1 MB sample of that input's own documents. What could change with the seed, and why it does not change the answer:

| What could change | What the program does |
|---|---|
| Term frequencies | The sample decides each query's path and anchor, never its result (covers, design 3) |
| The query terms | The perfect hash is built per input; a bucket with no free pilot is placed again by its thread, a clash re-seeds, every third seed doubles the table, and the probe always compares the whole key. Over 300 hash seeds on each of 32 datasets (9,600 builds), no build needed a second seed; before step 58, 257 of 9,600 did (PERF.md, step 58) |
| Document count and ids | Checked: ids 0, 1, 2, ... below 2^20, and sums below 2^39 (an assert) |
| Token lengths | Terms of 13 to 32 bytes go to the long map; longer tokens cannot be terms |

Evidence, on the binary the Dockerfile builds:

- **Holdout seed 99**, full scale, byte-equal to the reference at every gate since step 33,
  including the submitted code's (PERF.md, "How the final number was verified").
- **Fresh seeds**, generated for the gates and compared with `reference/b_index.py` at 1, 3 and 8
  CPUs: tiny 801-806 and 911-914, and sample 51 and 52 (step 49); earlier tiny 701-706 and sample
  41-43.
- **Robustness runs on the replica** (Sun 1 Nov, about 17:00 UTC), on the exact Dockerfile build of
  step 59 (steps 60 to 62 change lines, checks and a prefetch, not the output path, and passed the
  gate), with data generated on the replica by the organisers' generator (Python 3.11, which
  reproduces the official sample seed 1 and its reference output byte for byte). Every run was
  byte-exact, with no crash, no nonzero exit and no output on stderr:
  - 30 fresh full-scale seeds (101-130) at 12 threads, seeds 101-102 also at 1 and 3 CPUs and
    seeds 103-104 at 2, 7 and 11: 40 of 40 runs;
  - 20 sha256-checked repeats per seed at 12 threads, and runs with a busy loop on 1, 2 or 4 of
    the twelve CPUs (seeds 109, 110 and 113): 660 of 660;
  - tiny and sample seeds 201-210 at 1, 2, 3, 7, 12 and 20 CPUs, each run as root and as uid
    65534 (the board's privilege drop) with read-only data, 3 repeats, and full scale at 12 and
    20 CPUs for both users: 840 of 840;
  - copies of seed 101 whose every file is an exact multiple of 4096 bytes, so that a read past
    the end of a file would fault, at 1, 3, 7, 12 and 20 CPUs: 5 of 5.
- **The perfect hash, forced.** The per-region retry ran end to end from forced start seeds: in
  300 runs, 20 placed a region again, and all were exact.
- **Time and memory on other seeds.** On the 30 fresh seeds step 59 ran 84.73 to 88.18 ms (medians
  of 7 runs; seed 1: 85.47 ms), and its memory peak on seeds 101-110 was 30.11 to 31.63 MB (max of
  5 runs; seed 1: 30.03 MB), under the 44 MB of full credit. Earlier, the block engine of step 46b
  ran three full-scale seeds within 0.5% on the test server (PERF.md).
- **Not covered:** more than 20 threads (the replica has 20 CPUs), and the table-doubling path,
  which now needs four failed placements of a region and then a failed hash seed.

## Error handling

Errors are panics with a message, and the build aborts on panic, so the exit status is nonzero:
- a missing file, or a failed mmap;
- a docs.txt line without a TAB, or an id that is not all digits or is over 9 digits;
- document ids that are not 0, 1, 2, ... below 2^20, each before a TAB;
- a slice whose last id does not meet the next slice's first;
- a slice with 2^16 documents or more, or more than 2^16 queries (the 4-byte first-id log's
  limits; the spec's input has 31,000 documents a slice at most, and 50,000 queries);
- a phrase of over 8 terms (the spec's have 2 to 8);
- a second call to `par` while one runs (the program calls it from the main thread only).

A line that starts with a separator byte (an empty id, for example) has no id token: its text
joins the previous document. That is caught when it breaks the id sequence (the next line's id
check), but not when it is an extra line or the last line, where the answer is silently wrong.

queries.txt is trusted to follow the spec's grammar (2.3): a malformed line either panics on a
slice bound or is answered as parsed (an unknown operator reads as AND).

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise` and `prctl` declared by hand
(`dependency_count = 0`).
- *CPU.* The build needs AVX2, BMI2 and AES-NI (`-C target-cpu=native`; the board builds this
  track on the scoring box).
- *PGO.* The Dockerfile runs a profile-guided build, trained on data from a byte-identical copy of
  the organisers' generator (`b_index.py`, not counted as code). It uses LLVM's atomic profile
  counters (`-instrprof-atomic-counter-update-all`), so the twelve-thread training run loses no
  counts and the profile is repeatable.
- *No alignment flags.* B builds without loop or function alignment flags: on the replica they
  were 0 to 0.4% slower.

## With another 24 hours

Measured or modelled on seed 1; none of this was built.

- **The row ORs.** Every token sets its document's bit in its term's row, and 37% of the tokens go
  to one sink row: those writes cost about 10.7 ms of step 53's scan on the replica. Rotating sink
  rows, a branch for row terms and sending repeated terms to the sink all lost (PERF.md,
  experiments). It is the largest store cost left, and we have no design for it yet.
- **Setup is still about 11 ms of 88.** The plan alone was 5.7 ms on step 49. Tokenizing needs only
  the perfect hash, not the plan, so the scan could start as soon as the hash is built, buffering
  each thread's first documents until the plan is published: about 3 to 4 ms, by our estimate.
- **The mispredicts that remain** (replica counters on an earlier stage of step 52):
  - the extraction loop's exit, 4.0M (cheap, see PERF.md's failed branch-free extraction);
  - the ANCHOR branch on the probe's info word, 1.1M, which resolves late;
  - the per-range entry loop, 1.0M;
  - the superblock's first-id logging, 0.6M.

  The ANCHOR branch is the one worth a branch-free form, but the replica measured our first one
  (q14an) 1.2 ms slower.
- **A denser perfect hash.** The table runs at a load of 0.58 (2 MB). Placing buckets at a load
  near 0.9, as PTHash does with more pilot candidates, would fit it in 1.25 MB of slots and cut
  the misses of the scan's one random load per token. The build would take longer, on twelve
  threads. Untested.
