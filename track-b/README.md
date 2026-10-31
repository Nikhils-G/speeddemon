# Track B: Index

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

There is no index. The queries are read first; then the corpus is scanned once, in parallel
slices, and every query is answered during that scan.

**Terms.** Only query terms matter, so each token is looked up among them: a small table of the
query terms that are most frequent in a sample of the corpus (512 KB, half of a core's L2) answers
most tokens; a Bloom filter turns away most of the rest (a fifth of the corpus's tokens are words no
query uses) before the full table is probed. Tokens are found 64 bytes at a time with AVX2, and a
token's key is its first 12 bytes, lowercased and zero-padded in one register.

**BOOL queries, per block of 64 documents.** While a block is scanned, each query term present gets
a 64-bit word with one bit per document. At the block's end, AND, OR and NOT are word operations
applied strictly left to right, as the spec requires. A query is only evaluated in blocks where one
of its cover terms occurs: a set of terms that every match contains, chosen to be rare in the sample.
The match count is a popcount, the sum of matching ids is `base·popcount(w) + Σ 2^k·popcount(w & M_k)`
over six fixed masks, and the first ids come from trailing-zero counts.

**Phrases.** Each phrase is anchored on its rarest term. When a document ends, the anchors seen in
it are checked against its token ids, which are still in L1.

**Merging.** Each thread keeps, per query, the count, the id sum and a log of first ids. The slices
are in document order, so merging the threads in order gives the ten smallest ids. Output lines are
formatted in parallel.

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise` and `prctl` declared by hand. The build
needs AVX2 (`-C target-cpu=native`).

## With another 24 hours

- Parse the queries and build the plan in parallel: on the box this serial setup is 29 ms of the run.
- Shrink the per-thread working set (term words, query entries, states) to fit the box's L2, which
  two hyperthreads share.
- Evaluate the queries that are checked in every block over 256 documents at a time.
