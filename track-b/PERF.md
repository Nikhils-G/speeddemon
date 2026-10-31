# Track B: performance log

Every step from the reference to the current version, each with the live-board run that measured it,
including the steps that were reverted. Points are the board's 50·log10(K). Reruns of one commit
usually spread 1 to 3%, but a disturbed box has made single runs of unchanged code 35 to 45% slower
(0.361 s against 0.265, 0.252 against 0.176, 0.241 against 0.177), most likely other builds sharing the
box, so no decision rests on one run.

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
| 9 | 15bc6b2 | Leaner tokenizer; branch-free table probe (inline-asm cmov). Rerun run-b33ff9c0bd9c: 0.1716 s, 71.84 | run-a6b773e07cad | 0.1864 | 41.25 | 1,081 | 70.94 |
| 10 | 68e6990 | Static linking (`crt-static`): less dynamic-loader work before `main`. Its first run, run-6e66fcd85faa (0.252 s), was a disturbed box: A, C and D were slow in the same dispatch | run-058dad4dcdce | 0.1713 | 41.7 | 1,081 | 71.85 |
| 11 | 8246f98 | **Reverted** (cf64a47). One worker pool, parallel query parsing, per-block presence bitmaps: test server 118.2 → 109.6 ms, box no faster, 144 more lines. First run run-9be281b6c1b8 (0.241 s) was noise | run-7c5098eab231 | 0.1766 | 38.4 | 1,225 | 71.1 |
| 12 | 9711c01 | **Reverted** (cf64a47). Step 11 with touched lists again and the presence words set in their own prefetched loop: no box gain either | run-fe77dad5e332 | 0.1789 | | 1,235 | 70.95* |
| 13 | 0fe0395 | The answer state written when it is allocated (a lazily zeroed page read before it is written faults twice). Box-neutral, kept: 2 lines | run-c33e91b88799 | 0.1792 | | 1,083 | 71.36* |
| 14 | 56f8b0a | **Reverted** (1421702). Six threads, one per P-core, instead of twelve: memory 26.8 MB, but M already scored in full, and time lost 7% | run-0022d3e17006 | 0.1921 | 26.8 | 1,088 | 70.59* |
| 15 | 1421702 | Twelve threads again; an all-zero superblock result returns early while its query still logs first ids; block evaluation without bounds checks; slices start at any line instead of a 256-document boundary. One slow run, not repeated: step 16 builds on it | run-37b3b2a1ace8 | 0.2131 | | 1,084 | 69.48* |
| 16 | 091e35b | Setup spread over the threads (query lines split and keyed in parallel, the plan's per-line passes in parallel); each token carries its bucket index. Rerun run-5a8826a5f10b: 0.1601 s, 72.43 | run-3682cf4f995c | 0.1619 | 38.3 | 1,134 | 72.31 |
| 17 | 26b5cb5 | Leaner token lookup: long tokens on a cold path, the bucket probe compares dwords, the scan's probe and the table's `get` share one function that keeps the table in registers, the CRC stays in 32 bits | run-b16e2000c75d | 0.1484 | 38.6 | 1,140 | 73.24 |
| 18 | e24c58d | Superblock queries evaluated over their real terms only, sorted by term count and operators so the operator branch predicts. A second run, on 33cac7e, took 0.1643 s | run-6820636c30fc | 0.1529 | | 1,142 | 72.91* |
| 19 | 34dd399 | No bounds checks in the block-end loops (9b7b6f9, which had no board run of its own); then no per-token prefetch of the presence word. First run run-13cc611d4761: 0.1518 s, 72.96* | run-2bc33f24a19f | 0.1478 | 38.54 | 1,151 | 73.26 |
| 20 | 8fdef14 | The block end prefetches the entry range of the slot 16 ahead and the entries of the slot 8 ahead (one thread: cycles −5.2%). Rerun run-f0af75ddb8ab: 0.1416 s | run-4b9ff571a190 | 0.1404 | 38.46 | 1,162 | 73.78 |
| 21 | 831097c | Same engine in fewer lines; hot loops unchanged. First run run-bb8fc84e151e: 0.1466 s, M 39.68, 73.81, in a dispatch where A was slow too | run-72157fe9a7e1 | 0.1435 | 39.51 | 998 | 74.04 |
| 22 | a3a700b | Fewer lines again, hot code reformatted with the same operations; SAFETY arguments for every prefetch and unchecked load | run-1ed2ef15f7af | 0.1459 | 38.54 | 854 | 74.37 |
| 23 | 8274ba4 | Answers numbered in evaluation order (README, design 5): test server −3.4%, L2 misses −10%. Its only run was a disturbed dispatch (A 0.0263 s against about 0.0195) | run-36419fde7616 | 0.1550 | | 880 | 73.61* |
| 24 | d8e7a08 | **Reverted** (cb193e5). Step 23 plus a 64 KB one-word filter in front of the term table, so most tokens no query uses skip the 2 MB table: L2 misses −19% on the test server, box equal to step 22 (0.1443 against 0.1459) for 19 more lines | run-cda99758e008 | 0.1443 | | 899 | 74.32 |
| 25 | cb193e5 | Step 23's numbering kept in a shorter form, filter dropped. No board run of its own: step 26 followed six minutes later | | | | 859 | |
| 26 | 9a61acf | Fewer lines in setup and the result gather. M 44.09 is just over the 44 MB credit; a later run of the same code (387ebb3, ID not logged) gave 0.1420 s and 43.8 MB. The cause, found in step 28: step 23's formatter copied every thread's state into line order, about 4.5 MB | run-d00be2692c6c | 0.1389 | 44.09 | 844 | 74.93 |
| 27 | 368aa64 | **Reverted** (387ebb3). PGO training on one thread (`taskset -c 0`), so that builds are repeatable: two local builds then differed by 5 KB instead of 1.1 MB. The board's builder may not use CPU 0, so the Docker build failed on every track and there is no scored run | | | | 844 | |
| 28 | 841e809 | The block end in two passes (README, design 3): ranges gathered and prefetched first, then words and state fetched 4 ranges ahead of the evaluation (test server −4.5 to −5.7%); the formatter reads each thread's state through `answer_of` instead of a copy (M back to 39 MB). Rerun on 2515d02, run-65497002e714: 0.1381 s, 38.68 MB, 74.88 | run-3814466b62c2 | 0.1374 | 39.22 | 875 | 74.94 |

Step 28 is the current code. S is the harness's `static_axes.py` count, which matches the board for
every run where the board's S was logged. M is blank where it was not logged. Points marked * are
computed from T and S with the board's formula: every M logged since step 6 is under the 44 MB cap
(26.8 to 41.7 MB) except step 26's 44.09 MB, which costs 0.01 points, and the formula reproduces every
logged point value to within 0.01. Steps 24 and 27 are the round's reverted experiments; step 23 was
kept because its server gain was clear and the box did not show it slower than step 22 once
step 24 ran without the disturbance.

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
- **Step 9**: LLVM compiled the probe's hit-or-miss select into a branch, which mispredicts because
  whether a token is a query term is random; an inline-asm `cmov` keeps it branch-free. Test server
  132 → 120 ms.
- **Steps 11–12: test-server gains did not transfer to the box.** Step 11 cut one-thread
  instructions from 6.62G to 5.72G and the test server's time by 7%, and the box did not move
  (0.1766 and 0.1789 s against about 0.172). The box runs the scan about 1.6 times slower than the test
  server. The likeliest cause is L2: the box's P-cores share 1.25 MB of L2 between two hyperthreads,
  and each thread's hot arrays come to about 1.4 MB, so cycles there go to cache misses that an
  instruction cut does not remove. Both steps were reverted, and from then on the box was measured
  directly.
- **Timing probes on the box** (step 10's code): two temporary builds stopped at the end of setup and
  at the end of the scan and reported the elapsed time through their exit status, so the board
  shows them as failed runs: run-c4f62a9ba1d0 (setup ends at 26 ms) and run-cafcc5244948 (scan ends at
  166 ms). Of about 170 ms, setup was 26, the scan 140 and the tail about 4. Serial setup was the
  largest single cost that did not depend on the cache.
- **Step 14**: one thread per core lost 7% even though two hyperthreads share an L2, most likely
  because the second thread hides memory latency that one thread waits out.
- **Steps 15–16: setup work transferred.** Steps 15 and 16 together took the box from 0.1792 s
  (step 13) to 0.1619 and 0.1601: 17 to 19 ms, close to the 26 ms of serial setup that step 16
  spread over twelve threads.
- **Step 17: instruction cuts that also cut cycles transferred.** The token-path changes cut 6.8%
  of the instructions on the test server and the box went 0.1619 → 0.1484 s (−8%).
- **Steps 18–19 are within the box's noise.** On the test server, step 18 took the instruction cut
  against step 16 from 6.8% to 11%, and step 19 cut 4.1% and then 4.8% more, but the box gave 0.1529,
  0.1518 and 0.1478 s against step 17's 0.1484. They were kept because they are not slower and step 19 ran the best time so far. The design
  is now at about 141 instructions per corpus token in all; a further gain needs a smaller working set, not fewer
  instructions (steps 23–24 later showed that a smaller working set did not pay either; fewer stalls did).
- **Step 20: hiding random loads transferred.** The touched slots come in first-seen order, so the
  block end's loads of their entry ranges and entries were random L3 hits. Prefetching them ahead
  took the box from 0.1478 s to 0.1404 and 0.1416 (−4 to −5%), more than the 3.3% the test server
  showed at twelve threads.
- **Steps 21–22, 25–26: lines are cheap points.** S went from 1,162 to 844 with the hot loops
  unchanged; at the board's S exponent that is worth about 1 point, the same as a 9% time cut.
- **Steps 23–24: L2-miss cuts did not transfer.** Numbering the answers in evaluation order cut L2
  misses 10%, and the one-word filter 19% more, on the test server; the box did not move (0.1443
  against 0.1459). The test server has 512 KB of L2 per core, less than the box's 1.25 MB, and is
  still faster, so the L2 explanation of the box's slower scan in steps 11–12 does not hold; clock
  and L3 latency are the likelier causes.
- **Step 26: a memory regression hidden in a line cut.** M rose from about 38.5 to 44.09 MB, just past
  the 44 MB credit. Step 23's formatter gathered every thread's state into line order: twelve
  threads times 50,000 answers of 8 bytes. Step 28 reads the states in place through `answer_of`.
- **Step 27: PGO builds are not deterministic.** With a twelve-thread training run and non-atomic
  profile counters, every build is a different binary; builds of the same code ran 2.5 to 6% apart
  in speed. A single board run therefore mixes the code's effect with the build's, which explains part
  of the spread above, and the final scoring rebuilds. Training on one thread made builds repeatable
  locally but failed on the board's builder, so it was reverted at once (README, next 24 hours).
- **Step 28: stall cuts transferred again.** The two-pass block end hides the remaining random loads
  of an evaluation (words and answer state) behind the evaluations before it. The box gave 0.1374 and
  0.1381 s against step 26's 0.1389 and 0.1420, and M fell back to 39 MB, for 31 more lines. What the
  box rewarded in this whole round was more parallel setup (steps 15–16) and fewer stalls on random
  loads (steps 20, 28), not cuts of instructions (steps 18–19) or of L2 misses (steps 23–24).
- **A measurement bug**: some test-server A/B comparisons after step 11 compared a stale binary,
  because copied source files kept older modification times and cargo did not rebuild. It was found
  and fixed; every keep-or-revert decision above rests on board runs, not on those comparisons.

## Experiments that did not make it (measured, reverted)

Board-measured reverts are in the table above (steps 11, 12, 14, 24 and 27). These were measured elsewhere.

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
| Profile-guided build trained on the full scale instead of the sample | neutral on the test server, not pushed |
| Single-thread PGO training (step 27) pinned to the first allowed CPU instead of CPU 0, or LLVM's atomic profile counters | not yet tried: either should make builds repeatable without breaking the board's builder |
| Document frequencies sampled into 2^16 hashed u16 counters, to overlap the sample with query parsing | simulated on seed 1: hash collisions move queries to the superblock path (5,882 → about 8,530), +138M instructions in total; not built |

## Correctness checks run before every push

sha256 of the seed-1 output; byte comparison with the reference output for sample seeds 2–4; 30
generated `tiny` seeds against `reference/b_index.py` at 1, 3 and 8 CPUs; adversarial inputs (token
lengths 1, 12, 13, 16, 17, 32, 33 and 70, mixed case, separators, empty and token-less documents,
repeated and absent terms, a corpus whose size is an exact multiple of 4096); the same output at 1,
2, 3, 7, 12 and 16 threads; `objdump | grep -c vpinsrb` = 0 on the server build. For steps 8 and 9,
also a full-scale seed 99 against the official reference.
