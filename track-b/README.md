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
  140 ms and the tail (merge, format, write, exit) at about 4 ms.

## The design

There is no index. The queries are read first, the corpus is scanned once in parallel slices of whole
lines, and every query is answered during that scan, so nothing grows with the corpus: the board
measures about 38.5 MB.

1. **Setup, in parallel.** The query lines are split and keyed on every thread, and the terms get
   ids, BOOL terms first, so that they index dense per-thread arrays. The start of every slice (2 MB
   in total) is scanned to estimate each term's document frequency, and the plan is built from that
   sample, a range of query lines per thread.
2. **Finding and looking up tokens.** Each 64-byte chunk is classified with AVX2 into a token-byte
   mask and an LF mask; token starts and lengths come from bit operations on them. A token's key is
   its first 12 bytes, lowercased and zero-padded in one 16-byte register, and its hash is one CRC32
   instruction. The term table holds four keys per 64-byte bucket, so a probe is one cache line, and
   that line is prefetched as soon as the token is found. A document's tokens are all found first and
   looked up after, so the bucket misses of one document overlap. The probe compares the four keys at
   once and selects hit or miss with a conditional move: whether a token is a query term is too random
   for the branch predictor. Terms of 13 to 32 bytes (0.2% of the tokens) go to a cold hash map.
3. **BOOL queries, per block of 64 documents.** While a block is scanned, each BOOL term present gets
   a 64-bit word with one bit per document. At the block's end, AND, OR and NOT are word operations in
   the query's order. A query is evaluated only in blocks that contain one of its *cover* terms: a set
   of terms of which every match contains at least one, chosen to be rare in the sample. On seed 1
   that is 4.27M block evaluations, against 121M for every BOOL query in every block.
4. **Common queries, per superblock of 256 documents.** The 5,882 queries whose cover terms are
   expected in more than a quarter of the blocks (summed over the terms) would be evaluated in almost
   every block anyway. They are evaluated once per 256 documents instead, with AVX2 on four words per
   term, and only over their real terms: they are sorted by term count and operators, so the operator
   branch predicts.
5. **Counts, sums and first ids without a loop over documents.** The count is a popcount. The sum of
   the matching ids is `base * popcount(w) + sum over k of 2^k * popcount(w & M_k)`, with six fixed
   masks M_k (M_0 = 0xAAAA..., the odd positions); for 256 documents at once, nibble lookup tables do
   the same in AVX2. First ids come from trailing-zero counts, and only while a query has fewer than ten.
6. **Phrases.** Each phrase is anchored on its rarest term. When a document ends, the anchors seen in
   it are checked against the document's term ids, which are still in L1.
7. **Merging.** The slices are in document order, so the threads' first-id logs, each sorted, merge in
   slice order into the ten smallest ids. Output lines are formatted in parallel, and the process
   exits right after writing instead of freeing its tables.

## Why the output is exact

- **Tokens.** The key keeps exactly the token's bytes (a mask of its length) and ORs 0x20 into them,
  which lowercases A to Z and leaves digits unchanged. Token bytes are never zero, so the zero padding
  makes keys of different lengths different, and the table compares whole keys, not hashes. Longer
  terms are compared byte for byte; a token over 32 bytes cannot be a term (the grammar caps terms at
  32 bytes) and is skipped.
- **BOOL.** Bit i of a word is "document base + i contains the term", so the word operations are the
  spec's set operations, applied in the query's order. Block queries are padded to six steps with AND
  of an all-ones word, which changes nothing.
- **Covers.** A block without any cover term of a query holds no match of it, so skipping it is exact.
  A block with several of them is still evaluated once: each cover term's entry knows the earlier
  ones and stands down when one of them is present.
- **The sample** only decides where each query is evaluated (per block, per superblock, which anchor),
  never its result.
- **Slice edges.** A block may straddle two slices. A document outside a thread's slice has a zero bit
  in every word there, and a left-to-right evaluation whose first term is zero stays zero, so each
  document is counted by exactly one thread.
- **Sums fit.** Each thread packs its count and its id sum (below N^2 / 2 < 2^39 for N < 2^20, which
  the program checks) into one u64.
- **Checks** before every push, listed at the end of PERF.md: the seed-1 hash, byte comparison with the
  reference on sample seeds 2 to 4, 30 generated tiny seeds at 1, 3 and 8 CPUs, adversarial inputs and
  a thread-count sweep.

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise` and `prctl` declared by hand
(`dependency_count = 0`). The build needs AVX2 (`-C target-cpu=native`; the board builds this track
on the scoring box). The Dockerfile runs a profile-guided build, trained on data from a byte-identical
copy of the organisers' generator (`b_index.py`, not counted as code).

## With another 24 hours

Measured or modelled on seed 1; none of this was built.

- **Fit each thread's working set in L2.** The box's P-cores share 1.25 MB of L2 between two
  hyperthreads, and each thread's hot arrays come to about 1.4 MB (term words 427 KB, touched list
  214 KB, superblock rows 318 KB, answer state 400 KB), next to the 1 MB of query entries and the 2 MB
  term table that all threads share. This is the likeliest reason the box runs the scan about 1.6
  times slower than the test server, and why cuts that only saved instructions did not always transfer.
- **Number the answers in evaluation order.** Each superblock's common queries now write state spread
  over 3,993 cache lines; numbered in their evaluation order they would fill 736.
- **Sort the first-id logs by answer in place** (about 122,000 entries per thread) instead of with a
  comparison sort, if a timing shows the sort above 1.5 ms of the tail.
- **Probe four tokens at once with AVX2 gathers** (Polychroniou, Raghavan and Ross, SIGMOD 2015): the
  only route we know below the current cost per token, untested at 75,747 twelve-byte keys.
