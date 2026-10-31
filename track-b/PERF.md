# Track B: performance log

Every step from the reference to the current version, each with the live-board run that measured it.
Points are the board's 50·log10(K). Reruns of one commit usually spread about 2%, but one run of
unchanged code took 0.361 s instead of 0.265 (the box was disturbed), so single runs are not trusted.

| Step | Commit | Change | Run ID | T (s) | M (MB) | S | Points |
|---|---|---|---|---|---|---|---|
| 0 | | The reference, unchanged | | 56.35 | 2,200 | 91 | 0 |
| 1 | cf9c345 | Index only the query terms: (term, doc) pairs and a serial counting sort; phrases re-tokenize candidate documents | run-1e34f886a47f | 1.291 | 288 | 463 | 44.5 |
| 2 | 451f09a | Phrases matched during the scan (Aho-Corasick); parallel scatter of the pairs | run-8d140e3195a3 | 0.778 | 316 | 626 | 48.6 |
| 3 | e42aedf | Plan from a sampled document frequency: dense bitsets for common terms, phrases anchored on their rarest term | run-97a895e4b296 | 0.512 | 205 | 820 | 54.2 |
| 4 | 2020832 | SIMD tokenizer, Bloom filter in front of the term table, small hot table | run-11666ac583f0 | 0.396 | 201 | 934 | 56.6 |
| 5 | 42acaa7 | Profile-guided build | run-acc60580e004 | 0.355 | 203 | 937 | 57.8 |
| 6 | 5c7b09a | No index at all: every query answered during one scan, BOOL queries per block of 64 documents (see README) | run-de33f090eda0 | 0.267 | 38.4 | 910 | 67.6 |
| 7 | 20856e2 | Exit right after writing; each slice's pages dropped by its thread | run-cc0e62d06c76 | 0.265 | 39.1 | 922 | 67.6 |
| 8 | 196434d | Queries with common cover terms evaluated once per 256 documents with AVX2; lookups gathered per document with their buckets prefetched; one term table instead of hot table + Bloom filter | run-1a4ed151ee61 | 0.199 | 41.5 | 1,028 | 70.4 |
| 9 | 15bc6b2 | Leaner tokenizer; branch-free table probe (inline-asm cmov) | run-058dad4dcdce | 0.171 | 41.7 | 1,081 | 71.9 |

## What each step taught

- **Steps 1–5** built an inverted index of the query terms. However it was laid out, the 21M
  (term, document) pairs of the BOOL terms cost 85 MB or more, and the M axis only gives full credit
  under 44 MB: at 200 MB the memory factor alone cost about 6.6 points.
- **Step 6** removes the index. The corpus is scanned once in parallel slices of 64-document
  blocks. Each query term present in a block gets a 64-bit word, one bit per document; at the end of a
  block, the BOOL queries that one of their "cover" terms triggers (a set of terms every match must
  contain) are evaluated with word operations, left to right as the spec requires. Counts, id sums and
  first ids come from popcounts and trailing-zero counts of the result words. Memory fell from 203 to
  38 MB, which is worth more than the time (0.355 → 0.267 s).
- **Step 7** did not move T on the box (6% on the test server): unlike tracks A and C, B's exit
  had little to free.

- **Step 8** (test server, one thread): cycles 4.1G → 2.5G, about 112 → 69 cycles per corpus
  token. The 256-document superblocks alone took 175 → 151 ms at 12 threads, prefetched lookups
  151 → 139, and dropping the hot table and Bloom filter 139 → 128: with every bucket prefetched,
  one table is cheaper than three structures.

## Experiments that did not make it (measured, reverted)

| Experiment | Result |
|---|---|
| v7–v11: streaming evaluation per document instead of an index (1cd58a4) | board run-5a81a3585198: T 0.517 s, M 104 MB, S 1,132, 56.0 pts: worse than v6, reverted |
| Prefetched lookups in batches per document | no gain |
| A larger hot table in v6 | no gain |
| Block engine: a per-block presence bitmap to avoid cold `w` loads | 4.30G → 4.72G cycles (one thread) |
| Block engine: early return when an evaluation's result is zero | no change |
| Block engine: hot table of 2^10 to 2^14 buckets | 2^13 best on the server |
| Prefetching the term table while parsing queries | no gain |
| Superblock threshold ALWAYS at 0.125 / 0.5 instead of 0.25 | 129–131 / 133 ms vs 127 ms |
| One term table of 2^17 buckets | 135 ms vs 129 at 2^16 (kept 2^15 for memory) |
| Prefetching the entries' `w` words at the block end | cycles 2.45G → 2.55G |
| Branching around a zero evaluation result | cycles 2.76G → 2.82G |

## Correctness checks run before every push

sha256 of the seed-1 output; byte comparison with the reference output for sample seeds 2–4; 30
generated `tiny` seeds against `reference/b_index.py` at 1, 3 and 8 CPUs; adversarial inputs (token
lengths 1, 12, 13, 16, 17, 32, 33 and 70, mixed case, separators, empty and token-less documents,
repeated and absent terms, a corpus whose size is an exact multiple of 4096); the same output at 1,
2, 3, 7, 12 and 16 threads; `objdump | grep -c vpinsrb` = 0 on the server build.
