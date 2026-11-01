# Track B: Index

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).
The step-by-step history, with a board run for every step, is in [PERF.md](PERF.md).

## The problem

250,000 documents (254 MB) and 50,000 queries known up front: 31,000 BOOL queries of 1 to 6 terms,
evaluated strictly left to right, and 19,000 phrases of 2 to 8 terms. Each answer is the match count,
the sum of the matching ids and the ten smallest ids.

## The bottleneck

- **Memory, first.** The obvious design, an inverted index of the query terms, holds about 21M
  (term, document) pairs for the BOOL terms alone. However it was packed (steps 1 to 5 in PERF.md),
  the run peaked at about 200 MB. The M axis gives full credit only under 44 MB (the 50x cap on the
  reference's 2,200 MB), so at 200 MB memory alone cost about 6.6 points.
- **Then the token path.** Without an index, the work is one pass over 36.3M tokens: 76% of them are
  BOOL query terms, 3% are phrase-only terms and 21% are words no query uses, and each one must be
  identified exactly. On the box, timing probes of step 10 (PERF.md) put the serial setup at 26 ms, the scan at
  140 ms and the tail (merge, format, write, exit) at about 4 ms. Probes of step 31's code (PERF.md,
  "Timing probes") put the scan's end at 118 ms, and at 79 ms with the BOOL evaluation turned off: the
  BOOL evaluation costs about 39 ms on the box, twice its 19 ms on the test server. A probe of step 32's
  code put the end of setup at 16.5 ms, the same as the test server's 16.4 to 16.8 ms; step 33 cut it
  to 11.6 ms there.
- **Then stalls, not instructions.** Once the token path was lean, what the box paid for was waiting
  on loads whose address is random: the term table's slots, and at each block's end the entry
  ranges, entries, presence words and answer state of the queries a block triggers. Steps 20 and 28
  of PERF.md, which fetch those ahead, gained on the box; cuts of instructions alone (steps 18 to 19)
  or of L2 misses alone (steps 23 to 24) did not.

## The design

There is no index. The queries are read first, the corpus is scanned once in parallel slices of whole
lines, and every query is answered during that scan, so nothing grows with the corpus: the board
measures 40.1 to 42.2 MB, under the 44 MB credit.

1. **Setup, in parallel.** About 35 ms of work on one thread of the test server, so all of it runs on every thread, and
   what it mostly waits on is page faults of fresh memory, so it allocates little:
   - *Split.* Each thread splits a range of query lines into compact records: 8 bytes per term (its
     offset in the file, and its length | op << 30) and 16 bytes per key (its hash, offset and key
     length; the key bytes are reloaded from the file when needed). The vectors are sized for the
     most terms a chunk can hold, so they never grow and never copy; their untouched pages cost
     nothing. The keys are grouped by hash group (`hash & 63`) with the same counting sort, `csr`,
     that the plan uses.
   - *Perfect hash* of the terms, built on every thread (design 2). The ids are then read back for
     every term; the map of long terms is consulted only for terms over 12 bytes (it hashes every
     byte, and asking it for all 155,000 terms took most of the read-back's time).
   - *Sample.* The start of every slice, 1 MB in total (about 1,030 documents on seed 1), is scanned
     to estimate each term's document frequency.
   - *Plan*, a range of query lines per thread: whether each line is an `always` query (design 4), with
     each term's kind set by an atomic OR; the term order (BOOL terms first, so that they index dense
     per-thread arrays) by a counting sort through `csr`; each part renumbers its own terms; `csr`
     groups the entries and anchors straight from the parts' vectors, without merging them first.
   - *Memory back.* glibc keeps freed blocks resident once it has seen large ones freed, which left
     about 4 MB of setup memory in the peak; `malloc_trim(0)` before the scan returns it (0.2 ms).
2. **Finding and looking up tokens.** Each 64-byte chunk is classified with AVX2 into a token-byte
   mask and an LF mask; token starts and lengths come from bit operations on them. A token's key is
   its first 12 bytes, lowercased and zero-padded in one 16-byte register. The query terms of up to
   12 bytes (75,735 distinct keys on seed 1) are in a **perfect hash**, so a token is compared with one
   16-byte slot, never more:
   - *Hash.* Two AES rounds of the key (`aesenc` twice, the seed XORed into the first round key),
     the low 64 bits kept. After two rounds every output bit depends on every key byte, and a
     different seed gives an unrelated hash; it costs three instructions per token.
   - *Pilots* (PTHash style). A hash h falls in bucket `h & bmask` (16,384 buckets on seed 1, about
     4.6 terms each), and the bucket's 16-bit pilot m places it in slot
     `(h * m).rotate_right(6) >> shift`. The builder takes the largest buckets first and gives each
     the first pilot whose slots are all free, so every term gets a slot of its own.
   - *Parallel build.* Every pilot is 1 mod 64, so the low 6 bits of `h * m` are those of h (its
     *group*), and the rotation makes them the slot's top bits: each of the 64 groups owns one
     region of the table, whatever its pilots. Each of the 12 threads sorts, deduplicates and places
     a range of groups in its own regions, with no locks; a barrier, then each thread reads back the
     ids of its share of the query lines.
   - *Slot.* 16 bytes: the term's 12 key bytes under its 4-byte info word (id and ANCHOR flag). The
     table is 2^17 slots (2 MB, a load of 0.58) plus one slot per long term and a final MISS slot;
     empty slots are left zero. The slot is computed and prefetched as soon as the token is found,
     and a document's tokens are all found before any is looked up, so the misses of one document
     overlap.
   - *Probe.* One aligned 16-byte load, an XOR with the token's key, a `vptest` of the 12 key bytes
     and a `cmov` that selects the info word or MISS. There is no branch: whether a token is a query
     term is too random for the branch predictor.
   - *Re-seed.* If two different keys of a group have the same 64-bit hash, or a bucket finds no free
     pilot among its 1,024 candidates, the build fails and runs again with the next seed; every third
     seed also doubles the table. Seed 0 succeeds on full seed 1, the holdout seed 99 and sample
     seeds 1 to 4. The 1,000-query `tiny` inputs have about 10 keys per bucket instead of 4.6 (the
     bucket count follows the term occurrences, the table the distinct terms) and usually succeed at
     seed 3, the first doubling; the output is the same whichever seed succeeds.
   Terms of 13 to 32 bytes (0.2% of the tokens) go to a cold hash map; their slots sit after the
   hash's, so the scan treats both alike.
3. **BOOL queries, per block of 64 documents.** While a block is scanned, each BOOL term present gets
   a 64-bit word with one bit per document. At the block's end, AND, OR and NOT are word operations in
   the query's order. A query is evaluated only in blocks that contain one of its *cover* terms: a set
   of terms of which every match contains at least one, chosen to be rare in the sample. On seed 1
   that is 4.27M block evaluations (counted with the 2 MB sample used up to step 33), against 121M for
   every BOOL query in every block.
   The block end runs in two passes, because the slots touched in a block come in first-seen order,
   so every load it makes is random. The first pass walks the touched slots, keeps the entry ranges
   of those that trigger any query (about a third), and prefetches the range table 16 slots ahead
   and each kept range's first entry. The second pass evaluates the ranges and, 4 ranges ahead,
   prefetches the presence words and the answer state that the next range's first entry reads. The
   evaluation itself has no branch: the operator is folded into two masks per step.
4. **Common queries, per superblock of 256 documents.** The 5,882 queries whose cover terms are
   expected in more than a quarter of the blocks (summed over the terms) would be evaluated in almost
   every block anyway. They are evaluated once per 256 documents instead, with AVX2 on four words per
   term, and only over their real terms: they are sorted by term count and operators, so the operator
   branch predicts.
5. **Answers numbered in evaluation order.** Each thread keeps one u64 of state per query (count and
   id sum) and a log of first ids. Queries are numbered in the order the scan updates them: the
   `always` queries first, so that every superblock reads and writes their state as one 47 KB
   stream instead of about 4,000 scattered cache lines, then the block entries in table order,
   then the phrases. `answer_of` and `line_of` map between query lines and these numbers.
6. **Counts, sums and first ids without a loop over documents.** The count is a popcount. The sum of
   the matching ids is `base * popcount(w) + sum over k of 2^k * popcount(w & M_k)`, with six fixed
   masks M_k (M_0 = 0xAAAA..., the odd positions); for 256 documents at once, nibble lookup tables do
   the same in AVX2. First ids come from trailing-zero counts, and only while a query has fewer than ten.
7. **Phrases.** Each phrase is anchored on its rarest term. When a document ends, the anchors seen in
   it are checked against the document's term ids, which are still in L1.
8. **Merging.** Each thread maps its first-id log back from answer numbers to line numbers and sorts
   it. The slices are in document order, so the logs merge in slice order into the ten smallest ids.
   Output lines are formatted in parallel; each formatter adds up the threads' counts and sums by
   reading their states through `answer_of`, without first copying them into line order (that copy
   cost about 4.5 MB and put the run over the 44 MB memory credit, step 26). The process exits right
   after writing instead of freeing its tables.

## Techniques

What limits this track is waiting on loads whose address is random, not bandwidth: one per corpus
token into the term table, and a burst at each block's end into the query entries, presence words
and answer state. Each technique below names the cost it attacks, what the code does, and why the
output stays the same.

| Technique | Bottleneck | What the design does | Why the output is unchanged |
|---|---|---|---|
| No index; queries first | An index of the query terms holds 21M (term, document) pairs: 85 to 200 MB, past the 44 MB credit (steps 1 to 5) | All 50,000 queries are read first and answered during one parallel scan; nothing grows with the corpus | Every query is evaluated on every document that can match it (covers, below), with the spec's set operations |
| Perfect hash, AES-hashed | One random probe per token; the old 4-way buckets compared four keys and spilled into the next bucket | PTHash-style pilots give every term of up to 12 bytes its own 16-byte slot; the hash is two `aesenc` rounds; the probe is one load, an XOR, a `vptest` and a `cmov` | The probe compares the token's whole key with the slot's; the hash only says where to look. A failed build re-seeds; it never yields a table that answers wrongly |
| Lock-free parallel hash build | Sorting, deduplicating and placing 155,000 term keys takes about 10 ms on one thread of the test server | Pilots are 1 mod 64, so each of 64 hash groups owns a region of the table and 12 threads place their groups without locks | Groups, bucket order and ids do not depend on thread timing; each thread writes only its regions before the barrier |
| SIMD tokenizer, prefetched lookups | 36.3M tokens to find and look up | AVX2 classifies 64 bytes into token and LF bitmaps; starts and lengths are bit operations; each slot is prefetched when its token is found, and a document's tokens are all found before any is probed | A token is exactly a maximal run of `[A-Za-z0-9]`; the key is its bytes OR 0x20 and masked to its length |
| Bitset evaluation per 64 documents | 31,000 BOOL queries evaluated document by document | Each term present in a block gets a 64-bit word; AND, OR and NOT are word operations in the query's order, branch-free | Bit i is "document base + i contains the term", so a word operation is the spec's set operation on 64 documents at once |
| Cover triggers from a sample | 121M evaluations if every query ran in every block | From a 1 MB sample, each query gets a cover, a set of terms every match contains; it is evaluated only in blocks with a cover term (4.27M evaluations); queries with common covers run per 256 documents with AVX2 | A block without a cover term holds no match. The sample only decides where a query is evaluated, never its result |
| Two-pass block end | Each evaluation's entry, words and state are random L3 loads | The first pass collects and prefetches the touched terms' entry ranges; the second fetches words and state four ranges ahead | Prefetches read nothing the program uses; the second pass evaluates exactly the ranges the first kept, in order |
| Popcount sums and first ids | Count, id sum and ten smallest ids without a loop over documents | Count = popcount; sum = `base * popcount(w) + sum_k 2^k * popcount(w & M_k)`, AVX2 nibble tables for 256 documents; first ids by trailing-zero counts while fewer than ten | Integer identities; per-thread sums below 2^39 (checked) |
| Locality by numbering | Hot presence words and answer state scattered over cache lines | Terms renumbered by kind and sampled frequency; answers numbered in the order the scan updates them | Both are bijections (`answer_of`, `line_of`); they change where state lives, not what it holds |
| Phrases on their rarest term | Checking 19,000 phrases per document | Each phrase is anchored on its rarest sampled term and checked against the document's token ids while they are in L1 | A phrase match contains its anchor, so every match is checked; a document is counted once |
| Setup without page faults | Once setup ran in parallel, its threads mostly waited on page faults of fresh, growing vectors: on the test server the setup still spends more CPU time in the kernel than in the program (34.6 of 58 ms) | Compact pre-sized records, grouping by counting sort, a stable counting sort for the term order, `malloc_trim` before the scan | Records reload each key from the same file bytes; counting sorts are permutations, which change the order terms are numbered in, never what a term is |
| Exit-code probes | Where the box spends its time cannot be profiled | Temporary builds exit with an elapsed time as their status (setup end, scan end, scan end without BOOL evaluation) | Measurement only; never pushed as a scored version |
| Build | Build-to-build speed spread of 2.5 to 6% (steps 27, 32) | PGO with atomic counters, hot loops aligned to 64 bytes, fat LTO, `crt-static`, `target-cpu=native`; `cmov` and `vpmovmskb` as inline asm where LLVM would branch or rebuild masks bit by bit | Flags and instruction choice only |

## Why the output is exact

- **Tokens.** The key keeps exactly the token's bytes (a mask of its length) and ORs 0x20 into them,
  which lowercases A to Z and leaves digits unchanged. Token bytes are never zero, so the zero padding
  makes keys of different lengths different. Longer terms are compared byte for byte; a token over
  32 bytes cannot be a term (the grammar caps terms at 32 bytes) and finds none.
- **The perfect hash decides where to look, never whether a token matches.** A token's slot is
  computed with the same seed, pilots and shift the build used, and the probe compares the token's
  full 12-byte key with the key stored there. A query term therefore finds its own slot, which holds
  its key: the builder gave every distinct key its own slot (a slot is claimed once, and a bucket's
  keys are placed only when all their slots are free). Any other token lands on an empty slot, whose
  zero key no token has, or on another term's slot, whose key differs: MISS either way. A bad hash
  or seed can only make the build fail, and a failed build is retried with another seed; it cannot
  change a lookup. The build itself is deterministic: the groups, the bucket order (by size, with a
  deterministic sort) and the id numbering do not depend on thread timing, and each thread writes only its own
  regions of the table before the barrier.
- **BOOL.** Bit i of a word is "document base + i contains the term", so the word operations are the
  spec's set operations, applied in the query's order. Block queries are padded to six steps with AND
  of an all-ones word, which changes nothing.
- **Covers.** A block without any cover term of a query holds no match of it, so skipping it is exact.
  A block with several of them is still evaluated once: each cover term's entry knows the earlier
  ones and stands down when one of them is present: it adds an empty result, which changes no
  count, sum or log.
- **Two-pass block end and prefetches.** A prefetch reads nothing the program uses, and the second
  pass evaluates exactly the ranges the first pass kept, in the same order; a range the first pass
  dropped is empty. The result of a block is the same as with one pass.
- **Answer numbers** are a bijection: every query line is a phrase or has at least one entry (a cover
  has at least one term), and each line gets a number the first time it is met, so `answer_of` and
  `line_of` are inverse and every output line reads its own state and log entries.
- **The sample** only decides where each query is evaluated (per block, per superblock, which anchor),
  never its result; so its size (1 MB since step 34) is a speed choice.
- **Setup records.** A key record holds the offset and length of the term's bytes, and the key is
  loaded again from those bytes whenever it is needed, so the hash, the equal-hash check and the
  stored key are those of the term's own bytes. `csr` groups and the counting sorts are stable
  permutations; they decide the order in which terms and entries are numbered, which only moves
  state around. `malloc_trim` returns free memory only.
- **Slice edges.** A block may straddle two slices. A document outside a thread's slice has a zero bit
  in every word there, and a left-to-right evaluation whose first term is zero stays zero, so each
  document is counted by exactly one thread.
- **Sums fit.** Each thread packs its count and its id sum (below N^2 / 2 < 2^39 for N < 2^20, which
  the program checks) into one u64.
- **Checks** before every push, listed at the end of PERF.md: the seed-1 hash at several thread counts,
  byte comparison with the reference on sample seeds 2 to 4 and holdout seed 99, 30 generated tiny
  seeds and fresh tiny and sample seeds at 1, 3 and 8 CPUs, adversarial inputs (token lengths 1 to
  70, 1-term phrases, empty answers), and four builds that force each failure path of the perfect
  hash.

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise`, `prctl` and glibc's `malloc_trim`
declared by hand (`dependency_count = 0`). The build needs AVX2, BMI2 and AES-NI
(`-C target-cpu=native`; the board builds this track on the scoring box). The Dockerfile runs a
profile-guided build, trained on data from a byte-identical copy of the organisers' generator
(`b_index.py`, not counted as code), with LLVM's atomic profile counters
(`-instrprof-atomic-counter-update-all`: the twelve-thread training run no longer loses counts, so
the profile is repeatable) and hot loops aligned to 64 bytes (`-align-loops=64`, PERF.md step 35).

## With another 24 hours

Measured or modelled on seed 1; none of this was built.

- **Builds whose speed does not depend on layout.** The PGO counters are atomic now (step 29), but
  two PGO builds of the same source still ran 84.9 and 88.3 ms on the test server with the hot
  functions instruction for instruction the same and at the same addresses (step 32). Aligning the
  hot loops to 64 bytes (step 35) removed one such gap on the test server, but the box gave mixed
  results (PERF.md steps 35 to 37). Next: find which placement costs the time on the box
  (stack and heap addresses modulo 4 KB are the first suspect) and pin it.
- **The tail.** On the test server about 4 ms pass between the last write of results.txt and the
  end of the process. Freeing the states, logs and scan tables in parallel first, and writing the
  output in parallel with `pwrite`, did not change it, so it is not page freeing; we did not find
  where it goes.
- **More stall hiding at the block end.** The two passes fetch only the first entry of each range;
  ranges with many entries still wait on their later entries' words and state.
- **Sort the first-id logs by answer in place** (about 122,000 entries per thread) instead of with a
  comparison sort, if a timing shows the sort above 1.5 ms of the tail.
- **A denser perfect hash.** The table runs at a load of 0.58 (2 MB). Placing buckets at a load near
  0.9, as PTHash does with more pilot candidates, would fit it in 1.25 MB of slots and cut the misses
  of the scan's one random load per token; the build would take longer, on twelve threads. Untested.
