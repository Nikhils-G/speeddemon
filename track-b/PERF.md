# Track B: performance log

**Result.** The reference runs 56.35 s at 2,200 MB, S 91. Naive Rust (step 1, run-1e34f886a47f)
took 1.291 s at 288 MB, S 463, for 44.5 points. The submitted code is step 90 (bpl-1, 798ff2d,
S 1175). Its best board run, run-9c27a7fdfe8f, took 0.066749 s at 31.98 MB for 81.82 points: 844×
faster than the reference.

**Three ideas carry it.**
- No index: every query is answered in one parallel scan (step 6: 0.355 s at 203 MB → 0.267 s at
  38.4 MB).
- A perfect hash with one branch-free probe per token, built on 12 threads (step 31: 0.1338 →
  0.1199 s).
- BOOL queries folded into truth tables and evaluated per document, 8 at a time, with the common
  ones per 256 documents (steps 49 and 66; the superblocks since step 8).

**Reading the numbers.** One binary's board runs spread by up to 45% in disturbed dispatches. So
from step 52, every keep-or-revert decision rests on paired runs on a replica of the box ("How we
measured"); where the board cannot resolve a step, its row says so. Reverted steps stay in the
table. Every measured experiment that was not kept is under "Experiments that did not make it",
with its numbers.

Every run ID below is in the board's history for team SALAAR. A run with "probe" in place of T, or
a failed run in the probe tables, is a timing build that reports through its exit code.

| What moved the score most | Step | Board, before → after |
|---|---|---|
| No index: every query answered during one scan | 6 | 0.355 s and 203 MB → 0.267 s and 38.4 MB |
| Common queries per 256 documents, prefetched lookups | 8 | 0.2658 → 0.199 s |
| Setup spread over the threads | 15-16 | 0.1713 (step 10; step 13's 0.1792 came in a disturbed dispatch) → 0.1619 s |
| A perfect hash of the query terms | 31 | 0.1338 → 0.1199 s |
| BOOL queries per document from truth tables | 49 | 0.108-0.115 → 0.097889-0.10298 s |
| The quality port, byte stamps, phrase records | 52 | replica −7.05%; 0.092845 s |
| One persistent thread pool for the setup | 55 | replica −2.23% against step 54 |
| Triggered BOOL entries evaluated 8 at a time | 66 | replica −2.8 to −3.3%; 0.085425 → 0.082048 s |
| Less setup work, releases where threads wait, smaller plan intermediates, a larger sample | 67-70 | replica about −3.6 ms; 0.082048 → 0.078573 s |
| Less copying on the main thread, branch-free anchors, stamp lines fetched for writing and lists left-packed | 72-77 | replica about −4.1 ms; 0.078573 → 0.074086 s |
| 16-byte pending tokens, ids and rows 8 at a time, superblock rows side by side | 78 | replica about −1.5 ms; 0.074086 → 0.072974 s |
| ALWAYS 0.40, the left-pack's lane numbers from a table, one plain build, the tail's releases over the threads | 79-81 | replica about −1.6 ms; 0.072974 → 0.072172 s |
| Freed memory kept in the heaps, 16-byte query entries, superblock counts from byte sums, entries loaded and transposed | 82-85 | replica about −3 ms; 0.072172 → 0.068502 s |
| 25 lines cut with the scan's code and alignment unchanged, std's start-up skipped, a shorter plan | 87-90 | replica about −1.0 ms, S 1194 → 1175; 0.068502 → 0.066749 s |

Every step from the reference to the current version, each with the live-board run that measured it,
including the steps that were reverted. Points are the board's 50·log10(K). Reruns of one commit
usually spread 1 to 3%, but a disturbed box has made single runs of unchanged code 35 to 45% slower
(run-96f25e2c5a89 0.361 s against run-cc0e62d06c76 0.265; run-6e66fcd85faa 0.252 against 0.176 for
the next run of that code, whose ID was not logged; run-9be281b6c1b8 0.241 against run-7c5098eab231
0.1766). Step 49's binary, run 15 times from 11:11 to 14:08 UTC, took 0.097889 to 0.113384 s. So no
decision rests on one board run. A dash means a value is not in our run logs; * marks points
computed from T and S with exponents fitted to the logged runs (the note after the table; "How the
final number was verified"), where the logs hold none.

Before pushing, steps were timed in three ways ("How we measured", near the end):
- every step: on a 16-vCPU AMD test server (interleaved runs of two PGO builds, medians);
- from step 10 on: with exit-code and in-process A/B probes on the box itself;
- from step 52 on (Sun 13:20 UTC): on a replica of the scoring box, with the same CPU and the
  board's own timer. Step 52 was built and kept on the replica.

Steps 5 to 80 were built with profile-guided optimisation, and their timings here are of PGO
builds unless marked non-PGO. Step 81 drops it for one plain release build (step 81 below; README,
"Dependencies").

Names such as b-stack-15, bj-2, bs-5j2, q6t or bc-3 are our checkpoint names: from step 52 on, each
row starts with its checkpoint's name, and an experiment that names a step ran on that step's code.

| Step | Commit | Change | Run ID | T (s) | M (MB) | S | Points |
|---|---|---|---|---|---|---|---|
| 0 | | The reference, unchanged | | 56.35 | 2,200 | 91 | 0 |
| 1 | cf9c345 | Index only the query terms: (term, doc) pairs and a serial counting sort; phrases re-tokenize candidate documents | run-1e34f886a47f | 1.291 | 288 | 463 | 44.5 |
| 2 | 451f09a | Phrases matched during the scan (Aho-Corasick); parallel scatter of the pairs | run-8d140e3195a3 | 0.778 | 316 | 626 | 48.6 |
| 3 | e42aedf | Plan from a sampled document frequency: dense bitsets for common terms, phrases anchored on their rarest term | run-97a895e4b296 | 0.512 | 205 | 820 | 54.2 |
| 4 | 2020832 | SIMD tokenizer, Bloom filter in front of the term table, small hot table | run-11666ac583f0 | 0.396 | 201 | 934 | 56.6 |
| 5 | 42acaa7 | Profile-guided build | run-acc60580e004 | 0.355 | 203 | 937 | 57.8 |
| 6 | 5c7b09a | No index at all: every query answered during one scan, BOOL queries per block of 64 documents (see README) | run-de33f090eda0 | 0.267 | 38.4 | 910 | 67.6 |
| 7 | 20856e2 | Exit right after writing; each slice's pages dropped by its thread. Rerun on a4e4404 (B unchanged), run-cc0e62d06c76: 0.2650 s, 67.64 | run-94bcf1f00dfc | 0.2658 | 38.64 | 922 | 67.61 |
| 8 | 196434d | Queries with common cover terms evaluated once per 256 documents with AVX2; lookups gathered per document with their buckets prefetched; one term table instead of hot table + Bloom filter | run-1a4ed151ee61 | 0.199 | 41.5 | 1,028 | 70.4 |
| 9 | 15bc6b2 | Leaner tokenizer; branch-free table probe (inline-asm cmov). Rerun run-b33ff9c0bd9c: 0.1716 s, 71.84 | run-a6b773e07cad | 0.1864 | 41.25 | 1,081 | 70.94 |
| 10 | 68e6990 | Static linking (`crt-static`): less dynamic-loader work before `main`. Its first run, run-6e66fcd85faa (0.252 s), was a disturbed box: A, C and D were slow in the same dispatch | run-058dad4dcdce | 0.1713 | | 1,081 | 71.85 |
| 11 | 8246f98 | **Reverted** (cf64a47). One worker pool, parallel query parsing, per-block presence bitmaps: test server 118.2 → 109.6 ms, box no faster, 144 more lines. First run run-9be281b6c1b8 (0.241 s) was noise | run-7c5098eab231 | 0.1766 | 38.4 | 1,225 | 71.1 |
| 12 | 9711c01 | **Reverted** (cf64a47). Step 11 with touched lists again and the presence words set in their own prefetched loop: no box gain either | run-fe77dad5e332 | 0.1789 | | 1,235 | 70.95* |
| 13 | 0fe0395 | The answer state written when it is allocated (a lazily zeroed page read before it is written faults twice). Its one run, 0.1792 s against step 10's 0.1713 s, came in a dispatch where Track A ran 0.026108 s, against 0.0198 to 0.0226 s before and 0.019836 s for the same A code in the next dispatch (runs.log 18:55Z and 19:01Z): no reading either way. Kept, at 2 lines; step 16, built on it, ran 0.1619 s | run-c33e91b88799 | 0.1792 | | 1,083 | 71.36* |
| 14 | 56f8b0a | **Reverted** (1421702). Six threads, one per P-core, instead of twelve: memory 26.8 MB, but M already scored in full, and time lost 7% | run-0022d3e17006 | 0.1921 | 26.8 | 1,088 | 70.59* |
| 15 | 1421702 | Twelve threads again; an all-zero superblock result returns early while its query still logs first ids; block evaluation without bounds checks; slices start at any line instead of a 256-document boundary. One slow run, not repeated: step 16 builds on it | run-37b3b2a1ace8 | 0.2131 | | 1,084 | 69.48* |
| 16 | 091e35b | Setup spread over the threads (query lines split and keyed in parallel, the plan's per-line passes in parallel); each token carries its bucket index. Rerun run-5a8826a5f10b: 0.1601 s, 72.43 | run-3682cf4f995c | 0.1619 | 38.3 | 1,134 | 72.31 |
| 17 | 26b5cb5 | Leaner token lookup: long tokens on a cold path, the bucket probe compares dwords, the scan's probe and the table's `get` share one function that keeps the table in registers, the CRC stays in 32 bits | run-b16e2000c75d | 0.1484 | 38.6 | 1,140 | 73.24 |
| 18 | e24c58d | Superblock queries evaluated over their real terms only, sorted by term count and operators so the operator branch predicts. A second run, on 33cac7e, took 0.1643 s. Both runs were over step 17's 0.1484 s (+3% and +11%); kept because step 19, built on it, ran 0.1478 s | run-6820636c30fc | 0.1529 | | 1,142 | 72.91* |
| 19 | 34dd399 | No bounds checks in the block-end loops (9b7b6f9, which had no board run of its own); then no per-token prefetch of the presence word. First run run-13cc611d4761: 0.1518 s, 72.96* | run-2bc33f24a19f | 0.1478 | | 1,151 | 73.26 |
| 20 | 8fdef14 | The block end prefetches the entry range of the slot 16 ahead and the entries of the slot 8 ahead (one thread: cycles −5.2%). Rerun run-f0af75ddb8ab: 0.1416 s | run-4b9ff571a190 | 0.1404 | 38.46 | 1,162 | 73.78 |
| 21 | 831097c | Same engine in fewer lines; hot loops unchanged. First run run-bb8fc84e151e: 0.1466 s, M 39.68, 73.81, in a dispatch where A was slow too | run-72157fe9a7e1 | 0.1435 | 39.51 | 998 | 74.04 |
| 22 | a3a700b | Fewer lines again, hot code reformatted with the same operations; SAFETY arguments for every prefetch and unchecked load | run-1ed2ef15f7af | 0.1459 | 38.54 | 854 | 74.37 |
| 23 | 8274ba4 | Answers numbered in evaluation order (README, design 5): test server −3.4%, L2 misses −10%. Its only run was a disturbed dispatch (A 0.0263 s against about 0.0195) | run-36419fde7616 | 0.1550 | | 880 | 73.61* |
| 24 | d8e7a08 | **Reverted** (cb193e5). Step 23 plus a 64 KB one-word filter in front of the term table, so most tokens no query uses skip the 2 MB table: L2 misses −19% on the test server, box equal to step 22 (0.1443 against 0.1459) for 19 more lines | run-cda99758e008 | 0.1443 | | 899 | 74.32 |
| 25 | cb193e5 | Step 23's numbering kept in a shorter form, filter dropped. No board run of its own: step 26 followed six minutes later | | | | 859 | |
| 26 | 9a61acf | Fewer lines in setup and the result gather. M 44.09 is just over the 44 MB credit; The cause, found in step 28: step 23's formatter copied every thread's state into line order, about 4.5 MB | run-d00be2692c6c | 0.1389 | 44.09 | 844 | 74.93 |
| 27 | 368aa64 | **Reverted** (387ebb3). PGO training on one thread (`taskset -c 0`), so that builds are repeatable: two local builds then differed by 5 KB instead of 1.1 MB. The board's builder may not use CPU 0, so the Docker build failed on every track and there is no scored run | | | | 844 | |
| 28 | 841e809 | The block end in two passes (the block engine's, replaced in step 49): ranges gathered and prefetched first, then words and state fetched 4 ranges ahead of the evaluation (test server −4.5 to −5.7%); the formatter reads each thread's state through `answer_of` instead of a copy (M back to 39 MB). Rerun on 2515d02, run-65497002e714: 0.1381 s, 38.68 MB, 74.88 | run-3814466b62c2 | 0.1374 | 39.22 | 875 | 74.94 |
| 29 | 7fc8af9 | Fewer lines with the hot loops unchanged (Rust 864 → 778 code lines, the token and LF masks from one load; test server −1.3%), and the PGO training run with LLVM's atomic profile counters (fb34663, all tracks) | run-59b2e0a8fdd5 | 0.1353 | | 789 | 75.44 |
| 30 | 31cbd48 | Query parsing relies on the spec's query grammar (2.3) instead of re-checking it; malformed lines still abort on a slice bound. Neutral on the test server | run-ff1d9b72da23 | 0.1338 | 39.21 | 781 | 75.60 |
| 31 | 776e1dc | A perfect hash of the query terms instead of the 4-way bucket table (README, design 2): two AES rounds as the hash, one 16-byte slot per term, a branch-free probe, built on 12 threads. Test server −8.4%, 12-thread cycles −11%; box −10.4%. | run-4a28a650e9ac | 0.1199 | 39.62 | 818 | 76.64 |
| 32 | bdf6d27 | Fewer lines in cold code only: the hot functions compile to the same instructions as step 31's. Test server: two PGO builds 84.96 and 85.28 ms (median of 31 runs, mean of 3 interleaved rounds) against step 31's three builds 85.98, 85.25 and 85.96. Rerun on bbdb79e (B unchanged), run-a361a4d1c316: 0.1189 s, 76.82 points (its M is not in our logs), then the best B run | run-0741f63811ab | 0.1205 | 39.67 | 796 | 76.67 |
| 33 | ecc8c7d | Setup in compact pre-sized records (README, design 1): 8- and 16-byte split records that never grow, grouped by `csr`; the long-term map asked only about terms over 12 bytes; the plan's kind bits, term order and renumbering in parallel or by counting sort; `malloc_trim` before the scan. Test server 86.8/87.2 → 83.8/83.8 ms (21 interleaved runs, two rounds), setup 16.8 → 11.6 ms. C ran slow in the same dispatch (0.0252 s against 0.0217 to 0.0221) | run-c9888b108142 | 0.1252 | 41.48 | 847 | 76.06 |
| 34 | c43ef66 | A 1 MB sample instead of 2 MB. Test server 84.0/83.7/84.0 → 82.5/82.2/82.9 ms (3 × 21 interleaved runs); 512 KB planned worse (87.1 and 87.2 ms against 83.4 and 83.0 for 1 MB in the same rounds) | run-ffe904556c9a | 0.1174 | 41.76 | 847 | 76.75 |
| 35 | b25326b | Hot loops aligned to 64 bytes (`-C llvm-args=-align-loops=64`) in the profile-use build of A, B and C; B's code unchanged. B slower than step 34's 0.1174, so B's Dockerfile dropped the flag in step 36 | run-178d358d9dc4 | 0.1206 | | 847 | 76.47* |
| 36 | 250436c | Fewer lines in setup and the plan only (S 847 → 817), without loop alignment. A slow-box dispatch: A, unchanged, ran 0.0260 s against its best 0.0189 | run-39e2a79a78c4 | 0.1479 | | 817 | 74.36* |
| 37 | a02cacb | Step 36 with loop alignment back on (test server: with the flag, step 36 times the same as step 34; without it, 1.5 to 2.5 ms slower). A slow-box dispatch again: A, unchanged, ran 0.0266 s. Rerun on f8206db (B unchanged), run-00e91f7a4d7b: 0.1203 s, 42.16 MB, 76.60 | run-b2ce4f101146 | 0.1313 | | 817 | 75.66* |
| 38 | fcc1dd3 | Dockerfile only: the loop-alignment flag off again, as a board A/B test of step 37 (same source as steps 36 and 37). Then the best B run; a second run (16747e3, run-36074c771c0e) gave 0.119467 s, 41.17 MB, 76.68 | run-a77b19c6bb15 | 0.116583 | 41.82 | 817 | 76.95 |
| 39 | e233061 | Fewer instructions per token (test server −2%, instructions −6.2%): scan windows end at their last non-token byte, so token lengths come from a mask of token ends and no carry crosses windows; pending tokens carry their slot's byte offset; the probe loads the info word on its own, and `Phf::slot` reads the pilot as a plain u16. Run on e3fea29 (B unchanged). First run run-e00a8a0bb94b: 0.123967 s, 76.17, in a dispatch where A was slow | run-f9e9c664228f | 0.113311 | 41.49 | 845 | 77.15 |
| 40 | b9237f3 | ALWAYS 0.25 → 0.125: with the cheaper token path, more queries on the superblock path | run-f190e97aef76 | 0.11394 | 41.9 | 845 | 77.09 |
| 41 | 45c369c | First ids put in line order by a prefix sum of the logged counts and one scatter, instead of a per-thread sort (README, design 8) | run-b1c5ceda0b06 | 0.12433 | 41.6 | 856 | 76.10 |
| 42 | 061e6e7 | Back to sorting the first ids, as a board A/B of step 41. Run on 2c8ba1c (B unchanged) | run-1d513d5e3c12 | 0.114997 | 41.34 | 845 | 76.99 |
| 43 | 1877866 | Step 41's code again, a second sample | run-4bf47a08b8fc | 0.112506 | 42.23 | 856 | 77.18 |
| 44 | d576688 | ALWAYS back at 0.25, as a board A/B of step 43's 0.125. The best B run then | run-4bb3867f9472 | 0.108588 | 42.02 | 856 | 77.57 |
| 45 | 0613dd1 | The formatter prefetches the 12 threads' states of the line 8 ahead: they are stored in answer order and read in line order, so each line's 12 reads are random. Test server: format phase 1.9 → 1.35 ms, −0.35 to −0.56 ms on three PGO builds. Later samples of this code: run-6b284372d9e8 (ade4b30) 0.109028 s, 41.98 MB, 77.52; run-92f2f932e951 (82cf0cf) 0.114512 s; run-cc93468c4bc0 (aa82492) 0.110054 s. On the box no run of it beat step 44's one run (0.108588 s): its four took 0.109028 to 0.115166 s. Step 58 dropped the prefetch, which the replica found did not pay (−0.428 to +0.066 ms) | run-2e7482eb1235 | 0.115166 | 41.06 | 858 | 76.92 |
| 46 | 631d15b | **Reverted** (ade4b30). Step 45 plus a block end that filters touched slots by a per-term trigger bitmap and drops entries whose two rarest required terms share no document before evaluating them (1.37M of 4.26M block evaluations on seed 1): test server −1.9% on three builds, but the box ran 23% slower and over the 44 MB credit | run-e007250809db | 0.134041 | 45.52 | 892 | 75.00 |
| 46b | d063ddd | Board A/B, pushed after step 47: the quality pass alone (qb-2: step 44's code with the quality changes, without step 45's prefetch and step 47's second thread) | run-a9b8897b3449 | 0.112084 | 42.69 | 896 | 77.07 |
| 47 | 5a0612d (board runs from 5a0612d, 7f08fe7, 045a6b5) | The code-quality pass (renames, SAFETY arguments, setup split into named functions, named constants) on step 45's code, plus `Plan::new` grouping the anchors and sorting the `always` entries on a second scoped thread while the main thread groups the entries (serial before: about 1.45 ms on the test server; −0.9 to −1.3 ms on three PGO builds). The first run was in a slow dispatch (A's unchanged code ran 0.022241 s in it). The third run's M is not in our logs, so its points are computed (*) | run-5892b2f7eb00, run-eb924b66b4aa, run-29fa59a9bf1f | 0.144406, 0.115198, 0.108491 | 40.88, 41.8, — | 905 | 74.29, 76.74, 77.40* |
| 48 | 1987ee2 (not kept: the next B commits restored step 47) | bspeed-9: block results compacted before adding; the phrase compare in one vector load. Not kept | run-79b933c57064, run-6ebf5408aefa | 0.113784, 0.13007 | 42.55, 41.05 | 929 | 76.79, 75.34 |
| 49 | 92372ef (later runs up to 068c302) | **A new design**: BOOL queries evaluated per document from truth tables (README design 3) | run-f34943827101, run-0e07600f6670, run-91a932d82d0e, run-75b0cfe3cc79, run-104cca2cc2ac | 0.10298, 0.098492, 0.102535, 0.100665, 0.097889 | 37.9, 37.51, 36.06, 37.2, 37.96 | 923 | 77.90, 78.38, 77.94, 78.14, 78.45 |
| 50 | 0f530b1, a8fe628 (reverted in a4f841d) | **Reverted.** bc-3: large allocations from the malloc heaps (`mallopt`) | run-346f5941106b, run-0157e09626c3, run-09a5b88224bd | 0.117047, 0.113299, 0.102303 | 39.08, 40.73, 40.37 | 928 | 76.49, 76.84, 77.95 |
| 51 | 774e354 (reverted in a8fe628) | **Reverted.** bc-4: 512-document superblocks, one reduction per query (C1) | run-7c8b0f9c238a | 0.107105 | 43.07 | 952 | 77.37 |
| 52 | 5290d47, cfad1ec (one comment; second run from c8f98f2) | b-stack-1: step 47's quality pass on step 49's design, byte stamps, phrase records (replica −7.05%) | run-0f3b8e8a39f9, run-394a80cc8014, run-b18a35f4b44f | 0.103801, 0.093419, 0.092845 | 40.19, 39.87, 39.49 | 1004 | 77.54, 78.68, 78.75 |
| 53 | 813ac5d (later runs from 48ee977, d29ece2) | b-stack-2: ALWAYS 0.25 → 0.30, comments brought up to date (replica −0.33%: noise) | run-ef8b62a003db, run-7a47df633d9c, run-2c7a4b98be65 | 0.092929, 0.092909, 0.091413 | 39.26, 40.32, 39.44 | 1004 | 78.74, 78.74, 78.92 |
| 54 | 594fcee (later runs from 783e796, e2d0381, 4968eb1) | b-stack-3: the plan's tables grouped without zero fills or copies; one packed sort key for the `always` entries (replica −0.99%; first run DISTURBED, third and fourth with B alone slow; no run beat step 53's 0.091413 to 0.092929 s, so the gain rests on the replica) | run-90775fc26164, run-a1657d166453, run-7a61ec8545e4, run-dba4a5d81bde | 0.097526, 0.093673, 0.105612, 0.114435 | 39.96, 39.1, 39.27, 37.52 | 1009 | 78.20, 78.64, 77.33, 76.46 |
| 55 | ffef5f8 (later runs from 44e0f8f, e466e5a; pushed again in 1230a75, run from 2c3726c) | b-stack-4: one persistent pool of threads behind `par`, its workers waiting in `umwait` between calls (replica −2.23% against step 54; second run: DISTURBED dispatch) | run-f98c6958604d, run-6bedf511e709, run-54758cd41117, run-5a75106fd40c | 0.095419, 0.098412, 0.09033, 0.089795 | 39.74, 39.72, 39.24, 39.97 | 1042 | 78.33, 78.00, 78.93, 78.99 |
| 56 | d45040c | b-stack-5: block-major superblock rows, the review's fixes, rustfmt (replica −0.57% against step 55). Its one board run, 0.09138 s, lies inside step 55's four runs (0.089795 to 0.098412 s): the board cannot resolve 0.57% | run-9ef5b681aed8 | 0.09138 | 38.81 | 1065 | 78.73 |
| 57 | bd025d6 (later runs from c25a4fc, a106aad) | b-stack-6: the first slice's thread publishes which answers it alone fills with ten ids, and the later threads regroup only the rest; exact-size first-id arrays; line ends instead of starts and cursors (replica −0.70% against step 56) | run-97d54b356d06, run-06956c44e035, run-e52b3723fb8c | 0.090239, 0.088985, 0.088606 | 37.02, 36.54, 36.92 | 1067 | 78.86, 79.01, 79.06 |
| 58 | 88af8ca (later runs from 398eb51, 53173d3, 42de64e) | b-stack-7: a perfect-hash region whose bucket finds no pilot placed again by its own thread, up to four times, and a new seed that changes every hash bit; first-id log entries in 32 bits; the formatter's prefetch dropped (replica +0.07% against step 57: the same speed; memory, max of 5 runs, 31.06 MB). Its second run was the best Track B run until step 59 (first run: DISTURBED dispatch) | run-069dc1385e9b, run-9c22107047b4, run-f9ce4bea2564, run-65e7bf2f2bb3 | 0.10775, 0.088068, 0.08862, 0.08865 | 31.57, 30.97, 31.4, 30.93 | 1073 | 76.92, 79.11, 79.04, 79.03 |
| 59 | 10ef5ba (runs from 86b9c7c, 807ce06) | b-stack-8: every phrase checked by one masked compare (look_up writes 8 `miss` ids after each document's tokens, so 8 ids exist from any phrase start; the loop, `Plan::terms` and `Phrase::start` gone; a phrase of over 8 terms stops the program); the row index as `id.min(rows)`, which LLVM compiles to `cmovb` (checked in the PGO binary), so the asm `min` is gone; the tail's lines pass (bt-4). Replica −1.349 ms (−1.56%, 29 of 31) against step 58; S 1073 → 1042. Its first run was the best Track B run until step 65 | run-31588c94b3ee, run-ee87cc98e73b | 0.08596, 0.087145 | 30.11, 30.25 | 1042 | 79.46, 79.32 |
| 60 | a046df6 (run from 30c2691) | b-stack-9: a lines pass at the same speed (tuple `let`s split, the state of the range `RANGES_AHEAD` on fetched without an `if let`, a stride not redefined; replica −0.25%, 20 of 31) | run-3c2c95caf5f8 | 0.08742 | 30.06 | 1030 | 79.32 |
| 61 | 33ecba3 (later runs from eb1258d, 3aa95c6) | b-stack-10: review fixes: `par` asserts one call at a time (`BUSY`), and its SAFETY text covers the job left in `JOB`; the fallback wait without WAITPKG spins with `spin_loop`; a comment on why the setup's barrier cannot deadlock on the pool; one more line cut (replica +0.15%, 12 of 31: the same speed; second run in a slow dispatch) | run-6f92692cfe79, run-d023cb5dbb75, run-e39902c82c5d | 0.088016, 0.144833, 0.092338 | 30.13, 30.47, 30.63 | 1033 | 79.24, 73.83, 78.72 |
| 62 | 5fda0c0 (later runs from 96502b4, 475aae6, 2f05cec) | b-stack-11: near the end of the work list, the trigger's lookahead prefetch reads the current range's first entry instead of entry 0, so every entry it reads is the first of a trigger term's range, which is never empty; 17 hand-made edge inputs added to the gate (replica +0.07%, 15 of 31: the same speed; first run in a slow dispatch) | run-9a68373e9fd4, run-72fc88641e4b, run-43ec6fb133ea, run-e734232a0ec5 | 0.132044, 0.088963, 0.090207, 0.087799 | 29.97, 30.34, 30.42, 30.49 | 1033 | 74.83, 79.12, 78.97, 79.26 |
| 63 | 854810e (later runs from ffb2e77, aa1fe77) | b-stack-12: `slices` cuts a range of more than 2^16 documents every 2^16 lines, so a valid corpus of many short lines no longer aborts on the first-id log's limit (step 62 aborted on 9 of 15 runs of five skewed corpora); replica −0.11% (17 of 31) | run-995664fe93be, run-68dc93bf4db2, run-563148ea7a1e | 0.087268, 0.091365, 0.089198 | 30.45, 30.33, 29.96 | 1048 | 79.28, 78.78, 79.04 |
| 64 | 50f07bb (run from 5216711) | b-stack-13: pool workers that wait for a job map the corpus pages in ahead of the scan, one read per 64 KB (replica −0.787 ms, 29 of 31, on seed 1; −1.361 ms, 31 of 31, on holdout 99) | run-d84c9fde9fad | 0.085659 | 30.43 | 1059 | 79.45 |
| 65 | 97b4897 (later runs from b8b733a, 3bd65f7) | b-stack-14: the terms' info words built by a parallel round over ranges of the old ids, instead of one serial pass of 0.51 ms (replica −0.334 ms, 20 of 31; eight more A/Bs, two builds a side, all ≤ 0, mean −0.25 ms). Its first run was the best Track B run until step 66; the third came in a dispatch where C's unchanged code ran 0.020187 s, against 0.017134 to 0.018676 s in the next three | run-a0a52b4578ea, run-e3147d5de2df, run-ee50cf65988f | 0.085425, 0.086155, 0.093611 | 30.28, 30.21, 30.23 | 1062 | 79.47, 79.38, 78.48 |
| 66 | fabe12e (later runs from c36861d, 8a293ff and the control push 15388b9) | b-stack-15: the BOOL entries of a document's trigger terms evaluated 8 at a time in AVX2. Each fresh term's entry indices are stored 8 per vector into one list; each 8 entries take gathers of their terms, those terms' stamps and their truth tables; the matches (52% on seed 1) are left-packed by `pdep`/`pext` and `vpermd`, and only they are added. 32-byte entries, 8 padding entries that never match, the two-pass prefetch (`RANGES_AHEAD`) gone, and a BOOL query of over 6 terms stops the program (replica −2.730 ms, 29 of 31, and −2.298 ms, 30 of 31, against two builds of step 65). Its first run was the best Track B run until step 68. The second, 0.094116 s, is unexplained: Track A ran slow in all three of these dispatches, Track C normally in that one | run-b16bec586dd7, run-3e3ce97f718c, run-2a6f1b53f20b, run-4f16fee79168 | 0.082048, 0.094116, 0.082597, 0.089935 | 32.43, 32.17, 32.31, 34.16 | 1087 | 79.83, 78.34, 79.76, 78.84 |
| 67 | 1e8c3f0 | b-stack-16: less setup work per term (the truth tables in one pass, the cover cost kept as it goes, no key copies when the lines are split, a counting sort in `dedup_keys`), and malloc's mmap threshold fixed at 1 MB (replica, four A/Bs with two builds a side: mean −1.37 ms; its run in a slow dispatch) | run-a91e7b8576ba | 0.101006 | 32.24 | 1087 | 77.57 |
| 68 | 83a938d (later runs from 2b59127, 695cd44) | b-stack-17: the tail's page releases moved into one parallel round after the formatting, where the threads wait, and the `always` entries sorted by length, operators and first row, so consecutive entries read neighbouring rows (replica −0.793 and −1.037 ms, 29 and 30 of 31; first and third runs DISTURBED). Its second run had the best Track B points until step 70 | run-62b9b6e108c7, run-fc0b5c2ebe3e, run-c2b88acd45c9 | 0.103242, 0.079675, 0.100641 | 32.68, 31.67, 31.99 | 1096 | 77.31, 80.12, 77.59 |
| 69 | fc30062 (second run from 5719822) | b-stack-18: half-size intermediates when the plan is built (an entry keeps its cover term in its eighth word), so the plan's serial grouping reads half the bytes (replica −0.528 and −0.634 ms, 26 and 28 of 31). Its second run was the fastest Track B time until step 70 | run-7884f8311d08, run-65df9250e334 | 0.081637, 0.07961 | 32.49, 32.72 | 1099 | 79.85, 80.12 |
| 70 | 883c315 (second run from a09d036) | b-stack-19: the plan's sample 2.5 MB instead of 1 MB, for better document-frequency estimates, its documents' terms deduplicated by stamps instead of a sort (replica −0.813 and −0.641 ms, 27 and 29 of 31; about −1 ms on holdout 99 and fresh seed 109). Its first run was the best Track B run until step 77, the second came with B alone slow | run-aecc0a5f3a09, run-eb533f034fdc | 0.078573, 0.093372 | 32.06, 31.82 | 1099 | 80.27, 78.39 |
| 71 | 79fb02b | b-stack-20: review comments only (`trigger`'s SAFETY bounds, `DocEntry`'s layout, `par` under `panic = "abort"`, the module doc, `split_lines`); the token stream is step 70's | run-2a3e7f333147 | 0.078623 | 32.1 | 1099 | 80.26 |
| 72 | 4134ca1 (run from 14aa7c9) | b-stack-21: the eight padding entries placed by the plan's grouping as one more part, instead of an `extend` that reallocated and copied the 1 MB entries array on the main thread (0.76 → 0.20 ms); the 7+-term BOOL assert moved into `cover`, the first function every BOOL line reaches (replica −0.610 and −0.754 ms, 26 and 28 of 31; its run in a dispatch where Track A ran slow too, run-64335e22618d at 0.02058 s) | run-5e1129bac322 | 0.080472 | 32.49 | 1101 | 80.00 |
| 73 | f37f273 | b-stack-22: `look_up` keeps the document's anchor positions without a branch, as it does the trigger terms', and the anchor prefetch goes (replica −0.455 and −0.118 ms, 26 and 18 of 31; branch misses −11.8%; its run: B alone slow; step 74, whose scan has the same machine code by objdump, ran 0.078856 s) | run-8640ff23f7c7 | 0.095266 | 32.38 | 1101 | 78.17 |
| 74 | 9db18fc (run from 9b2ee21) | bj-2: the code split into modules (`pool`, `vocab`, `plan`, `scan`; `main.rs` keeps `main`, the slicing and the formatting) and six review items in the setup: the plan's records built by a function returning a named struct (`records`, `Records`), the hash's bucket placement as `place_regions` with bounds-checked stores, a compile-time check of `DocEntry`'s layout, the queries check once after `Plan::new`, the slice check on the slice's own ids, and `place` in plain statements (the scan's functions identical by objdump; replica −0.071 and +0.121 ms, +0.179 ms on holdout 99, against an A/A of +0.033: equal) | run-9c2169af7f30 | 0.078856 | 32.26 | 1147 | 80.09 |
| 75 | 41e6cfe | bs-5j2: the queries stay in the threads' splits, one part per thread, so `parse_queries` no longer copies every line and term into one array on the main thread, and `Plan::new` and `records` take a part each (replica −0.520 and −0.245 ms, 27 and 21 of 31, and −0.689 ms on holdout 99, 29 of 31; memory, max of 10 runs, 31.32 MB). Board 0.078867 s against step 74's 0.078856 s: equal, since 0.3 to 0.9% is below what one board run resolves; the gain rests on the replica's pairs | run-54d2972c3d09 | 0.078867 | 31.53 | 1142 | 80.10 |
| 76 | 433d16b (run from 1eb28f0; later runs from 12e48b1, bbce7e9) | Round-5 review comments on the bounds, the sample and the pool; the token stream is step 75's. Its first run came with B alone slow, its second in a dispatch where A was slow too; its third was the fastest Track B time until step 77 | run-1507537fc514, run-de3cde6a13a2, run-b86db6c1a285 | 0.094031, 0.124942, 0.077853 | 31.44, 31.27, 31.32 | 1142 | 78.19, 75.11, 80.24 |
| 77 | 3305ff6 (later runs from 9f2993f, abc7b44) | bt9-1: `look_up`'s probe loop stores only the info words; a second pass over them, 8 at a time, makes the presence stores, each stamp's line fetched for writing (`prefetchw`) just before its store so that a miss no longer holds up the stores after it, and left-packs the trigger and anchor lists 8 lanes per store (`vmovmskps`, `pdep`/`pext`, `vpermd`); keys loaded from the window's start, and the vocabulary, hash and table read once per scan (replica, PGO, two builds a side: −2.734 and −2.827 ms on seed 1, 30 of 31 each, −2.122 and −2.542 ms on holdout 99, 29 and 31 of 31; memory, max of 20 runs, 31.26 MB). Its third run was the best Track B run until step 78 | run-afcb0c2c9346, run-e7b46efd1891, run-567ede8911b8 | 0.074272, 0.075027, 0.074086 | 31.38, 31.44, 31.58 | 1155 | 80.72, 80.61, 80.74 |
| 78 | 51f656f (second run from a later dispatch, B unchanged) | bt9-2: a pending token in 16 bytes, the slot's offset written as a second 4-byte store over the key's top 4 bytes, which the probe does not compare; for each 8 tokens, their ids (`vpand`), stored over their info words, and their rows (`vpminud`) made at once; the superblock rows as four adjacent words again (word b of row r at 4r + b), so `end_super` reads a term's 256 bits in one 32-byte load (replica, PGO, two builds a side, each piece against the one before, means of two A/Bs: seed 1 −0.03, −0.42 and −1.05 ms, holdout 99 −0.69, −0.42 and −1.10 ms; memory, max of 20 runs, 31.16 MB). Its first run was slow, in a dispatch where A set its best time and C ran normally; its second was the best Track B run until step 81 | run-202e90d8f16b, run-81452d9a12ae | 0.093093, 0.072974 | 31.28, 31.47 | 1164 | 78.24, 80.88 |
| 79 | 03fe293 (later runs from 779a8bf and a later dispatch) | bsu-1: ALWAYS 0.30 → 0.40, since step 78's `end_super` reads one 32-byte row per term (non-PGO sweep against 0.30: 0.35 −0.35, 0.40 −0.75, 0.45 −0.47 ms; PGO at 0.40, two pairs of builds against step 78: seed 1 −0.450 and −0.480 ms, holdout 99 −0.346 and −0.524 ms; then seed 1 +0.026, −0.312, −0.122 and −0.236 ms, holdout 99 −0.432 ms); `parse_queries` stops with a message on a queries.txt that is empty or has no last LF (spec 2.3); memory, max of 20 runs, 31.34 MB. Its first run was slow with A slow too, its second slow with A and C normal; its third, 0.074609 s, is 2.2% over step 78's best (0.072974 s), inside the fast group (0.068 to 0.083 s), so a gain of 0.1 to 0.5 ms is below what one board run resolves | run-093744130f82, run-a361d7c46b11, run-670a94ac00ad | 0.093217, 0.092461, 0.074609 | —, 31.26, 30.9 | 1165 | 78.22*, 78.31, 80.64 |
| 80 | 7cb130c | bt9-3m: step 79 with one change in `scan.rs`, made on step 78's code: `left_pack` reads the kept lanes' numbers from PACK[mask], a 2 KB table built at compile time, one 8-byte load folded into `vpmovzxbd`, instead of making them with `pdep`, `imul`, `pext` and `vmovq` (replica, PGO, on step 78's code, two builds a side: seed 1 −0.361 and −0.279 ms, holdout 99 −0.263 and −0.209, fresh seeds 105 and 110 −0.180 to −0.442; mean of the eight −0.315 ms; memory, max of 20 runs, 30.93 MB). Its first run was slow, with A and C normal; its second normal | run-87863a1a8888, run-1527cbd364db | 0.093693, 0.073165 | 30.97, 31.72 | 1172 | 78.15, 80.83 |
| 81 | 6af0e44 | bsu-2: one plain release build instead of PGO (the same source built with PGO ran 0.55 to 0.67 ms slower), and the tail's releases spread over the threads: each output chunk once written, every slice's answer states, line ends and first ids, and the plan's phrase records and entries (exit 1.77 → 1.48 ms, RSS at the exit 16.3 → 10.9 MB) (replica, the plain build against step 80's two PGO builds: seed 1 −0.983 and −0.863 ms, holdout 99 −1.001 ms; memory, max of 20 runs, 30.91 MB). Its run was the best Track B run until step 82, with A slow in that dispatch | run-88a51030508d | 0.072172 | 31.42 | 1174 | 80.97 |
| 82 | ff31dee (runs from 8682b81 and a0e119b) | bsu-3: freed memory kept in the heaps (`M_TRIM_THRESHOLD` at 1 GB: a trim flushes the TLB of every CPU the threads ran on, and `Plan::new`'s drops took 0.25 ms with trims, 0.01 ms without), and `term_order` sorted from 4-byte keys, freed before `rank` so that `rank` takes over their pages (0.89 → 0.63 ms) (replica, plain builds, one a side: seed 1 −0.593 and −0.464 ms, holdout 99 −0.520 ms; the gate build against step 81's: seed 1 −0.315 ms, 24 of 31, holdout 99 −0.401 ms, 24 of 31; memory, max of 20 runs, 32.78 MB). Its second run was the best Track B run until step 83; its third, with A and C normal, is logged as disturbed | run-3cfc459bf70a, run-dd1e6fa41385, run-1590735a6a67 | 0.071995, 0.071937, 0.121495 | 32.42, 32.25, 32.13 | 1178 | 80.99, 81.00, 75.31 |
| 83 | c7c0c36 (second run from 58edfbd) | btr-1: the trigger's BOOL entries in 16 bytes instead of 32 (5 two-byte terms, a two-byte answer and the 4-byte truth table; the cover term moved out, and the matches' answers left-packed straight from the batch's words), and the trigger groups' range starts in 16 bits (214 → 107 KB); past 2^16 BOOL terms, lines or entries, the plan evaluates every BOOL query per superblock (replica, plain builds: seed 1 −1.277 and −1.458 ms, 31 and 30 of 31, holdout 99 −1.579 and −1.479 ms, 31 and 29 of 31; memory, max of 20 runs, 31.94 MB). Its second run was the best Track B run until step 84 | run-2d38c062dff3, run-62cfcf3cba73 | 0.06988, 0.06982 | 32.33, 32.2 | 1192 | 81.27, 81.28 |
| 84 | 82a5616 (runs from 90bc87f, 0dbb27c and ccc90a3) | bm-1: step 83 with bap-1's superblock counts: an `always` answer's increment for its 256 hits from `psadbw` byte sums and one `vpmuludq`, about 27 uops instead of 39 (bap-1 on step 82's code: seed 1 −0.737 and −0.928 ms, holdout 99 −0.753 and −0.665 ms; merged, the gate build against step 83's: seed 1 −0.580 ms, 29 of 31, holdout 99 −0.609 ms, 28 of 31; memory, max of 20 runs, 32.11 MB). Its first run came in a heavily disturbed dispatch; its third was the best Track B run until step 85 | run-944d12c72398, run-44708f294898, run-707010fe44c5 | 0.115912, 0.069944, 0.069276 | 31.63, 32.34, 32.42 | 1189 | 75.79, 81.27, 81.38 |
| 85 | 5257718 (second run from 446aee0) | bm-2: the batch reads its 8 entries with 8 aligned 16-byte loads, and 4 `vinserti128` and 8 unpacks transpose them, instead of gathering their words: 32 gathered elements become 8 loads (on step 83's code: seed 1 −0.532 and −0.601 ms, holdout 99 −0.532 and −0.531 ms; merged, the gate build against step 84's: seed 1 −0.673 ms, 30 of 31, holdout 99 −0.610 ms, 24 of 31; memory, max of 20 runs, 32.46 MB). Its first run was the best Track B run until step 88; its second came with A slow too | run-3835f14c495a, run-ea9947df82d6 | 0.068502, 0.143676 | 32.15, 31.4 | 1192 | 81.49, 73.45 |
| 86 | 1b4d9ff (later runs from d85e890, a0632f7 and 835cf16) | btr-3: each slice's first-id log, a 2 MB mapping of its own, given back with `MADV_DONTNEED` at the end of the merge and its empty mapping left to the exit, instead of twelve `munmap`s that took `mmap_lock` for writing one after the other (from the last merge to the end of the round 0.53 → 0.006 ms) (replica, plain builds, the gate build against step 85's: seed 1 −0.661 and −0.435 ms, 28 and 27 of 31, holdout 99 −0.281 and −0.180 ms; memory, max of 20 runs, 32.39 MB). Its first run came with A slow too, its second and third in dispatches where every track ran slow, its fourth with A slow too | run-4b47a698c5ee, run-0710212e6833, run-8a80992130bc, run-9248e505b340 | 0.098043, 0.079581, 0.090154, 0.070026 | 32.4, 32.07, 31.9, 32.35 | 1194 | 77.59, 79.86, 78.50, 81.25 |
| 87 | ba50248 (runs from afa2a36 and 4131672) | bl-1: 17 fewer code lines in `pool.rs`, `mmap.rs`, `plan.rs` and `scan.rs`, all outside the hot loops: `umwait`'s `asm!` on one line, `par`'s job coerced where it is made, a phrase's and an `always` entry's terms made by `std::array::from_fn`, the `csr` pairs read from the records in place, with the 8 padding entries an array chained after them. The scan's functions are step 86's by objdump (replica, plain builds, against step 86's: seed 1 −0.149 and +0.026 ms, 21 and 14 of 31, holdout 99 −0.126 and +0.024 ms, 18 and 14 of 31, a mean of −0.056 ms: the same speed; memory, max of 20 runs, 32.04 MB). Its first run came with A slow too | run-3259e1688e68, run-b46032503ace | 0.082291, 0.071064 | 31.97, 32.07 | 1177 | 79.54, 81.13 |
| 88 | 802dda6 (runs from 835b127, 68e2d9f and 0098691) | bl-2: 8 more lines cut in `plan.rs` and `scan.rs` (`classify` takes a whole part of the queries, so `Plan::with` calls it straight from `par`; the records call as a one-line closure; two `let`s in `trigger` and one in `Answers::add` merged into others), with the scan's hot functions step 86's by objdump and at step 86's addresses mod 64. The linker places functions in name order, so a size change in `main`, `Plan::with` or a `par` job moves the hot code in 16-byte steps: the same code 16 bytes off ran 0.126 and 0.172 ms slower, and a 0-line rewrite of the padding chain keeps it in place (replica, against step 86's build: seed 1 −0.083 and −0.145 ms, 19 and 18 of 31, holdout 99 +0.015 and −0.060 ms, 15 and 18 of 31, a mean of −0.068 ms: the same speed; memory, max of 20 runs, 32.11 MB). Its first two runs came with A slow too; its third was the best Track B run until step 89 | run-8bac7298d692, run-86d61fafbc8a, run-a28060420b19 | 0.088981, 0.088847, 0.067846 | 32.03, 31.97, 32.12 | 1169 | 78.71, 78.73, 81.66 |
| 89 | 480db54 (later runs from 4f6e8b0, cbc99fd, f10f4b7, 60f559f and 4b997e4) | bnm-1: std's start-up skipped (`#![no_main]`, `main` called by the C runtime, as in Track A): no stack-guard lookup through /proc/self/maps, and no SIGSEGV and SIGBUS handlers with an alternate signal stack, which each of the 11 spawned threads mapped too (strace, tiny seed 201 at 8 CPUs: 352 → 286 system calls); the data paths joined by `concat` instead of `format!`, which keeps the scan's hot functions at step 88's offsets mod 64 (with `format!` they sat 32 bytes off: −0.085 ms for `concat`) (replica, plain builds, against step 88's: a tiny input −0.118 and −0.125 ms, 98 and 96 of 101; seed 1 −0.211 and −0.214 ms, 20 and 24 of 31, holdout 99 −0.080 and +0.058 ms, 17 and 14 of 31, a mean of −0.112 ms; 30 fresh full seeds, mean −0.118 ms, median −0.121; memory, max of 20 runs, 31.71 MB). Its first run came with A slow too, its third with B alone slow; its sixth, run-a83c16179001, was the best Track B run until step 90 | run-f7d43533ffb5, run-0fc9764834f2, run-d7f91eb8db60, run-e4fc11287808, run-a4259bf6753c, run-a83c16179001 | 0.103705, 0.067543, 0.135523, 0.068272, 0.067464, 0.067066 | 31.42, 31.92, 31.17, 32.03, 31.78, 31.82 | 1171 | 77.05, 81.70, 74.14, 81.59, 81.71, 81.78 |
| 90 | 798ff2d (later runs from 9ccf3cb and 9514121) | bpl-1: `plan.rs` only. The entries' lines numbered in the plan's grouping round, right after their `csr`, by the thread that groups the entries (`number`, which replaces `Plan::number_answers`), then the `always` and phrase lines; the round as `par(3)`, so that the `always` sort has a thread of its own (the round 1.265 → 1.091 ms); `term_order` gives each term the next id of its key's run, without its scatter; the info words made in place. The whole plan 3.609 → 2.685 ms (phase marks, seed 1, medians of 9), with the scan's hot functions at step 89's offsets mod 64 (replica, plain builds, against step 89's: seed 1 −0.899 ms, 26 of 31, holdout 99 −0.883 ms, 31 of 31; fresh full seeds 101-116 all faster, mean −0.859 ms; memory, max of 20 runs, 32.00 MB). Its first run came with B alone slow, its third in a disturbed dispatch with A slow too; its second is the best Track B run | run-2e8c0a9b6f41, run-9c27a7fdfe8f, run-77f4c7adf864 | 0.077266, 0.066749, 0.147443 | 31.32, 31.98, 31.05 | 1175 | 80.23, 81.82, 73.21 |

**Code-quality pass (5a0612d, step 47; also pushed as 7f08fe7 and 045a6b5), on the box:**
run-5892b2f7eb00 0.144406 s (a slow dispatch), run-eb924b66b4aa 0.115198 s (runs.log 09:47Z) and
run-29fa59a9bf1f 0.108491 s and 77.40* points (computed with the fitted formula; its M is not in our logs). The pass itself changed no hot
instruction: `Scan::run`, `end_block` and `end_super` are the same instructions at the same
offsets as step 44's in local PGO, local plain and test-server PGO builds (objdump, 0 differing),
and two PGO builds of each time the same on the test server (medians of 31 interleaved rounds of
11 runs: 80.90 and 81.02 ms for step 44, 81.05 and 80.89 for the quality pass).

**Steps 76 to 89 (433d16b to 480db54), S 1142 → 1171**: one step of comments (76) and thirteen
code steps, each measured on the replica (ab.sh, 31 pairs): steps 77 to 80 with two PGO builds a
side, step 81's plain build against step 80's two PGO builds, and steps 82 to 89 with one plain
build a side (steps 87 and 88 against step 86's build, step 89 against step 88's). Steps 83, 85
and 86 come from one round on the trigger and the first-id log, step 84 from one on the
superblocks, and steps 87 to 89 from one on lines and the start-up.
- *Step 76.* Step 75 with the round-5 review comments on the bounds, the sample and the pool, the
  same code token for token. Its first board run, run-1507537fc514 (0.094031 s, 31.44 MB, 78.19
  points), came in a dispatch where A and C ran normally: one of B's slow runs, like step 73's
  0.095266 s. The second, run-de3cde6a13a2 (0.124942 s), came with A slow too; the third,
  run-b86db6c1a285 (0.077853 s, 80.24 points), was the fastest Track B time until step 77.
- *Step 77 (bt9-1).* Only `scan.rs` changed, in five pieces (pw1, c1, f2, w1 and h1), each
  measured on the replica against the one before (ab.sh, 31 pairs, non-PGO screens on seed 1),
  then the whole step with PGO (the production Dockerfile, two builds a side). Board:
  run-afcb0c2c9346, 0.074272 s (80.72 points), run-e7b46efd1891, 0.075027 s (80.61), and
  run-567ede8911b8, 0.074086 s (80.74), the best Track B run until step 78.
  - *Bottleneck.* Timing-only builds of step 76 (wrong output; non-PGO, 31 pairs, full seed 1)
    split its 77.7 ms into 21.0 ms for the phrases, the triggered evaluation and the `always`
    superblocks, 18.9 ms for the rest, and 37.9 ms for the token path: extraction 11.72 ms, the
    hash, pilot, slot and prefetch 7.28, the presence stores 8.19 (row ORs 3.93, stamps 2.84),
    the `pending` and `look_up` stores 6.22 and the probe 4.66; the trigger and anchor lists'
    scalar stores alone cost 3.66. The token path ran about 70 uops per token at about 4.5 per
    cycle per core, close to what the core can rename, and the store buffer was full
    (`resource_stalls.sb`) in 168M of 1,620M cycles, about 119M of them from the stamp store
    (13M read-for-ownership requests). Stores commit to L1 in program order, so a stamp store
    whose line is not in L1 holds up every store after it until the line arrives.
  - *Design.* The probe loop now stores only the info words. A second pass takes them 8 at a
    time. For each token it fetches the stamp's line for writing (`prefetchw`, `_MM_HINT_ET0`)
    just before the stamp store, then writes the stamp and the row bit as before. A prefetch is
    not a store: it runs out of order, as soon as its address is known, so the line is on its way
    long before the store's turn to commit. For the 8 tokens together it loads their info words
    into one AVX2 register and left-packs the trigger occurrences (TRIG, bit 30, shifted to the
    sign bit) and the anchors' positions (ANCHOR, the sign bit): `vmovmskps` gives an 8-bit mask,
    `pdep` spreads it to a byte per lane, `pext` picks those lanes' numbers, `vpermd` moves the
    kept lanes to the front, and one 32-byte store writes them, where the scalar code made two
    stores and two adds per token. It is step 66's pack, now one function, `left_pack`, which
    `trigger` shares with the same machine code. Keys are loaded from the window's start plus the
    token's offset, with no `at + b` per token. And `run` reads the vocabulary, the hash and the
    table's address once per scan: read in the loop, LLVM loaded them again after every run of
    tokens and kept the table's address on the stack, one reload per token. (An earlier try of
    the same move, on the test server, left the instructions unchanged: experiments table.)
  - *Why it is exact.* The same stores and the same lists. Every token's stamp and row bit are
    written, only in a later loop, and a prefetch changes no data. `left_pack` keeps exactly the
    lanes whose flag bit is set, in order, so `trig` and `anchors` hold what the scalar code kept;
    the spare lanes are overwritten by the next store or lie past the list's end, inside the room
    each list reserves (one entry per token, plus 8). The last 8 ids may include some of the 8
    `miss` ids that `look_up` writes after the tokens: `miss` has no flag, so no list keeps one,
    and its stamp and row bit go to the sinks, `miss`'s own stamp and the sink row, which no
    evaluation reads, as for any token of no query term.
  - *Measured.* Non-PGO screens on seed 1, each piece against the one before: the `prefetchw`
    (pw1) −1.709 ms (30 of 31; store-buffer stalls 161M → 50M cycles), the left-pack (c1) −0.711
    ms (30 of 31), the presence stores moved into the same 8-token pass (f2) −0.624 ms (28 of 31),
    keys from the window's start (w1) −0.359 ms (24 of 31), and the vocabulary, hash and table
    read once (h1) −0.773 ms (29 of 31): −4.217 ms in all (31 of 31). With PGO, two builds a
    side: −2.734 and −2.827 ms on seed 1 (30 of 31 each), −2.122 and −2.542 ms on holdout 99 (29
    and 31 of 31), against an A/A of +0.256 ms; every build gave the seed-1 output f86973c21022….
    A first tidy-up that reordered statements in `look_up` got another register allocation and
    ran 0.3 to 0.7 ms slower with PGO. So the submitted `look_up`, `run` and `end_doc` are h1's,
    identical by normalized objdump (non-PGO), and `trigger`, which now calls the shared
    `left_pack`, has the same machine code as before; h1's own PGO builds measured −2.985 and
    −2.926 ms on seed 1 (31 and 30 of 31) and −2.489 and −2.164 ms on holdout 99 (30 and 31 of
    31). The final gate passed ("How the final number was verified"); memory, max of 20 runs,
    31.26 MB. S 1142 → 1155.
  - *Not kept* (experiments table): the `prefetchw` earlier, in the probe loop (+1.404 ms), the
    probes and the presence in one loop again (+2.156 ms), and four more variants of the stores
    and the window loop.
- *Step 78 (bt9-2).* Only `scan.rs` changed again, in three pieces, each measured with PGO against
  the one before (the production Dockerfile, two builds a side, ab.sh, 31 pairs), after non-PGO
  screens. Its first board run, run-202e90d8f16b (0.093093 s, 31.28 MB, 78.24 points), was slow:
  in the same dispatch Track A set its best time then (run-b3a4984e4320, 0.015715 s) and Track C ran
  normally (run-7435088e4032, 0.017202 s); it is one of B's slower group of board runs, which the
  replica does not show ("How the final number was verified", the box number). Its second,
  run-81452d9a12ae (0.072974 s, 31.47 MB, 80.88 points), was the best Track B run until step 81.
  - *Bottleneck.* Three costs were left around the presence stores and the superblocks. A pending
    token took 32 bytes, a 16-byte key and an 8-byte offset padded to the key's alignment: twice
    its data. Each presence store made its own id (`& ID`) and row (`id.min(rows)`, a compare and
    a `cmov`): 3 uops a token. And `end_super` built each term's 256 bits from four 8-byte loads on
    four lines 72 KB apart, because step 56 had laid the row words out block by block, so that a
    document's row updates fell in one contiguous array.
  - *Design.* (1) A pending token is 16 bytes: its key (12 bytes and zeros) as one store, then the
    slot's byte offset as a second, 4-byte store over the key's top 4 bytes; the probe loop reads
    the offset back with a 4-byte load. Both are `volatile`, so that each stays one instruction:
    otherwise LLVM inserts the offset into the key with `vpinsrd` or extracts it from the key's
    16-byte load, 2 uops either way (the `vpinsrd` form, in step 77's experiments, ran 0.4 ms
    slower). So a token still costs two stores and two loads, over half the lines. The cold paths
    for long tokens insert the offset with `vpinsrd`. (2) For each 8 tokens, one `vpand` makes
    their ids, stored over their info words in `toks`, and one `vpminud` and one shift make their
    rows' word offsets, 4 · id.min(rows), into an 8-word array that the presence stores read: 5
    uops for the 8, where `& ID`, a compare and a `cmov` took 3 per token. (3) `wide` is row by row
    again, word b of row r at 4r + b, so `end_super` reads a term's 256 bits with one 32-byte load
    from one line, and the presence store's offset is the shift in (2). This reverses step 56. With
    the presence stores made 8 at a time, the stamps' lines fetched ahead (step 77) and the rows'
    offsets computed 8 at a time (2), the one-load `end_super` wins; on piece 1 alone, where each
    token still multiplied its row by 4, the same layout gained about nothing (below).
  - *Why it is exact.* (1) The probe compares only the 12 key bytes: its `vptest` mask leaves out
    the top 4, so the offset stored there changes no comparison, and the info word it returns is
    loaded from the slot, not from the token. Offsets fit 32 bits: the table is far below 4 GB.
    (2) `vpminud` is the unsigned 32-bit minimum, which is `id.min(rows)` for these values. The
    trigger and anchor lists are packed from the info words loaded before the ids are written over
    them, so they keep their flags, and `end_doc` masks every token word it reads with ID (the
    anchors and the phrase compares); in the sample the table has no flags, so the info words are
    the ids. (3) The same words with the same bits, only placed row by row: a token sets bit
    `doc % 64` of word `block` of its row, and `end_super`'s load puts block b in lane b, as
    `_mm256_set_epi64x` did. The clear after each superblock covers the real rows; the sink row is
    never read and, as before, never cleared.
  - *Measured.* With PGO, two builds a side, each piece against the one before; wins of 31 in
    brackets. (1) Seed 1 +0.068 and −0.125 ms (12 and 19), holdout 99 −0.673 and −0.702 ms (28 and
    29), fresh seed 105 −0.261 and −0.294 ms (24 and 20); non-PGO −0.234 ms (21). (2) Seed 1
    −0.471 and −0.364 ms (27 and 26), and −0.469 ms (28) against a third build of (1); holdout 99
    −0.534 and −0.300 ms (27 and 24); non-PGO −0.716 ms (26). (3) Seed 1 −1.153 and −0.956 ms (31
    and 31), holdout 99 −1.094 and −1.113 ms (30 and 31). On piece 1 alone, the rows of four words
    gave seed 1 −0.102 and −0.105 ms and holdout 99 −0.241 and +0.134 ms (non-PGO −0.567): each
    token's multiply of its row by 4 cost what `end_super` gained, until (2) made it one shift
    per 8 tokens. In all, about −1.5 ms on seed 1 and −2.2 ms on holdout 99. The final gate passed
    ("How the final number was verified"); memory, max of 20 runs, 31.16 MB. S 1155 → 1164 (the
    rows of four words alone −2).
  - *What is left.* Timing-only builds of step 78, as for step 77 (non-PGO, 31 pairs, full seed
    1): 73.4 ms in all, 20.9 ms for the phrases, the triggered evaluation and the `always`
    superblocks, 18.9 ms for the rest, and 33.3 ms for the token path, against step 76's 37.9:
    extraction 11.93 ms, the presence stores 5.27, the probe about 4.8, the hash, pilot, slot and
    prefetch 4.73, the `pending` stores and the probe loop's other work 3.84, and the trigger and
    anchor left-packs 2.83.
  - *Not kept* (experiments table): probes in fixed batches of 32 during the scan (+0.462 ms),
    the probe fused into the extraction loop with no prefetch and no `pending` (+3.353 ms), no
    slot prefetch at all (+1.412 ms), the probe loop unrolled by 8 (the same speed), and the token
    loop as a do-while (the same machine code).
- *Step 79 (bsu-1).* Step 78 with two changes, in `main.rs` and `vocab.rs`; `scan.rs` is step
  78's. The seed-1 output is f86973c21022, as before.
  - *ALWAYS 0.30 → 0.40.* A query whose cover terms are expected in more than ALWAYS of the
    64-document blocks, summed over the terms, is evaluated once per superblock instead of per
    document (README, design 4). Step 78's `end_super` reads one 32-byte row per term, so a
    superblock evaluation got cheaper, and more queries pay there. A sweep on step 78's code
    (non-PGO, against 0.30): 0.25 +0.75 ms, 0.35 −0.35, 0.40 −0.75, 0.45 −0.47, 0.50 −0.36 and
    0.60 +0.23 ms. With PGO at 0.40, a first pair of builds a side: seed 1 −0.450 and −0.480 ms
    (27 and 26 of 31), holdout 99 −0.346 and −0.524 ms (28 and 27 of 31). A second pair: seed 1
    +0.026 (15 of 31), −0.312 (22), −0.122 (19) and −0.236 ms (23), holdout 99 −0.432 ms (26),
    where the two builds of step 78 differ by 0.116 ms between themselves (20 of 31). Eight of
    the nine A/Bs are negative: about −0.1 to −0.5 ms. ALWAYS decides only where a query is
    evaluated, never its result.
  - *queries.txt must end with LF.* Spec 2.3 has one query per line, each ending with LF.
    `parse_queries` now asserts that the file ends with LF ("queries.txt: empty, or no last LF"),
    so an empty file and a last line without LF both stop the program with that message (exit
    134). Before, an empty file stopped on an index out of bounds in `number_answers` (the 8
    padding entries carry answer 0, and `answer_of` was empty), and a last line without LF was
    parsed with its last byte cut, as if it were the LF: a BOOL query lost the last letter of its
    last term, and a phrase a letter and its closing quote, with no error.
  - *Verified.* The final gate passed ("How the final number was verified"); memory, max of 20
    runs, 31.34 MB. S 1164 → 1165, the assert's one line.
  - *Board.* run-093744130f82, 0.093217 s, with Track A slow in the same dispatch too
    (run-19e874ad85e8, 0.018389 s) and Track C normal (run-c506d7aa651d, 0.017222 s);
    run-a361d7c46b11, 0.092461 s, with A and C normal; run-670a94ac00ad, 0.074609 s (80.64
    points). A later A/B of the two gate builds, step 79's against step 78's on seed 1: −0.392 ms
    (28 of 31).
- *Step 80 (bt9-3m).* Step 79 with one change in `scan.rs`, made and measured on step 78's code
  (bt9-3); the merged step passed the final gate.
  - *Bottleneck.* `left_pack` keeps the trigger and anchor lists in `look_up`'s 8-token chunks and
    the matches in `trigger`. It made the kept lanes' numbers with `pdep`, `imul`, `pext` and
    `vmovq`: 4 uops per pack, on a path whose uops count (step 77).
  - *Design.* The lane numbers are now PACK[mask]: 256 entries of 8 bytes (2 KB), built at
    compile time, read by one 8-byte load that folds into the `vpmovzxbd` widening them. Entry m
    is m's lowest set bit, then entry m & (m − 1) a byte higher; the unused bytes are 0, so the
    spare lanes take lane 0's value, as before. An earlier version of step 66 had such a table,
    together with a work list of ranges; the version kept then, with `pdep`/`pext`, one flat
    index list and the presence bits shifted in, was 0.697 ms faster, so the table alone had not
    been measured (experiments table).
  - *Why it is exact.* Entry m lists exactly the set bits of m, in increasing order, as
    `pdep`/`pext` did; the permute, the store and the count (the mask's popcount) are unchanged.
  - *Measured.* With PGO on step 78's code, two builds a side: seed 1 −0.361 and −0.279 ms (25 and
    21 of 31), holdout 99 −0.263 and −0.209 ms (28 and 21), fresh seed 105 −0.180 and −0.442 ms
    (26 and 27), fresh seed 110 −0.366 and −0.419 ms (23 and 23): mean −0.315 ms; non-PGO
    −0.236 ms (22). Its gate build against step 78's: seed 1 −0.267 ms (21), holdout 99
    −0.284 ms (24). Steps 77, 78 and this change together, against step 76's build: −4.338 ms
    (−5.68%, 30 of 31) on seed 1 and −4.663 ms (−6.11%, 31 of 31) on holdout 99. Merged onto
    step 79: memory, max of 20 runs, 30.93 MB; S 1165 → 1172. Board: run-87863a1a8888,
    0.093693 s (30.97 MB, 78.15 points), a slow run, with A and C normal; run-1527cbd364db,
    0.073165 s (31.72 MB, 80.83 points).
  - *Not kept* (experiments table): the superblock rows 32-byte aligned (not built), the next
    entries' rows prefetched in `end_super`, the anchors as bits, the sample's counts per thread,
    one bounds check per window in `masks`, a `prefetchw` of the row word, no `prefetchw` of the
    stamp (+1.467 ms), and every document looked up one document late.
- *Step 81 (bsu-2).* Step 80 with two changes, in `main.rs` and the Dockerfile; `scan.rs`,
  `vocab.rs` and the rest are step 80's, and the seed-1 output is f86973c21022, as before.
  - *One plain release build.* The Dockerfile is one `RUN` line, `cargo build --release` with
    `target-cpu=native` and `crt-static`, in the same two stages and images. The same sources
    built both ways: the plain build ran 0.553 and 0.637 ms faster than two PGO builds on seed 1
    (26 and 30 of 31) and 0.668 ms faster on holdout 99 (30 of 31). The likely cause is the PGO
    training run, at the sample scale (`b_index.py --scale sample`), which no longer matches the
    hand-tuned scan. A plain build is deterministic: the gate's build through the Dockerfile gave
    the same binary as the screening build (4a661174044b), so the board's build cannot differ by
    layout, and an A/B needs one build a side. S −4, the Dockerfile's code lines.
  - *The tail's releases.* In the last `par` round, the main thread gives each output chunk back
    to the kernel once written, and the workers give back, in shares, every slice's answer
    states, line ends and first ids (slice 0's too, which step 80 never released) and the plan's
    phrase records and entries, besides the corpus and table shares they released before. RSS
    left at the exit (smaps just before it): 16.3 → 10.9 MB. Without PGO, 14 runs each: the exit
    (from the last phase mark to the process end) 1.77 → 1.48 ms, and the round itself ends at
    the same time. A/Bs on PGO builds: −0.877 and −0.689 ms on step 79 (30 and 28 of 31);
    −0.332, −0.054, −0.272 and −0.367 ms (22, 17, 24 and 25 of 31) and −0.461 ms on holdout 99
    (24) on step 80. Without PGO the gain is smaller: −0.095, +0.360 and −0.098 ms (19, 14 and 17
    of 31), −0.248 ms on holdout 99 (24), while the exit probe holds at −0.29 ms. S +6.
  - *Measured, the whole step.* The plain build against step 80's two PGO builds, which is how
    step 81 compares with step 80 (the release round alone was smaller without PGO): seed 1
    −0.983 and −0.863 ms (30 and 31 of 31), holdout 99 −1.001 ms (29 of 31). The final gate
    passed ("How the final number was verified"); memory, max of 20 runs, 30.91 MB. S 1172 →
    1174.
  - *Not kept* (experiments table): `malloc_trim(0)` in the release round, the query file and
    the plan's line tables released too, `term_order` without its keyed copy, the plan's grouping
    on three threads, `vocab.finish` in parallel, SAMPLE_BYTES swept again, and MIN_SLICES.
  - *Board.* run-88a51030508d, 0.072172 s (31.42 MB, 80.97 points), the best Track B run until step
    82; in that dispatch Track A ran slow (run-b0e354a4edde, 0.018269 s) and Track C normally
    (run-3d775a9cee36, 0.017328 s).
- *Step 82 (bsu-3).* Step 81 with two setup changes, in `mmap.rs` and `plan.rs`.
  - *Freed memory kept in the heaps.* `set_up_memory` sets `M_TRIM_THRESHOLD` to 1 GB. A trim (an
    `sbrk` shrink) flushes the TLB of every CPU the threads ran on, and each phase reuses what the
    one before freed: `Plan::new`'s drops took 0.25 ms with trims, 0.01 ms without. On its own
    against step 81: seed 1 −0.218 and −0.507 ms (23 and 25 of 31), holdout 99 −0.274 ms (23).
    Memory, max of 20 runs, 32.47 MB against 30.94 MB. On step 77's PGO builds the same setting
    was within their spread (experiments table).
  - *Term order from 4-byte keys.* `term_order` sorts the keys alone as `u32`s, paired with their
    ids by `csr`'s closure, instead of (key, id) pairs, and frees them before `rank` is allocated,
    so `rank` takes over their pages instead of faulting in fresh ones: 0.89 → 0.63 ms. On its
    own: seed 1 −0.147, −0.192 and −0.110 ms (21, 21 and 18 of 31), holdout 99 −0.180 ms (21).
  - *Why it is exact.* The heaps only keep memory they would have returned, and the sort's keys
    are the same keys in the same order.
  - *Measured.* The whole step: seed 1 −0.593 and −0.464 ms (27 and 22 of 31), holdout 99
    −0.520 ms (28); the gate build against step 81's: seed 1 −0.315 ms (24), holdout 99
    −0.401 ms (24). ALWAYS swept again on the plain build, against 0.40: 0.35 +0.194, 0.45
    +0.733, 0.50 +0.664 ms, so 0.40 stays. The final gate passed; memory, max of 20 runs,
    32.78 MB; S 1174 → 1178.
  - *Board.* run-3cfc459bf70a 0.071995 s (80.99 points) and run-dd1e6fa41385 0.071937 s (81.00, the
    best Track B run until step 83); run-1590735a6a67, 0.121495 s, with A and C normal, is logged
    as disturbed.
- *Step 83 (btr-1).* The trigger's BOOL entries in 16 bytes, and its groups' range starts in 16
  bits; `plan.rs` and `scan.rs`.
  - *What the trigger does per document* (seed 1): 18.0 fresh trigger terms (repeats 0.3%), 27.2
    entries evaluated in 3.84 batches of 8, 16.1 matches (59%). An entry has 1.86 real other terms
    on average, and only 0.07 of them are present. 31,508 of the 53,444 BOOL terms trigger, and
    89% of their ranges hold one entry. So the batch's work does not shrink with the input: its
    cost is its instructions and its lines.
  - *Design.* An entry was 8 words: 5 terms, the truth table, the answer and the cover term. It is
    now 16 bytes, `repr(C, align(16))`: 5 two-byte terms, a two-byte answer and the 4-byte truth
    table. The cover term moved out (the plan groups (cover, entry) pairs). A batch gathers 4
    words per 8 entries instead of 6, and left-packs the matches' answers straight from their
    entries' words, so `add_doc` no longer loads the entry again. The entries take 580 KB instead
    of 1.16 MB, and `group`, each trigger term's range start, is `u16`: 107 KB instead of 214, half
    the lines for the first pass.
  - *Why it is exact.* Unused term slots now hold term 0, not a term in no document. The truth
    table is built over the k real terms, so its bits from 2^k on repeat the first 2^k: whatever
    term 0's presence, the lookup lands on the same bit. Sixteen bits hold the spec's inputs (about
    53,000 BOOL terms, 50,000 lines and 36,000 entries at full scale; seed 1: 53,444, 50,000 and
    36,174). Past 2^16 of any of them, `Plan::new` plans again with every cover's cost raised over
    ALWAYS, so every BOOL query is evaluated per superblock, which is exact for any query (0.35 s
    on seed 1 when forced). Nothing aborts or wraps; more than 2^16 answers were already refused.
  - *Measured.* The 16-byte entries alone −1.212 ms (29 of 31); the 16-bit groups on top −0.296
    and −0.259 ms (23 and 22), holdout 99 −0.187 ms (24). The whole step against step 82: seed 1
    −1.277 and −1.458 ms (31 and 30 of 31), holdout 99 −1.579 and −1.479 ms (31 and 29). The
    final gate passed; memory, max of 20 runs, 31.94 MB; S 1178 → 1192.
  - *Board.* run-2d38c062dff3 0.06988 s and run-62cfcf3cba73 0.06982 s (81.28 points, the best
    Track B run until step 84).
- *Step 84 (bm-1).* Step 83 with bap-1's superblock counts, merged by hunk: only `add_wide` and
  `count_positions` change.
  - *Bottleneck.* On step 82's code, skipping `end_super` saved 7.18 ms (31 of 31), and the count
    of an `always` answer's 256 hits 1.77 ms of it. That increment, count << SUM_BITS | count ·
    base + positions, came from 16-bit `maddubs` pairs, two `madd`s, a shift and a horizontal add
    of eight u32 lanes, then was unpacked and repacked in scalar code: about 39 uops.
  - *Design.* Per 64-bit word, `psadbw` adds up the nibble tables' counts and own positions; the
    byte's place in its word times its count (`maddubs` with 0 to 7) is a third `psadbw`; one
    `vpmuludq` makes n · (base + 64w); the four packed word increments are added, and the u64 goes
    to the state: about 27 uops. S −3.
  - *Why it is exact.* The same integer sums by other instructions, and no partial sum leaves its
    lane: a place times a count is at most 104, a byte.
  - *Measured.* On step 82's code: seed 1 −0.737 and −0.928 ms (28 and 27 of 31), holdout 99
    −0.753 and −0.665 ms (28 and 27). Merged, the gate build against step 83's: seed 1 −0.580 ms
    (29 of 31), holdout 99 −0.609 ms (28). The final gate passed; memory, max of 20 runs,
    32.11 MB; S 1192 → 1189.
  - *What the superblocks do* (seed 1): 4,342 `always` entries (1 to 6 terms: 173, 1,740, 1,319,
    685, 287 and 138) over 7,441 rows (`wide`, 238 KB a thread), 984 superblocks, 4.27M entry
    evaluations, 10.5% of them all zero and 6.7% still logging first ids; 12.4M row loads, 41.7%
    of them of all-zero rows. Skipped one at a time (wrong output, timing only, on step 82's
    code): `add_wide`'s logging path, against its count path, 1.31 ms; the `wide` clear 0.64 ms;
    the per-term operator dispatch, for an OR-only fold, 0.88 ms; the phrases 2.72 ms, their
    checks alone 1.14 ms. The anchor prefetch loop pays: without it the run was 0.24 ms slower, and
    alone, with the checks off, it costs 1.58 ms, about 49 thread-cycles per anchor occurrence
    (967K occurrences, 1.086M checks, 1.8% hits).
  - *Board.* run-944d12c72398, 0.115912 s, in a heavily disturbed dispatch; run-44708f294898
    0.069944 s; run-707010fe44c5 0.069276 s (81.38 points, the best Track B run until step 85).
  - *Not kept* (experiments table): shared prefixes, skipping after a zero first row, and a 2-id
    phrase pre-check, all three dropped on their counts; two other orders of the logging and the
    counting (+0.40 and +0.744 ms).
- *Step 85 (bm-2).* Step 84 with the batch's entries loaded, not gathered; `scan.rs` only.
  - *Bottleneck.* A fake with every stamp lane on one address ran 2.152 ms faster; LLVM had merged
    the five identical gathers into one and dropped two unused entry gathers, so it measured six
    gathers fewer per batch: about 0.36 ms per gather per batch over the run, however many lanes
    are live. With the stamps from one hot 128-byte line, their misses alone were worth 0.655 ms,
    so most of a gather's cost is the instruction itself.
  - *Design.* The batch reads its 8 entries with 8 aligned 16-byte loads; 4 `vinserti128` put
    entries j and j + 4 in one vector, and 8 unpacks transpose them, so lane j of word k is word
    k of entry j. 32 gathered elements become 8 loads; the stamps are still gathered.
  - *Why it is exact.* The transpose puts the same words in the same lanes the gathers filled, and
    the evaluation after it is unchanged.
  - *Measured.* On step 83's code: seed 1 −0.532 and −0.601 ms, holdout 99 −0.532 and −0.531 ms.
    Merged, the gate build against step 84's: seed 1 −0.673 ms (30 of 31), holdout 99 −0.610 ms
    (24). The final gate passed; memory, max of 20 runs, 32.46 MB; S 1189 → 1192.
  - *Board.* run-3835f14c495a, 0.068502 s (32.15 MB, 81.49 points), the best Track B run until
    step 88; run-ea9947df82d6, 0.143676 s, with Track A slow too.
- *Step 86 (btr-3).* Step 85 with the first-id logs given back in place; `main.rs` only.
  - *Bottleneck.* Each slice's first-id log (answers × 10 + 1 u32s, 2 MB) is a mapping of its own,
    over the 1 MB `M_MMAP_THRESHOLD`. Dropped at the end of the merge, the 12 threads unmapped
    their logs at once, and `munmap` takes `mmap_lock` for writing, so they ran one after the
    other: 0.53 ms between the last merge and the end of the round.
  - *Design.* The merge gives the pages back with `MADV_DONTNEED`, which takes the lock for
    reading, and forgets the vector, leaving its empty mapping to the exit: the gap is now
    0.006 ms. Two other forms measured: the log kept in the results and released in the release
    round, −0.520 and −0.331 ms but 8 more lines; leaked only, −0.181 and +0.085 ms, as the exit
    then unmaps it alone.
  - *Why it is exact.* The pages are given back after the merge has read the log, and nothing
    reads it again.
  - *Measured.* The gate build against step 85's: seed 1 −0.661 and −0.435 ms (28 and 27 of 31),
    holdout 99 −0.281 and −0.180 ms. Steps 83, 85 and 86 together: about −2.4 ms. The final gate
    passed; memory, max of 20 runs, 32.39 MB; S 1192 → 1194.
  - *What is left* (seed 1; fakes with wrong output, on this round's code): the answer states hot
    (`add_doc` on answer & 1023) −1.769 ms, mostly the first-id log rather than state misses,
    since with 1,024 answers each stops logging at 10 ids and the log stays near 10K entries a
    thread instead of about 120K; the merge's log loop skipped −0.436 ms, and with the log's
    writes during the scan removed too a further −1.109 ms (about 120 page faults a thread at
    4.4 µs each, plus the user cycles). Phase marks (medians of 9): sampled at 4.3 ms, planned at
    7.9, `Scan::new` 0.46 ms a thread, the scans end at 61.5 to 62.3, the merges at 62.9 to 63.2,
    formatted at 64.5 and released at 65.6; the workers' mapping of the corpus ends at 5.9.
  - *Not kept* (experiments table): 18 variants of the trigger, the answer numbering, and the log,
    merge and output.
  - *Board.* run-4b47a698c5ee, 0.098043 s (77.59 points), with Track A slow too (run-13f4d67ceb2b,
    0.018704 s) and Track C normal (run-bf2719c9e0fc, 0.017207 s); run-0710212e6833, 0.079581 s,
    and run-8a80992130bc, 0.090154 s, in dispatches where every track ran slow; run-9248e505b340,
    0.070026 s (32.35 MB, 81.25 points), with Track A slow too (run-8dc452c4fd51, 0.0197 s). None
    beat step 85's 0.068502 s: the replica's −0.2 to −0.7 ms is inside the board's spread.
- *Step 87 (bl-1).* Step 86 in 17 fewer code lines, in `pool.rs`, `mmap.rs`, `plan.rs` and
  `scan.rs`; `vocab.rs` and the Dockerfile are step 86's, and `main.rs` only drops `take` from its
  `std::mem` import.
  - *Cuts* (code lines, counted after rustfmt). `pool.rs` 53 → 47: `umwait`'s `asm!` on one line,
    its deadline halves written `!0`, all ones as before (−5), and `par`'s job closure coerced to
    `&dyn Fn` where it is made instead of bound twice (−1). `mmap.rs` 40 → 39: its two `std` uses
    as one. `plan.rs` 248 → 240: the unused `Default` derive of `Records` (−1); a phrase's and an
    `always` entry's terms made by `std::array::from_fn`, the words then zeros, instead of a zeroed
    array and `copy_from_slice`, the phrase built inside its `push` (−3; the over-8 and over-6
    asserts still run first); the sorted `always` entries stored in the plan first, so that
    `number_answers` numbers `plan.always` and takes no slice (−1); the ANCHOR test as a closure
    of its own, so that `flag` fits one line (−1); and the `csr` pairs read from the records in
    place, the 8 padding entries an array chained after them, with no `mem::take` into
    `by_trigger` and `by_anchor` and no `push` (−2). `scan.rs` 408 → 406: the row count folded
    into `wide`'s size in `Scan::new`, and `masks` indexing its 32-byte slice in the load (−1
    each).
  - *Why it is exact.* The same values in the same order: `from_fn` writes the words, then zeros,
    as the copy did; the chain yields the entries and then the same 8 padding entries; and
    `number_answers` numbers the same `always` entries in the same order.
  - *Measured.* `Scan::run`, `look_up`, `end_doc` (with `trigger` inlined), `end_super`, `masks`,
    `long_run` and `Scan::new`, with addresses normalised, are step 86's instructions by objdump:
    the cuts touch only the setup and code outside the hot loops. Replica, plain builds, against
    step 86's: seed 1 −0.149 and +0.026 ms (21 and 14 of 31), holdout 99 −0.126 and +0.024 ms (18
    and 14), a mean of −0.056 ms: the same speed. The final gate passed (seed-1 output
    f86973c21022, as before); memory, max of 20 runs, 32.04 MB; S 1194 → 1177.
  - *Board.* run-3259e1688e68, 0.082291 s (79.54 points), with Track A slow too
    (run-6eadbf0d60da, 0.017918 s); run-b46032503ace, 0.071064 s (32.07 MB, 81.13 points), 1.5%
    over step 86's fastest run: inside the spread, as the replica's equal time says. At equal T
    the 17 lines are worth +0.05 points.
- *Step 88 (bl-2).* Step 87 with 8 more lines cut in `plan.rs` and `scan.rs`, and the hot code
  kept where it was; `main.rs`, `mmap.rs` and `pool.rs` are step 87's.
  - *Cuts.* `classify` takes a whole part of the queries and returns its lines' flags, so
    `Plan::with` calls it straight from `par` instead of building a 9-line closure, and `type Part`
    names the (spans, terms) pair that `classify` and `records` take (−3); the lines before part p
    as a closure, `first`, and the records call as a one-line closure (−2); in `trigger`, `n` and
    `nh` declared in the `let`s of the pointers they go with (−2), and in `Answers::add`, `log` and
    `count` in one `let` (−1). Against step 86: `plan.rs` 248 → 235, `scan.rs` 408 → 403.
  - *Layout.* The linker places functions in the order of their mangled names. The scan's hot
    functions come right after `main` and `Plan::with` (and after every `par` job and other
    generic instance), and `masks` after the `Vocab` methods and `pool::worker`, so a size change
    in `main`, `Plan::with` or a `par` job moves the hot code, in 16-byte steps. The same hot code,
    only moved (means of four A/Bs of 31 pairs against step 86, by `run`'s shift mod 64): not
    shifted (steps 87 and 88) −0.056 and −0.068 ms; 16 bytes +0.172 and +0.126 ms (two builds,
    all 8 A/Bs positive); 32 bytes −0.074 ms; 48 bytes +0.096 ms (8 A/Bs, 6 positive). The cuts
    above shifted it by 16 bytes; the padding chain written `.copied().chain(pad)` instead of
    `.chain(&pad).copied()`, 0 lines, puts it back.
  - *Why it is exact.* `classify` marks the same kinds and returns the same flags, line by line,
    and both chains yield the same pairs.
  - *Measured.* `Scan::new`, `run`, `end_doc` (with `trigger`), `look_up`, `long_run`, `end_super`
    and `masks` are step 86's instructions by objdump, addresses normalised, and at the same
    addresses mod 64 (16, 32, 32, 16, 48 and 48, `long_run` aside). Replica, plain builds, against
    step 86's: seed 1 −0.083 and −0.145 ms (19 and 18 of 31), holdout 99 +0.015 and −0.060 ms (15
    and 18), a mean of −0.068 ms: the same speed. The final gate passed; memory, max of 20 runs,
    32.11 MB; S 1177 → 1169.
  - *Board.* run-8bac7298d692, 0.088981 s, and run-86d61fafbc8a, 0.088847 s, both with Track A
    slow too (run-72ac01d67671, 0.019251 s; run-619d42467af5, 0.017649 s); run-a28060420b19,
    0.067846 s (32.12 MB, 81.66 points), the best Track B run until step 89. That is −1.0% against
    step 85's best, inside the spread, with step 86's machine code: of its +0.17 points over step
    85's run, S 1192 → 1169 gives +0.06, and the rest is the board's draw.
  - *Not kept* (experiments table): the cuts with the hot code 16 bytes off, either `Plan::with`
    cut alone, `masks` moved onto a 64-byte boundary, and two cuts that rustfmt undoes; and, on
    this step's code, the scan started before the plan is published, measured again (a ceiling of
    0.6 to 0.75 ms).
- *Step 89 (bnm-1).* Step 88 with std's start-up skipped, as in Track A's step 73; `main.rs` only.
  - *Bottleneck.* Before `main`, std's start-up finds the main thread's stack guard by reading
    /proc/self/maps, and installs SIGSEGV and SIGBUS handlers with an alternate signal stack,
    which each of the 11 spawned threads then maps too. Nothing in B recurses (every module
    checked), so none of it is needed. strace -f -c (tiny seed 201, 8 CPUs): 352 → 286 system
    calls, with 16 `sigaltstack`, 8 `mmap`, 8 `mprotect`, 8 `gettid`, 8 `sched_getaffinity`, 5
    `rt_sigaction`, 2 `munmap`, one `prlimit64` and one `poll` fewer, and no open, read and close
    of /proc/self/maps; at 12 CPUs about 90 fewer, most of them taking `mmap_lock` for writing.
  - *Design.* `#![no_main]`, with Track A's comment: the C runtime calls our `main` directly, as
    `#[unsafe(no_mangle)] extern "C" fn main(_: i32, _: *const *const u8) -> i32`, with a SAFETY
    line. Its one exit is still `std::process::exit(0)`, and `std::env::args()` still works
    (glibc's `.init_array`). Panics still print their message, then abort (`panic = "abort"`).
  - *Layout.* Without std's start-up the binary has 124 KB less text (`pthread_getattr_np` pulls in
    `sscanf` and `strtod`), and `main` moves from before `Plan::with` to after `masks`. With the
    data paths joined by `format!`, the scan's hot functions, and `main`, sat 32 bytes off step
    88's offsets mod 64; joined by `[&data_dir, "/", name].concat()`, 0 lines, `Plan::with`, the
    scan, `worker`, `masks` and `main` are back at step 88's offsets, and the seven hot functions'
    normalised disassembly hashes as step 88's. Still off: the `par` jobs and the sorts (48),
    `push_decimal` (16) and the code after `main` (16). `concat` against `format!`: seed 1 +0.018,
    holdout 99 −0.108, fresh seeds 101 −0.100 and 103 −0.148 ms, a mean of −0.085 ms.
  - *Why it is exact.* Nothing B runs uses what the start-up sets up, and the arguments arrive as
    before. The output, the gate's three malformed query files and a missing data directory give
    step 88's results, exit codes and messages; the one visible difference is that a panic on the
    main thread names it `<unnamed>`, not `main`, as in Track A.
  - *Measured.* Replica, plain builds, against step 88's gate build. On tiny seed 201, where little
    but the start-up is timed: −0.118 and −0.125 ms (98 and 96 of 101 pairs). On full inputs, the
    four A/Bs we take for a change, seed 1 −0.211 and −0.214 ms (20 and 24 of 31) and holdout 99
    −0.080 and +0.058 ms (17 and 14): a mean of −0.112 ms, under our bar of −0.15 ms. Four more of
    each: seed 1 −0.110, −0.093, −0.310 and −0.284 ms, holdout 99 −0.008, +0.059, +0.069 and +0.023
    ms; all twelve, −0.092 ms. Over 30 fresh full seeds (101 to 130, 31 or 21 pairs each): a mean
    of −0.118 ms and a median of −0.121; without the nine seeds where the two differ by 0.5 ms or
    more (below), −0.089 and −0.115. By the formula: T −0.18%, +0.019 points; S 1169 → 1171,
    −0.006; about +0.013 net. It went in on the tiny input's start-up saving and on the 30 seeds'
    mean and median. The final gate passed; by hand, a missing data directory stops with exit 134
    and the OS error, as in step 88; memory, max of 20 runs, 31.71 MB (step 88's gate: 32.11);
    S 1169 → 1171.
  - *A slow mode per seed.* On some seeds one of the two binaries runs about 0.6 to 1.0 ms (1 to
    1.5%) slower in every run: step 89 won 0 to 2 of 31 pairs on seed 104 (+0.77, +0.86 and +0.92
    ms) and 21 of 21 on seed 122 (−0.90 ms). Of the 30 fresh seeds, step 88 is the slow one on six
    (106, 109, 110, 116, 122 and 130) and step 89 on three (104, 115 and 129). The `format!` build
    flips seeds 104 and 106 the same way (+0.88 and −0.80 ms), while step 86 against step 88, a
    change of layout only, does not (−0.04 and −0.09 ms). perf stat on seed 110 (step 88 slow) and
    seed 104 (step 89 slow): the same instructions (3.310e9 and 3.278e9), branch misses (8.6e6 and
    8.5e6) and page faults, dTLB walks within 5%; only the cycles are higher, by 1.1 to 2.4%. It is
    not thread placement: each thread does 8.3% of the cycles in both binaries. Our explanation,
    not proven: without the 12 alternate signal stacks, every later mapping moves relative to the
    others. So seed 1 and holdout 99 alone can mislead by up to 1 ms on any change to the start-up
    or the sequence of mappings, and such a change needs many seeds.
  - *Board.* run-f7d43533ffb5, 0.103705 s (77.05 points), with Track A slow too
    (run-ab14a5bbbaa8, 0.020882 s); run-0fc9764834f2, 0.067543 s (31.92 MB, 81.70 points),
    −0.45% against step 88's best, below what one board run resolves; run-d7f91eb8db60,
    0.135523 s (74.14 points), with B alone slow: in that dispatch Track A set its best time
    (run-53c59d1f073c, 0.015463 s) and Track C ran normally (run-a8ffa9cee029, 0.017592 s);
    run-e4fc11287808, 0.068272 s (32.03 MB, 81.59 points); run-a4259bf6753c, 0.067464 s (31.78 MB,
    81.71 points); and run-a83c16179001, 0.067066 s (31.82 MB, 81.78 points), the best Track B run
    until step 90.
  - *Not kept* (experiments table): the paths still joined by `format!`, and eleven other forms of
    `main.rs` near the paths, none of which put the hot code back.

**Step 75 (bs-5j2, 41e6cfe), S 1142**: step 70 with one step of comments
(71), three code steps (72, 73 and 75) and one of structure (74), the last four each measured on
the replica (ab.sh, 31 pairs, two PGO builds a side).
- *Step 71 (b-stack-20).* Review comments only; the token stream is step 70's. Board 0.078623 s
  (run-2a3e7f333147, 80.26 points).
- *Step 72 (b-stack-21).* The eight padding entries go in through the plan's grouping, as one more
  part no trigger term reads, instead of an `extend` that reallocated and copied the 1 MB entries
  array on the main thread (0.76 → 0.20 ms); and the assert for a BOOL query of over 6 terms moved
  into `cover`, the first function every BOOL line reaches (a 7-term OR used to overrun `cover`'s
  array first). −0.610 and −0.754 ms. S 1099 → 1101.
- *Step 73 (b-stack-22).* `look_up` writes every token's position and keeps the anchors' without a
  branch, as it already did for the trigger terms; the anchor prefetch, which measured no gain, is
  gone. −0.455 and −0.118 ms; branch misses −11.8%.
- *Step 74 (bj-2).* The code split into modules: `pool.rs` (`par`, the workers, `wait`, `ahead`),
  `vocab.rs` (the query split and the perfect hash), `plan.rs` (the plan's entries, `classify`,
  `cover`, `csr`), `scan.rs` (the scan and the answer states), and `main.rs` (`main`, the slices,
  the results and their formatting). Items moved with their text and comments; a sorted line-by-line
  check against the single file finds only the module docs and the `mod`/`use` lines added. With it,
  six review items in the setup: the plan's records built by a function returning a named struct
  (`records`, `Records`, instead of a 3-tuple read back as `.0`/`.1`/`.2`); the hash's bucket
  placement as `place_regions`, whose writes are bounds-checked stores into the thread's own
  regions; a compile-time check of `DocEntry`'s layout (32 bytes, the truth table at word 5), the
  one `trigger`'s gathers rely on; the check for at most 2^16 queries once, after `Plan::new`; the
  slice check on the slice's own ids; and `place` in three plain statements. The scan's functions
  (`Scan::run`, `look_up`, `end_doc`, `end_super`, `long_run`, `Scan::new`, `masks`) are identical
  by objdump, and every function's size is the same. Replica −0.071 ms (16 of 31) and +0.121 ms
  (11 of 31), and +0.179 ms on holdout 99 (7 of 31), against an A/A of +0.033 ms (15 of 31): with
  the scan's code identical, all are within the ±0.3% by which PGO builds of one source differ, so
  equal. S 1101 → 1147. Board: run-9c2169af7f30, 0.078856 s, 32.26 MB, 80.09 points.
- *Step 75 (bs-5j2).* `parse_queries` copied every thread's lines and terms into one array on the
  main thread. Now each thread's split stays as it is, one part of the queries, and `Plan::new` and
  `records` take one part per thread. Replica −0.520 and −0.245 ms (27 and 21 of 31), −0.689 ms on
  holdout 99 (29 of 31); memory, max of 10 runs, 31.32 MB; the gate passed. S 1147 → 1142. Board:
  run-54d2972c3d09, 0.078867 s, 31.53 MB, 80.10 points.

**Step 70 (b-stack-19, 883c315), S 1099**: step 69 with a larger sample for
the plan, 2.5 MB instead of 1 MB, whose documents' terms are deduplicated by stamps instead of a
sort. Replica, two PGO builds a side: −0.813 and −0.641 ms (27 and 29 of 31) against step 69; on
seed 1 −0.794 and −1.001, on holdout 99 −1.012 and −0.989, on fresh seed 109 −1.036 and −1.078 ms.
Memory, max of 10 runs, 31.69 MB. Board: run-aecc0a5f3a09, 0.078573 s, 32.06 MB, 80.27 points, the
best Track B run until step 77, and 0.093372 s with B alone slow (A and C normal). S unchanged.

**Steps 67 to 69 (b-stack-16 to 18), S 1087 → 1099**: three setup and tail steps, each measured on
the replica against the one before (ab.sh, 31 pairs, two PGO builds a side).
- *Step 67 (b-stack-16).* Less setup work per term: the truth tables built in one pass, the cover
  cost kept as it goes, no key copies when the query lines are split, a counting sort in
  `dedup_keys`; and malloc's mmap threshold fixed at 1 MB (at 32 MB the scan's first-id logs came
  from cleared heap and the peak reached 40 MB). Mean −1.37 ms over four A/Bs.
- *Step 68 (b-stack-17).* The tail's page releases moved into one parallel round after the
  formatting, where the threads wait anyway; and the `always` entries sorted by length, operators
  and their first term's row, so consecutive entries read neighbouring rows. −0.793 and −1.037 ms.
- *Step 69 (b-stack-18).* The plan's intermediate entries at half size: an entry keeps its cover
  term in its eighth word, so the serial grouping in `Plan::new` reads half the bytes. −0.528 and
  −0.634 ms. Memory, max of 10 runs, 32.73 MB.
- *Board.* Step 69 ran 0.081637 and 0.07961 s (run-65df9250e334, 32.72 MB, 80.12 points); step 68
  0.079675 s (run-fc0b5c2ebe3e, 80.12 points),
  and 0.103242 and 0.100641 s in two DISTURBED dispatches; step 67 0.101006 s in a slow one. S 1087
  → 1099 costs 0.04 points; about −2.9 ms on the replica is worth about 0.4.

**Step 66 (b-stack-15, fabe12e), S 1087**: step 65 with the triggered BOOL
entries evaluated 8 at a time. Replica −2.730 ms (83.705 → 80.989 ms, 29 of 31) and −2.298 ms
(83.492 → 81.127 ms, 30 of 31) against two PGO builds of step 65; memory, max of 10 runs, 33.55 MB,
under the 44 MB credit. Board: run-b16bec586dd7, 0.082048 s, 32.43 MB, 79.83 points, the best Track
B run until step 68; run-3e3ce97f718c, 0.094116 s, unexplained (step 66's row); run-2a6f1b53f20b, 0.082597 s; and run-4f16fee79168, 0.089935 s, in a control push of step 66's code.
Against step 65's best run the time is worth +0.44 points and the 25 lines cost 0.08.

**Step 65 (b-stack-14, 97b4897), S 1062**: step 64 with the terms' info words built in parallel.
Replica −0.334 ms (20 of 31) against step 64, and in eight more A/Bs (two builds a side, seed 1 and
holdout 99) all ≤ 0, mean −0.25 ms; memory, max of 10 runs, 31.64 MB (9 of the 10 at most 30.29).
Board: run-a0a52b4578ea, 0.085425 s, 30.28 MB, 79.47 points, the best Track B run until step 66;
run-e3147d5de2df, 0.086155 s; and run-ee50cf65988f, 0.093611 s, in a dispatch where Track C ran slow
too.

**Step 64 (b-stack-13, 50f07bb), S 1059**: step 62 with step 63's slicing of
skewed corpora and step 64's mapping of the corpus by waiting workers. Replica: step 63 −0.11% (17
of 31), step 64 −0.787 ms (29 of 31) on seed 1 and −1.361 ms (31 of 31) on holdout 99; memory, max
of 10 runs, 30.31 MB. Board: run-d84c9fde9fad, 0.085659 s, 30.43 MB, 79.45 points, the fastest
Track B time until step 65. S 1033 → 1059 costs 0.08 points; the replica's −1.0% on seed 1 is worth 0.11.

**Step 62 (b-stack-11, 5fda0c0), S 1033**: step 61 with the trigger's
lookahead prefetch reading only non-empty ranges (the old read was safe too: the work list is
non-empty only when a trigger term exists). Replica +0.07% against step 61 (15 of 31): the same
speed. Its gate added 17 hand-made edge inputs (only phrases, only `always` queries, one BOOL
query, empty documents at the start, middle and end and every 256th, all documents empty, one
document) at 1, 3 and 12 CPUs, all equal to the reference. Board: run-e734232a0ec5, 0.087799 s,
30.49 MB, 79.26 points; run-72fc88641e4b, 0.088963 s; run-43ec6fb133ea, 0.090207 s; and 0.132044 s in a dispatch where C ran
slow too.

**Step 61 (b-stack-10, 33ecba3), S 1033**: step 59's engine with step 60's lines pass and step
61's review fixes, at the same speed on the replica (−0.25% and +0.15%);
memory, max of 5 runs, 30.29 MB. Board: run-6f92692cfe79, 0.088016 s, 30.13 MB, 79.24 points; run-e39902c82c5d,
0.092338 s; and 0.144833 s in a dispatch where A and C ran slow too (0.018667 and 0.020717 s). S 1042 →
1033 is worth 0.03 points.

**Step 59 (b-stack-8, 10ef5ba), S 1042**: step 58 with one masked compare
per phrase, the row index by `min`, and the tail's lines pass. Replica −1.56% (29 of 31 pairs)
against step 58; memory, max of 5 runs, 30.28 MB. Board: run-31588c94b3ee, 0.08596 s, 30.11 MB,
79.46 points, and run-ee87cc98e73b, 0.087145 s. Steps 56 to 59 end at step 55's S (1042), and the
replica's −2.8% of T over them is worth about 0.31 points.

**Step 58 (b-stack-7, 88af8ca), S 1073**: step 55 with step 56's
block-major rows and the review's fixes, step 57's lighter regrouping, and step 58's hash build
and 32-bit log entries.
- *Replica.* Each step against the one before: step 56 −0.57% (24 of 31 pairs), step 57 −0.70%
  (28 of 31), step 58 +0.07% (14 of 31; the hash build alone +0.242 ms, 10 of 31, on the same code
  path at seed 0; the log entries and the dropped prefetch −0.270 ms, 19 of 31). Memory, max of 5
  runs: 40.44 MB (step 56), 36.91 (step 57), 31.06 (step 58). All are under the 44 MB of full
  memory credit, so M does not change B's points.
- *Board.* run-9c22107047b4, 0.088068 s, 30.97 MB, 79.11 points, the best Track B run until step
  59; run-f9ce4bea2564, 0.08862 s; run-65e7bf2f2bb3, 0.08865 s; and 0.10775 s in a DISTURBED dispatch, where C's unchanged code ran 25% over its previous run. Step
  57 ran 0.090239, 0.088985 and 0.088606 s (run-e52b3723fb8c, 79.06 points), and step 56 once,
  0.09138 s.

**Step 55 (b-stack-4, ffef5f8), S 1042**: step 53 with step 54's plan tables and one persistent
pool of threads for every parallel phase.
- *Replica.* Step 54 was 0.897 ms (0.99%) faster than step 53, winning 25 of 31 pairs, and step 55
  2.003 ms (2.23%) faster than step 54, 30 of 31: about 2.9 ms in all. The pool alone measured
  2.164 ms faster than step 53 (31 of 31). Memory, max of 5 runs: 39.67 MB.
- *Board.* Steps 54 and 55's first five runs took 0.093673 to 0.105612 s, two of them in
  DISTURBED dispatches, slower than step 53's three runs (0.091413 to 0.092929 s). Later, step 55
  ran 0.09033 s (run-54758cd41117, 78.93 points) and 0.089795 s (run-5a75106fd40c, 78.99 points),
  and step 54 once more 0.114435 s, in a dispatch where A and C ran normally. Board runs of
  one binary have spread by up to 16% today (step 49: 0.097889 to 0.113384 s over 15 runs), so a
  few runs cannot settle a 1 to 3% difference; the replica's paired runs are the measurement the
  step rests on.

**Step 53 (b-stack-2, 813ac5d)** is step 52 with ALWAYS at 0.30.
- *Board runs.* 0.092929, 0.092909 and 0.091413 s. The last, run-2c7a4b98be65 (39.44 MB, 78.92
  points), was then the best Track B run.
- *ALWAYS.* The replica cannot tell 0.30 from 0.25 (−0.33%, within the noise).

**Step 52 (b-stack-1, 5290d47)** is the engine.
- *Board runs.* run-0f3b8e8a39f9 0.103801 s, run-394a80cc8014 0.093419 s, and run-b18a35f4b44f
  0.092845 s (39.49 MB, 78.75 points). The first came in a dispatch where Track C was slow too
  (0.020984 s against a median of 0.018846 s).
- *Probe.* An in-process A/B probe of step 52 against step 49 on the box (19a626a,
  run-b16574a693b9) exited 194: step 52 8.7 ms faster, after the probe's +0.7 ms bias. The same
  probe on the replica read about 7.5 ms before the bias.
- *Replica.* Its parts, each against the one before (table below), and the whole against step 49:
  −6.862 ms, 31 of 31 pairs. A re-run at 14:16 UTC gave the same, 31 of 31.

**Step 47** was the submitted code until step 49. In the A/B of 09:28-09:47Z (pushes in turn,
runs.log 09:47Z), the three candidates ran: the quality pass alone (step 46b) 0.112084 s, step 47
0.115198 s, step 45 0.110054 s; then step 47 again 0.108491 s. All three versions sit inside one
0.108-0.115 s band (step 45's four runs span 0.109028 to 0.115166 s), so the box does not
separate them. The S cost is real: at the board's S exponent, step 47's 905 lines score 0.17
points below step 45's 858 at equal T.

**Step 49's binary** ran 15 times from 11:11 to 14:08 UTC (pushes that restored it after probes
included): 0.097889 to 0.113384 s, median 0.101022 s, M 36.06 to 37.96 MB.

Later runs of step 37's binary (with the flag): run-d97dcca08b7d, 0.12489 s, 40.61 MB, 76.20 (5d6d235, a dispatch our log
calls "a bit slow"); run-7bbf842f335a, 0.121849 s, 41.73 MB, 76.47 (51d3674, "box slower than
morning"); run-8addd6917ec9, 0.121414 s, 42.24 MB, 76.51 (3e7d9a4, SLOW BOX); run-749fda72c1d3,
0.118445 s, 41.5 MB, 76.78 (1f8d96a, normal box). S is the harness's `static_axes.py` count, which matches the board for
every run where the board's S was logged. M is blank where it was not logged. Points marked * are
computed from T and S with the fitted formula: every M logged since step 6 is under the 44 MB cap
(26.8 to 43.8 MB) except step 26's 44.09 MB, which costs 0.01 points, and the reverted step 46's
45.52 MB, whose logged points are used as they are; the formula reproduces every
logged point value to within 0.01. M is blank for steps 29 and 35 to 37 because runs.log did not
record it, and for steps 10 and 19 because the values once written there are not in runs.log;
steps 33 to 47 are at 40.6 to 42.7 MB wherever it was logged, step 46 aside; steps 49 to 52 are at 36.06 to 43.07 MB.
Steps 24 and 27 are the round's reverted experiments; step 23 was
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
- **Steps 15–16: setup work transferred.** Steps 15 and 16 together took the box from step 10's
  0.1713 s (step 13's one run, 0.1792 s, came in a disturbed dispatch) to 0.1619 and 0.1601: 9 to
  11 ms of the 26 ms of serial setup that step 16 spread over twelve threads.
- **Step 17: instruction cuts that also cut cycles transferred.** The token-path changes cut 6.8%
  of the instructions on the test server and the box went 0.1619 → 0.1484 s (−8%).
- **Steps 18–19 are within the box's noise.** On the test server, step 18 took the instruction cut
  against step 16 from 6.8% to 11%, and step 19 cut 4.1% and then 4.8% more, but the box gave 0.1529
  and 0.1643 s for step 18, and 0.1518 and 0.1478 s for step 19, against step 17's 0.1484. Step 18
  alone read 3% and 11% slower; step 19 brought the pair level with step 17, so both were kept as
  not slower, not as a gain. The design
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
  locally but failed on the board's builder, so it was reverted at once; step 29's atomic profile
  counters then made the profiles repeatable.
- **Step 28: stall cuts transferred again.** The two-pass block end hides the remaining random loads
  of an evaluation (words and answer state) behind the evaluations before it. The box gave 0.1374 and
  0.1381 s against step 26's 0.1389 and 0.1420, and M fell back to 39 MB, for 31 more lines. What the
  box rewarded in this whole round was more parallel setup (steps 15–16) and fewer stalls on random
  loads (steps 20, 28), not cuts of instructions (steps 18–19) or of L2 misses (steps 23–24).
- **Step 29: atomic profile counters.** With non-atomic counters the twelve-thread training run lost
  updates, so every build had a different profile. LLVM's `-instrprof-atomic-counter-update-all`
  fixed that without step 27's CPU pinning, and the board's builder accepted it.
- **Step 31: the lookup was still a stall, so one probe beat four compares.** The bucket table
  compared a token with four keys of a 64-byte bucket, and a full bucket continued into the next. A
  perfect hash gives every term one 16-byte slot, so the probe is one load, one XOR, one `vptest`
  and one `cmov`, and a token no query uses is turned away by the same single probe. The hash costs
  three instructions (two `aesenc` and a move) where the CRC cost one, and it still won: test
  server −8.4%, box 0.1338 → 0.1199 s (−10.4%), the largest single gain since step 20. The build
  runs on twelve threads because pilots of 1 mod 64 give each hash group its own region of the
  table, so setup stays parallel (steps 15–16).
- **Step 32: a speed check of a line cut has to look at the code, not only the clock.** The cut
  changes only setup, parsing and formatting; the scan, block end, superblock and answer functions
  are instruction for instruction the same as step 31's, in both the PGO and the plain build. The
  clock alone could not show that: two PGO builds of one source (a partial cut) ran 84.9 and 88.3 ms
  on the test server, with the same hot instructions at the same addresses and the same instruction
  count (5.239G), only 1.6% more cycles; they differed in where cold code, data and thread-local
  storage landed. A plain build of the final cut ran 3.4% slower than a plain build of step 31, also
  with identical hot instructions, while its PGO builds tied. The cut was taken on the identical hot
  code and the PGO tie, and the board run is the check.
- **Timing probes on the box, again** (steps 31 and 32's code). Two builds of step 31 stopped at the
  end of the scan, one with the BOOL evaluation turned off: run-dec0ff56b77b exited with 118 (ms) and
  run-9ce29e8f1465 with 79, so the BOOL evaluation costs about 39 ms on the box, twice its 19 ms on
  the test server, and the token path is the larger part. A build of step 32 stopped at the end of
  setup: run-dbda057bda65 exited with 165 (units of 0.1 ms), so setup took 16.5 ms, the same as the
  test server's 16.4 to 16.8 ms. That made the test server's setup numbers usable for step 33.
- **Step 33: what parallel setup waits on is the kernel.** With setup already on twelve threads, a
  test-server profile of a build that stops after setup showed the time going to kernel spin locks and
  page faults, and even after step 33 the setup spends more CPU time in the kernel than in the program
  (34.6 of 58 ms): page faults of freshly allocated, growing vectors, and the mmap lock that
  their reallocations and the thread spawns take. Smaller records (8 and 16 bytes instead of 48 and
  32) that never grow, a counting sort instead of a comparison sort for the grouping and the term
  order, and no merge copies cut setup from 16.8 to 11.6 ms. One cut was a plain bug: every term's id
  was read through the map of long terms first, which hashes all of a term's bytes with SipHash,
  though only terms over 12 bytes are in it. A thread pool and threads spawned in a tree, aimed at
  the spawn stagger (the twelfth thread started about 1 ms after the first), gave nothing.
  `malloc_trim(0)` is needed because of the pre-sized vectors: once glibc has seen large blocks
  freed it keeps freed memory resident, and the peak rose from 39.4 to 44.6 MB until the trim
  brought it back to 41.5 MB (12-CPU container on the test server).
- **Step 34: a smaller sample plans as well.** 1 MB (about 1,030 documents) gives the same plan
  quality as 2 MB for 1.4 ms less setup on the test server; 512 KB plans worse (about +4 ms in
  total).
- **Steps 35 to 37: loop alignment and layout.** Without the flag, step 36's build ran 84.0 to 85.3 ms
  on the test server against step 34's 82.4 to 82.8, with the same instruction count (5.250G) and
  1.7% more task-clock, and every edited variant we bisected landed at 83.6 to 86 ms: step 34's build
  was a lucky layout. With `-align-loops=64` both builds time the same (medians of 3 × 21 interleaved
  runs: step 34 82.2, 83.1 and 82.3 ms; step 36 82.2, 82.2 and 82.8). On the box the picture is not
  settled: step 35 (flag on, code of step 34) ran 0.1206 s against 0.1174 without, and steps 36 and 37
  fell on slow-box dispatches (A, unchanged, 37 to 41% over its best); the clean rerun of step 37
  gave 0.1203 s. Four later runs of step 37's binary gave 0.118445 s in a normal dispatch and
  0.121414 to 0.12489 s in dispatches our log calls slower. Step 38 builds B without the flag
  again, as a board A/B test. Result: 0.116583 and 0.119467 s without the flag against 0.118445 to
  0.12489 s with it (five runs): not slower, within the build-to-build spread, so B builds without
  the flag.
- **Steps 36–37: lines from setup and the plan.** S 847 → 817 with no hot function touched: an
  operator lookup from the low bits of its first byte (`A`, `N`, `O` end in binary 01, 10, 11), split
  types rustfmt keeps on one line, a shorter grouping and key-length expression, a u32 counting-sort
  key and the parts' results merged with `unzip` instead of a loop. The harness counts rustfmt's
  output, so only real simplifications count; re-wrapping lines does not.
- **Step 39: a token-path cut transferred again.** 6.2% fewer instructions and −2% on the test
  server gave 0.113311 s on the box against step 38's 0.116583 and 0.119467, like step 17's.
- **Steps 40 and 44: ALWAYS is a box decision.** After step 39 a sweep off the board measured 0.125
  fastest of 0.0625 to 0.25 (b9237f3's comment); with 0.125 the box ran 0.11394 to 0.12433 s (steps
  40 to 43), with 0.25 0.113311 and 0.108588 (steps 39 and 44), so 0.25 stayed on the block engine. A board sweep since (0.35: run-991a3ad938c2, 0.112231 s in a slow
  dispatch; 0.5: run-5fd89506515e, 0.117245 s; 0.30 on step 47's code: run-0c4bcc0a40b7, 0.113925 s) did not beat it. The
  test server's own sweep had picked 0.125: a constant tuned there was wrong for the box.
- **Steps 45 and 46: the box pays for random state, not for instructions.** Step 46's block end did
  less work (a third fewer evaluations, test server −1.9%) but kept a bitmap, a pair per entry and
  two more per-thread lists, and the box ran 0.134 s against 0.109 to 0.115 and passed the memory
  credit. Step 45 adds no state: it fetches the formatter's existing random reads ahead.
- **Steps 41 to 43: one slow sample is not a result.** The first run of the sort-free first ids,
  0.12433 s, looked like a loss; the A/B back to the sort gave 0.114997 and the same code again
  0.112506, so the scatter was kept. A disturbed dispatch can cost 10% on one run (the note above
  the table).
- **A measurement bug**: some test-server A/B comparisons after step 11 compared a stale binary,
  because copied source files kept older modification times and cargo did not rebuild. It was found
  and fixed; every keep-or-revert decision above rests on board runs, not on those comparisons.
- **Step 48: a probe on the test server is not the box.** bspeed-9 was faster in an in-process
  probe on the test server (+0.74 to +1.40 ms), and its two board runs (0.113784, 0.13007 s) were
  slower than step 47's (0.108491 to 0.115198 s). The restores went back to step 47.
- **The block engine's cost on the box** (V-probes on step 47's design, exit = scan time in 0.5 ms
  units, five dispatches; "Box probes since step 47"): the token path alone 38.0 ms; with the
  presence words 54.0; with the block ends 72.5; with the superblocks 88.5; all of it 94.5. So
  presence words (16 ms) and block evaluation (18.5 ms) were the BOOL cost that the next design
  attacked.
- **Step 49: per document instead of per block.**
  - *What changed.* The block engine evaluated a triggered query on 64-bit words at each block's
    end, which needed a presence word per term per block, a list of the words touched, and a
    two-pass block end to hide their random loads. Per document, presence is one compare of the
    term's last document against this one. The query's whole left-to-right formula was folded, at
    planning time, into a 32-bit truth table over its other terms (at most 5), so an evaluation is
    5 loads, 5 compares and one shift.
  - *Test server.* −6.2 ms (−7.7%): the triggered evaluation 14.4 → 6.7 ms, the per-token stores
    7.1 → 8.4 ms, instructions −14.5%.
  - *Box.* 0.097889 to 0.103 s in its first five runs, against step 47's 0.108491 to 0.115198 s:
    about −9%, more than the test server showed. M fell to 36-38 MB, since the per-thread presence
    words, touched lists and work arrays (0.85 MB per thread) are gone.
- **Steps 50 and 51: memory spent on speed did not pay on the box.** The malloc threshold (bc-3)
  cut mmap-lock contention in setup on the test server (−0.8 ms), and its three board runs
  averaged 0.1109 s against step 49's 0.1005. The 512-document superblocks (bc-4, C1) cut 2.6% of
  the instructions; the box's in-process probe read them 6.5 ms slower, and the board run was
  0.107105 s at 43.07 MB, near the 44 MB cap (its rows take 648 KB per thread, twice the 256-document
  ones).
- **Step 52: the replica decides, part by part.** From Sun 13:20 UTC, each part of step 52 was
  measured on the replica against the stack before it (table below). Two parts carried most of
  the gain:
  - *The quality port (q2n): −2.1 ms.* It brought the masked 32-byte phrase compare, the
    branch-free trigger dedupe and the probe's info word as a plain load (no 2-uop `vpextrd` per
    token).
  - *Byte stamps without `malloc_trim` (q6t): −3.3 ms.* The stamp of a term's last document is
    its id mod 127, so `lastdoc` is one byte per term (75 KB per thread instead of 303 KB), reset
    every 127 documents. `malloc_trim` had returned the setup's freed memory to the kernel before
    the scan. The two were measured together.

  Phrase records (q10) gave −0.43 ms. The rest was noise, kept for its simpler code or dropped.
  The board then ran step 52 at 0.093419 and 0.092845 s, about 8% under step 49's median.
- **Step 53: a constant the replica cannot see.** ALWAYS 0.30 against 0.25: the box's in-process
  probe had read 2.3 ms in 0.30's favour (one probe, on q6n), the replica −0.3% (19 of 31 pairs).
  It was kept as no worse; the code comment says both numbers.
- **Step 54: setup without zero fills.** `csr` now counts each key, turns the counts into run ends,
  and writes each value into its run from the end, last value first, into a vector's spare
  capacity: no zero-filled values vector and no clone of the starts. `line_of` gets its capacity
  up front, and the `always` entries sort on one packed `u32` key (the length, then the six
  operators) instead of a tuple of arrays. Replica −0.897 ms (−0.99%, 25 of 31 pairs).
- **Step 55: no thread is spawned twice.** Setup runs seven parallel phases, and each used to spawn
  and join 12 threads. Now the first call to `par` spawns the workers, which stay for the run.
  - *Waiting.* Between calls a worker waits in C0.2 with `umonitor`/`umwait` (WAITPKG, checked by
    CPUID, with a polling fallback). It leaves its core to the SMT sibling, keeps its heap arena,
    and its CPU stays out of the deep idle states, whose exit latencies the replica lists as 127
    and 1,048 µs (C2, C3).
  - *Phases* (replica, medians of 9): the end of setup moved from 12.39 to 11.10 ms and the scan
    threads' start from 12.44-12.56 to 11.10 ms; the post-scan tail shrank too (results joined
    86.30 → 84.84 ms, written 88.30 → 86.61).
  - *Variants.* Spinning workers (`pause`) gained only 1.19 ms in the phase probe, and spawning all
    workers before publishing the first job 1.37 ms in ab.sh, both less than `umwait`.
  - *Plan.* `Plan::new`'s second thread became a `par(2)` round: a `std::thread` next to the pool
    shared a CPU with a worker, and in the first pool build the entries' `csr` took 2.2 ms instead of
    0.77.
  - *Cost.* +33 lines (S 1009 → 1042). Replica −2.003 ms (−2.23%, 30 of 31 pairs) against step 54.
- **Step 56: rows by block.** The superblock's rows held a term's four block words
  together, 32 bytes per term, so the word every token updates was every fourth word of a
  rows × 32 B array. Step 56 keeps the four blocks' words in four arrays (word r of block b at
  `b * (rows + 1) + r`), so a block's updates fall in one contiguous rows × 8 B array, and
  `end_super` assembles a term's 256 bits from its four words. Replica −0.782 ms (−0.89%, 26 of
  31 pairs) against step 55; folding the four words with scalar operations instead, one vector per
  entry, was +1.640 ms (1 of 31). With the review's fixes and rustfmt: −0.499 ms (−0.57%, 24 of
  31) against step 55, M max of 5 runs 40.44 MB, S 1065. On the replica it runs full seed 1 in
  86.93 ms and holdout seed 99 in 86.69 ms (step 49: 97.58 ms on holdout 99).
- **Step 57: regroup only what is printed.** After the scan, each thread regrouped its whole
  first-id log into line order. But an answer whose first slice alone has ten ids prints only
  those: on seed 1, 10,144 such answers hold about 100K of each later thread's 122K logged ids,
  and the output uses 121K ids from thread 0 and only 3.4K to 16K from each later thread. Now the
  first slice's thread publishes, per answer, whether it has ten (50 KB); the later threads wait
  for it (their scans end within about 1 ms of each other) and skip those answers. Each line's
  place comes from an exact-size map, and the regroup's position array is kept as each line's end,
  which the formatter reads in place of starts and per-thread cursors. Replica: −0.772 ms (−0.88%,
  28 of 31 pairs) against step 55; merged onto step 56, −0.606 ms (−0.70%, 28 of 31). Memory, max
  of 5 runs: 37.22 MB against step 55's 39.23 MB. S 1065 → 1067.
- **Step 58: a hash build that does not fail on a seed, and 4-byte log entries.**
  - *Why builds failed.* Every failed build was one bucket whose pilot search found nothing. With
    pilots 1 + 64j, a key's slot moves by a fixed step per j, so some keys reach only a few slots
    (on fresh seed 109, the key "6236" reaches slots 160 and 161 only). And the seed was XORed into
    round-key bytes 0 and 1 only, which the second AES round carries into hash bits 0 to 31 alone:
    bits 32 to 63, which pick the slots, were the same at every seed, so the same keys failed at
    every seed. A forced retry (seed 101) had cost 3.70 ms on the replica (89.14 → 93.15 ms), and
    over 300 hash seeds on each of 32 datasets 257 of the 9,600 builds failed (2.68%), on 20 of
    the 32 datasets, at worst 15% (fresh seed 129). Seed 0 never failed on them, but a dataset
    of the final scoring had about a 3% chance of needing one retry.
  - *The fix.* A thread whose bucket finds no pilot places its own regions again with that bucket
    first, up to four times; the other threads are not affected. Attempt a's seed is a ×
    0x9e3779b97f4a7c15, which changes all 8 bytes it is XORed into, so every hash bit depends on
    it. Seed 0 is unchanged: the same hashes, table and scan as before.
  - *Sweep.* 300 seeds on each of 24 datasets (18 fresh full-scale ones, full seed 1, holdout 99
    and samples 1 to 4): 7,200 builds, none needing a second seed. Step 54's build had failed in
    34 of 300 seeds on fresh seed 109, 18 on 110 and 13 on seed 1. A region was placed again in 3
    to 28 of the 300 seeds per dataset, about 0.4 ms on that thread; seed 0 needed none on any of
    the 24. (At 8 threads, an earlier build of the fix needed a second seed in 2 of 6,600 builds.)
    The robustness runs later repeated the sweep on 32 datasets: 9,600 of 9,600 builds succeeded.
    Forced start seeds ran the per-region retry end to end: 20 of 300 runs placed a region again,
    and all 300 outputs were exact.
  - *Log entries.* A logged first id is now `answer << 16 | id & 0xFFFF` in a `u32` (it was a
    `u64`), rebuilt as `first + ((entry - first) & 0xFFFF)` from its slice's first id. That is
    exact while a slice holds at most 2^16 documents, so the corpus is cut into at least 8
    slices (about 31,000 documents each at the spec's 250,000), and an assert stops an
    out-of-spec input. Memory, max of 5 runs: 31.04 against 37.08 MB.
  - *Prefetch.* The formatter no longer prefetches the 12 states of the line 8 ahead: after step
    57 a line's work is those 12 reads, a few ids and the digits, and its 600K prefetches did not
    pay (five ab.sh runs: −0.428 to +0.066 ms).
  - *Cost.* Replica +0.07% against step 57 (14 of 31). S 1067 → 1073.
- **Step 59: one compare per phrase.** A phrase of up to 8 terms was checked by one masked 32-byte
  compare of the document's token ids, but near the document's end, and for longer phrases, a loop
  took over. Now `look_up` writes 8 ids of `miss` (the id of every token no query uses) after each
  document's tokens, so the 8 ids from any phrase start exist and none equals a phrase term. The
  loop, `Plan::terms` and `Phrase::start` are gone, and a phrase of over 8 terms (the spec's have 2
  to 8) stops the program. Replica −1.049 ms against step 58 (30 of 31 pairs), S −12. The sink
  row's index became `id.min(rows)`, which LLVM now compiles to `cmovb` (checked in the PGO
  binary), so the inline-asm `min` is gone: both together −1.146 ms (26 of 31), S −11 more. With
  the tail's lines pass (S −8, the same speed): −1.349 ms (−1.56%, 29 of 31). S 1073 → 1042.
- **Step 60: lines at the same speed.** Tuple `let`s that rustfmt spread over five lines became
  two short `let`s each; the trigger fetches the state of the range `RANGES_AHEAD` on without an
  `if let` (entry 0's near the end); `end_super` no longer redefines its stride. Replica −0.215 ms
  (−0.25%, 20 of 31) against step 59; memory, max of 5 runs, 29.89 MB. S 1042 → 1030.
- **Step 61: review fixes.** `par` serves one job at a time: its job slot and counters hold one
  job, so an overlapping call would mix two jobs' counts and could return while a worker still runs
  its job. The program calls `par` from the main thread only; an assert (`BUSY`) now makes a misuse
  stop, and the SAFETY text of the job's lifetime covers the reference left in `JOB` after the
  call. The fallback wait for CPUs without WAITPKG spins with `spin_loop`, and its comment cites
  the phase probe of step 55 instead of an unsourced figure. A comment says why the perfect hash's
  barrier cannot deadlock on the pool: its `n` is the `n` of `par`'s first call, which sized the
  pool. Replica +0.130 ms (+0.15%, 12 of 31) against step 60: the same speed. S 1030 → 1033.
- **Step 62: a prefetch that reads only what exists.** Near the end of the work list the trigger's
  lookahead prefetch read entry 0; it now reads the current range's first entry, which always
  exists, since a range belongs to a trigger term and is never empty. The old read was safe too,
  and 17 hand-made edge inputs, now part of the gate, gave the reference's output with both
  versions and with a bounds-checked build. Replica +0.07% (15 of 31). S unchanged.
- **Step 63: skewed corpora.** Slices were cut by bytes, and the 4-byte first-id log keeps 16 bits
  of an id, so a slice must hold at most 2^16 documents. A valid corpus with many short lines in
  one place broke that: 250,000 documents with 80,000 empty ones first, or last, made step 62
  abort at 1, 3 and 12 CPUs, where the reference answers. Generated corpora, with even line
  lengths and at most about 31,000 documents a slice, never hit it. `slices` now cuts a range of
  more than 2^16 documents every 2^16 lines. The ranges' document counts come from the ids at the
  byte cuts and the last line's id, so a normal input does no extra work. Five skewed corpora
  (empty documents first, last and in the middle, no trailing LF, 300,000 tiny documents) give the
  reference's output at 1, 3 and 12 CPUs (step 62: 9 of the 15 runs aborted), and joined the gate.
  Replica −0.11% (17 of 31). S 1033 → 1048.
- **Step 64: the corpus mapped in by waiting workers.** Between setup jobs the pool's workers sat
  in `umwait`, while the scan later took the corpus's page faults itself. A waiting worker now
  reads one byte per 64 KB of the corpus, and the kernel maps the 16 pages around each read
  (fault-around); the reads are interleaved over the slices, so every slice gains alike.
  `madvise(MADV_POPULATE_READ)` had held `mmap_lock` and stalled the main thread's allocations
  (+7.7 ms); a page fault takes only its VMA's lock on kernels with per-VMA fault locks (Linux 6.4
  and later; the replica runs 6.12). Replica −0.787 ms (29 of 31) on seed 1 and −1.361 ms (31 of
  31) on holdout 99, and in a second set of A/Bs −1.338 and −1.114 ms on seed 1, −1.077 and
  −1.465 ms on holdout 99 (two builds a side). The box's kernel was not probed: its first board
  run, 0.085659 s, was the fastest Track B time until step 65. S 1048 → 1059.
- **Step 65: the info words in parallel.** `Plan::new` built every term's info word (its new id
  and the ANCHOR and TRIG flags, by old id) in one serial pass of 0.51 ms, with eleven threads
  waiting. It is now a parallel round over ranges of the old ids, concatenated: the same words in
  the same order, with the random reads of the group and anchor tables on twelve threads. Replica
  −0.334 ms (20 of 31) against step 64; the eight A/Bs of the change, two builds a side on seed 1
  and holdout 99, were all ≤ 0, mean −0.25 ms. S 1059 → 1062.
- **Step 66: eight entries per evaluation, and only the matches written.**
  - *Bottleneck.* On seed 1 the triggered evaluation is 5.51M entry evaluations in 3.76M ranges of
    1.46 entries, and 52% match. Step 65 ran them range by range: five scalar stamp loads per entry,
    a truth-table bit, and a branch-free `add_doc` that read and wrote the answer state of every
    entry, match or not, while the per-range loop mispredicted its exit.
  - *Design.* The first pass stores each fresh trigger term's entry indices, 8 per AVX2 store, into
    one list (the next store overwrites the spare lanes; a range of over 8 entries, 1,064 of 3.76M,
    is stored on) and prefetches the range's first entry. Each 8 indices then take 11 gathers: the
    entries' 5 term ids, those terms' 5 stamps, and the truth tables. The presence bits are shifted
    in term by term, `truth >> presence` is the answer, and the matching indices are left-packed:
    `pdep` spreads the 8-bit mask to bytes, `pext` picks their lane numbers, `vpermd` moves the
    indices. Only the matches then update their answers.
  - *Why it is exact.* A lane computes the scalar `truth >> present & 1` from the same tables and
    stamps, under the same DONE rule. A gathered stamp keeps only its low 7 bits, so the 3 bytes
    after it never count, and EMPTY reads as 127, which no stamp equals. Every index read is a real
    entry or the last of 8 padding entries, whose table is 0. Each answer matches at most once per
    document, and the matches are added in document order, so the first ids are logged as before.
  - *Measured.* Replica −2.730 ms (29 of 31) and −2.298 ms (30 of 31) against two builds of step
    65. An early version against step 64: instructions −4.1%, branch misses −13.7% (12.10M →
    10.45M), cycles −1.6%, L2-miss loads +5.5% (the state prefetch is gone). Entries of 32 bytes
    beat 28 (−0.775 ms, 29 of 31): word 8i + k is field k of entry i, and no entry straddles a
    cache line. Two timing-only builds (wrong output) price what is left: every update sent to one
    answer −3.13 ms, every entry read from entry 0 −3.13 ms. The variants that lost are in the
    experiments table. S 1062 → 1087.
- **Steps 67 to 69: setup and tail work cut where it waits.** After step 66, phase marks showed
  serial stretches in the setup and a tail that freed its tables on one core. Step 67 cut per-term setup work and
  fixed malloc's mmap threshold at 1 MB; step 68 moved the tail's page releases into a parallel
  round where the threads wait anyway, and sorted the `always` entries so that consecutive ones
  read neighbouring rows; step 69 halved the plan's intermediate entries. Replica: mean −1.37 ms,
  then −0.793 and −1.037 ms, then −0.528 and −0.634 ms. Not kept on the way: ALWAYS at 0.25, 0.35,
  0.60 and 0.80, one gather per `always` term, the sample at 2 MB, a phrase pre-filter, and a sort
  of the `always` entries by all their rows (experiments table). S 1087 → 1099.
- **Step 70: a better plan from a larger sample.** The plan's covers, and the choice between
  evaluating a query per document or per superblock, rest on term frequencies from a sample. A
  2.5 MB sample instead of 1 MB gives better estimates, and deduplicating each sampled document's
  terms by stamps, as the scan does, instead of sorting them, pays for the larger sample. Replica
  −0.813 and −0.641 ms, and about −1 ms on holdout 99 and fresh seed 109. S unchanged.
- **Steps 71 to 75: smaller steps, and the structure.** Step 71 changed comments only. Two costs
  were left: a 1 MB copy of the entries on the main thread's setup path (step 72) and a branch per
  token for the anchors in the scan (step 73); together about −1 ms. Step 74 then split the file
  into modules and took six review items, with the scan's machine code unchanged by objdump, and
  step 75 dropped the main thread's copy of the queries into one array (−0.25 to −0.69 ms).
  S 1099 → 1142.
- **Steps 76 and 77: a store that misses L1 holds up the stores behind it.** Step 76 changed
  comments only. On its token path, about 70 uops per token ran close to the core's rename limit,
  and the stamp store, which had to fetch its line 13M times, kept the store buffer full for about
  119M cycles. A `prefetchw` just before the store starts that fetch out of order (−1.7 ms);
  placed earlier, in the probe loop, it lost 1.4 ms. The other pieces cut uops: the trigger and
  anchor lists left-packed 8 at a time in the same pass as the presence stores, keys from the
  window's start, and the table's address no longer reloaded from the stack per token. Non-PGO
  −4.2 ms; with PGO −2.1 to −2.8 ms, after a statement order that changed only the register
  allocation had cost 0.3 to 0.7 ms. S 1142 → 1155.
- **Step 78: a layout choice can expire.** Step 56 laid the superblock rows out block by block
  for the per-token stores. Once those stores were made 8 at a time, the stamps' lines fetched
  ahead (step 77) and the rows computed by one `vpminud` per 8 tokens, the old layout of four
  adjacent words, read by `end_super` in one load, won back about 1.05 ms on seed 1 and 1.10 on
  holdout 99; on step 78's first piece alone it had gained about nothing. A 16-byte pending
  token, which lost 0.39 ms with its offset inserted by `vpinsrd` (step 77's experiments), gained
  0.26 to 0.70 ms on holdout 99 and fresh seed 105, and nothing on seed 1, with the offset as a
  second 4-byte store. S 1155 → 1164.
- **Step 79: a threshold moves with the cost it trades.** ALWAYS trades per-document evaluations
  against superblock ones; once step 78 made a superblock read one load per term, the best value
  moved from 0.30 to 0.40 (non-PGO sweep: 0.40 −0.75 ms; 0.25 and 0.60 slower). With PGO the gain
  is smaller, about −0.1 to −0.5 ms, in eight of nine A/Bs. The same step closed a silent wrong
  answer on input the spec does not allow: a last query line without LF lost its last byte, and
  now stops the program with a message, as an empty file does. S 1164 → 1165.
- **Step 80: a table where uops are the limit.** `pdep`, `imul`, `pext` and `vmovq` made the
  left-pack's lane numbers in 4 uops; one load from a 2 KB table, folded into the instruction that
  widens them, replaced them: −0.32 ms over eight A/Bs. The same table had lost in step 66's work
  only inside a bundle of changes, so it had never been measured alone. S 1165 → 1172.
- **Step 81: PGO can stop paying.** The profile came from a sample-scale training run. After steps
  77 to 80 tuned the scan by hand, the plain build of the same source ran 0.55 to 0.67 ms faster
  than the PGO build, and it is deterministic, so one build a side settles an A/B. The tail's
  releases spread over the threads left 10.9 MB instead of 16.3 MB for the exit to unmap. S 1172
  → 1174.
- **Step 82: memory kept is cheaper than memory returned.** A trim flushes the TLB of every CPU the
  threads ran on. With trims off, the heaps keep what each phase frees for the next, and `rank`
  takes over the sort's freed keys: −0.3 to −0.6 ms for 1.5 MB more peak, a gain step 77's PGO
  builds could not separate from their spread. S 1174 → 1178.
- **Steps 83 to 85: the trigger's cost was its instructions.** A match is a coin flip and only
  0.07 terms per entry are present, so a batch's work is fixed. Halving the entries and their
  group starts saved 1.3 to 1.6 ms; byte sums cut the superblock count from 39 uops to 27
  (−0.6 ms); and a gather costs about 0.36 ms per batch whatever its live lanes, so loads and a
  transpose replaced the entry gathers (−0.5 to −0.7 ms). S 1178 → 1192.
- **Step 86: a lock taken for writing puts the threads in a queue.** Twelve threads freeing 2 MB
  mappings at once waited on `mmap_lock`; `MADV_DONTNEED` takes it for reading, and the 0.53 ms
  at the end of the merge became 0.006 ms. S 1192 → 1194.
- **Steps 87 and 88: a line cut can move the hot code.** The linker places functions in name order,
  so a cut that only shrinks `main`, `Plan::with` or a `par` job shifts the scan behind it. The
  same machine code 16 bytes off ran 0.13 to 0.17 ms slower, and 48 bytes off about 0.1 ms; so each
  cut was checked for the hot functions' addresses mod 64, and a 0-line rewrite put them back. 25
  lines at the same speed, about +0.07 points of S. S 1194 → 1169.
- **Step 89: a fixed cost, under a per-seed effect larger than itself.** Skipping std's start-up
  saves 66 system calls on 8 CPUs and about 90 on 12, most of those writers of `mmap_lock`, and
  about 0.12 ms, plain on a tiny input. On full inputs, on 9 of 30 fresh seeds one of the two
  binaries ran 0.6 to 1.0 ms slower in every run, most likely from where the mappings land; that
  hides the gain on any single seed, so it took 30 seeds to measure (mean −0.118 ms).
  S 1169 → 1171.


## Box probes since step 47

The board reports only T, M and S of a correct run, and the exit code of a failed one, so a
temporary build reports a measurement as its exit code; the board lists it as a failed run. Each
was removed by the next B commit. A probe exits early only on inputs over 100 MB, so the
Dockerfile's PGO training run exits 0.

**V-probes** exit with the scan's time, in 0.5 ms units, after a given part of the work:

| Design | Level | Run ID | Exit | Scan time |
|---|---|---|---|---|
| Step 47 (blocks) | V0: token path only | run-4ee0ffc77192 | 76 | 38.0 ms |
| | V1: + presence words | run-8bdfde87d1ff | 108 | 54.0 ms |
| | V2: + block ends | run-acf311181ac5 | 145 | 72.5 ms |
| | V3: + superblocks | run-a51936969cf5 | 177 | 88.5 ms |
| | V4: everything | run-ff8ead1d9034 | 189 | 94.5 ms |
| Step 49 (per document) | V1: token path + per-token stores | run-db59ea91d864 | 99 | 49.5 ms |
| | V2: + per-document BOOL evaluation | run-5eb984df15f3 | 127 | 63.5 ms |
| | V3: + superblocks | run-8cdacb61c737 | 145 | 72.5 ms |
| | V4: everything, twice | run-488d242b09a9, run-7bfdc19c49a1 | 201, 173 | 100.5 and 86.5 ms |

Each level ran in its own dispatch, so the differences carry the board's noise between
dispatches. Step 49's V4 read 100.5 and then 86.5 ms for the same build in consecutive
dispatches.

**In-process A/B probes** build two programs as two modules of one binary. On the full input
they run a warm-up, then 4 blocks of old, new, new, old (whole programs), and exit with 100 +
the median of (old − new) in 0.1 ms units: above 100 means the new one is faster.
- *Calibration.* Two identical copies read 75 without alignment: the second copy 2.5 ms
  slower, only from where its code landed. With every function aligned to 4 KB
  (`-C llvm-args=-align-all-functions=12`) they read 107, a bias of +0.7 ms, which the later
  probes subtract.
- *Repeats.* The two probes run twice gave opposite answers, so one board probe is a hint, not
  a result. That is why decisions moved to the replica.

| Probe (old → new) | Commit | Run ID | Exit | Reading, after the bias |
|---|---|---|---|---|
| bspeed-9 → an identical copy, no alignment | 121adef | run-06fbd04e438e | 75 | bias −2.5 ms |
| step 49 → an identical copy, 4 KB alignment | 5ff3bd6 | run-c43d43c3cc37 | 107 | bias +0.7 ms |
| bc-3 → bc-4 (C1, 512-document superblocks) | 124a828 | run-bf02cf18b5c3 | 35 | C1 6.5 ms slower as read, 7.2 after the bias |
| q2n → q4n (presence as an L1 bitmap of the document's terms, 9.5 KB) | ce053a3 | run-77410d781155 | 66 | the bitmap 4.1 ms slower |
| bc-3 → q2n (the quality port) | dae9496 | run-6e074f12bc59 | 110 | q2n 0.3 ms faster |
| q6t → q10 (phrase records) | cffca08 | run-78caf31a21dc | 113 | q10 0.6 ms faster |
| step 49 → 8 rotating sink rows for the tokens of no row | 040e60b | run-2202eca23930 | 80 | sink rows 2.7 ms slower |
| q6n → q6n with ALWAYS 0.30 | 8f7f442 | run-58d10d307a2f | 130 | 0.30 2.3 ms faster |
| q6n → q7n (the rows of an `always` entry prefetched 4 entries ahead) | 807de9d | run-a901468cbed3 | 93 | the prefetch 1.4 ms slower |
| q2n → q6n (byte stamps), twice | aef0bde, 299f80b | run-154dd88182f1, run-43c61f8ae49b | 121, 45 | +1.4 ms, then −6.2 ms |
| q10 → q12 (phrase records fetched first), twice | 3946d4f, 9eeca31 | run-7b7a3e79fd96, run-18d63237f1a5 | 119, 96 | +1.2 ms, then −1.1 ms |
| step 49 → step 52 | 19a626a | run-b16574a693b9 | 194 | step 52 8.7 ms faster |
| step 53 → step 54 (the plan tables; neither has the pool) | f44e04e | run-36831d3dd3bb | 125 | step 54 1.8 ms faster (the replica, in process: −0.12 to +0.88 ms) |

**The pool's wait on the box** (9b7d057, run-9ba229b6c0f0, exit 43): the probe exits at once
with the CPU's and kernel's `umwait` settings as bits. 43 means WAITPKG present, C0.2 enabled and
a `max_time` of 100,000: the same as the replica, so step 55's workers wait the same way on both.

## The replica A/B of step 52's parts

ab.sh on the replica, 31 pairs each, one run of each binary per pair in alternating order. The
table gives the medians, the median of the paired differences (new − old) and the pairs the new
binary won. All but the re-runs were taken before the replica froze builds during measurements
(14:25 UTC), but every pair alternates the two binaries, so both meet the same load.

| Step | Old → new | Medians (ms) | Paired median | Won |
|---|---|---|---|---|
| q2n: the quality port, phrase compare, trigger dedupe, info word | step 49 → q2n | 97.154 → 94.943 | −2.097 ms (−2.16%) | 31 |
| the same, re-run at 14:16 UTC | step 49 → q2n | 98.439 → 96.827 | −1.811 ms | 27 |
| q6t: byte stamps, no `malloc_trim` | q2n → q6t | 95.141 → 91.852 | −3.296 ms (−3.46%) | 30 |
| q10: phrase records | q6t → q10 | 91.637 → 91.364 | −0.431 ms | 22 |
| q12: the first phrase records fetched before the BOOL evaluation | q10 → q12 | 91.375 → 91.097 | −0.098 ms | 17 |
| q13: no prefetch of the stamps in the trigger's second pass | q12 → q13 | 90.802 → 90.747 | −0.004 ms | 16 |
| q14: no `mallopt` (measured as q12 without it) | q12 → q12nm | 91.049 → 90.677 | −0.157 ms | 18 |
| Step 52 = q14, as a whole | step 49 → q14 | 97.321 → 90.639 | −6.862 ms (−7.05%) | 31 |

- *Plain runs* (benchl.sh, 11 runs): step 52 median 91.03 ms, min 89.92.
- *Memory* (mem.sh, 5 runs): 39.05, 39.27, 39.82, 40.48 and 40.60 MB; max 40.60, under the
  44 MB credit.
- *Noise.* An A/A test of step 49 against itself read +0.14% (6 of 15 pairs won).
- *What was kept.* q12 and q13 are within that noise.

## Where the scan's time goes (replica, step 53's code)

Builds that skip one part of the scan (wrong output, timing only), each in a 15- to 21-pair ab.sh
A/B against the full program:

| Part skipped | Time saved |
|---|---|
| Phrase checks | 2.21 ms |
| The triggered per-document evaluation | 14.73 ms |
| The `always` superblocks | 9.24 ms |
| The per-token stores (stamps and row ORs; this also disables the triggers) | 25.67 ms |
| Everything but the token path | 34.88 ms: the token path, setup and tail are about 56.6 ms |
| The row ORs alone | 10.74 ms |

Of the 36,276,657 tokens, 13,542,645 (37%) are of no row term and go to the one sink row. So the
token path (tokenize, hash, probe) is the larger half of the run, and the read-modify-write of a
row word per token, 10.7 ms, is the largest single store cost. The ways around it that we tried
lost (experiments below: rotating sink rows, a branch for row terms, sending repeats to the sink).

On step 78's code (non-PGO, seed 1), the same skips save 2.90 ms for the phrase checks, 9.25 ms for
the triggered evaluation, 7.58 ms for the `always` superblocks and 19.10 ms for all three; its token
path is 33.3 ms (step 78, "What is left"). On step 82's code (plain build, seed 1), skipping the
triggered evaluation whole saved 11.22 ms, the `always` superblocks 7.18 ms and the phrases
2.72 ms (steps 83 to 86 above).

## Experiments that did not make it (measured, reverted)

Board-measured reverts are in the table above (steps 11, 12, 14, 24, 27, 46, 48, 50 and 51). These were measured elsewhere. Five early ideas (lookups batched per document, a larger hot table, an early return on a zero result, hot tables of 2^10 to 2^14 buckets, prefetching the term table while parsing queries) were tried on the test server, but their numbers were not kept, so no result is claimed for them.

What lost, by family:
- Branches on a random predicate: the row read-modify-write behind a branch, +11.2%; `add_doc`
  only on a hit, +3.869 ms (0 of 31).
- More per-thread state: an L1 bitmap of the document's terms, a board probe 4.1 ms slower; two
  scans interleaved per thread, +10.535 ms (0 of 31).
- Prefetches too early or too wide: `prefetchw` in the probe loop, +1.404 ms; every token's entry
  range, +2.59 ms.
- Branch-free token extraction: +1.45 to +1.73 ms over four designs.
- Setup work on a serial path or on `mmap_lock`: answers numbered by expected matches, +6.491 ms;
  `MADV_POPULATE_READ` by the waiting workers, +7.7 ms.

| Experiment | Result |
|---|---|
| 8 rotating sink rows, chosen branch-free, to break the store-forwarding chain on the one sink word (step 53's code) | Replica +0.24% (9 of 21 pairs). The box's in-process probe on step 49 had read 2.7 ms slower ("Box probes since step 47") |
| The row read-modify-write only for row terms, behind a branch (step 53's code) | Replica +11.2%: whether a token is a row term is random, so the branch mispredicts |
| Repeated terms of a document sent to the sink (`again = lastdoc[id] == stamp`), to skip their row writes (step 53's code) | Replica +20.6% |
| A per-frequency cost table instead of `powi`; the kind written only when new; the term order as a direct counting sort (step 54 work) | Replica on top of step 54: +0.025 ms (14 of 31), +0.047 ms (14 of 31), +0.339 ms (7 of 31) |
| `mallopt(M_TOP_PAD, 32 MB)`; `M_MMAP_THRESHOLD` 32 MB; `M_TRIM_THRESHOLD` 1 GB with `M_MMAP_THRESHOLD` 32 MB (step 53's code, without the pool) | Replica: TOP_PAD made setup 0.44 ms shorter but the scan 2.4 ms longer; MMAP_THRESHOLD +0.6 ms; both +0.85 ms |
| Board sweep on step 47's code (10:09 UTC), against its base run-29fa59a9bf1f (0.108491 s): the block end's slot prefetch 32 ahead instead of 16 (slots32), its range prefetch 8 ahead instead of 4 (ranges8), ALWAYS 0.20 | run-9ea0f03c117b 0.122315 s, 43.14 MB; run-9c6ffba5e923 0.109786 s, 41.65 MB; run-37c6fa647f3c 0.144971 s (marked slow in our log). None beat the base |
| Branch-free batched token extraction, simdjson style (Langdale and Lemire, 2019): three designs on step 49 and one rebased on step 52 (bx-ext) | Replica, 31 pairs each: +1.57, +1.70, +1.73 ms on step 49 and +1.45 ms on step 52, won 1 to 3 of 31. Each removed the 4.0M mispredicts of the extraction loop's exit (−30 to −33% branch misses). But that branch resolves early, on the `blsr` chain: about 25 issued uops and 5 recovery cycles per miss, and the sibling hyperthread fills the bubble. Every branch-free form cost more: +56 instructions per window (wasted entries), a serial chain of window loads, or 3 spill reloads per unrolled lookup with fewer uops from the uop cache |
| `masks()` without the slice bounds checks (an `unsafe fn` with a contract) | Replica: on step 49 −0.30, 0.00 and −0.54 ms (60 of 93 pairs); on step 52 +0.03 ms (15 of 31): not kept |
| Code alignment flags on step 52 | Replica: `-align-loops=64` +0.24% (13 of 31), `-align-all-functions=6` 0.00% (16 of 31), both +0.39% (12 of 31): B builds without them |
| ALWAYS on step 52 (replica, builds frozen) | 0.30: −0.127 ms (16 of 31); 0.35: +0.185 (13 of 31); 0.20: +0.154 (13 of 31). The board probe on q6n read 0.30 +2.3 ms (one probe, "Box probes since step 47") |
| The six rows of an `always` entry prefetched 4 entries ahead (q7n, q12rp) | Board probe −1.4 ms; replica, as q12 with it against q12, +0.484 ms (10 of 31) |
| Branch-free anchor capture in `look_up` (q14an) | Replica against step 52: +1.226 ms (5 of 31) |
| One flat list of entries for the per-document evaluation (q15) | Replica against step 52: +0.796 ms (7 of 31), with the scalar evaluation. Step 66's 8-wide evaluation reads one flat list of entry indices, in the design that won |
| A document's anchors found by `movemask` in `end_doc` (q16) | Replica against step 52: +0.553 ms (8 of 31) |
| Presence as an L1 bitmap of the document's terms, 9.5 KB, set per token and cleared through the document's tokens, instead of `lastdoc` (q4n) | Board probe 4.1 ms slower; test-server probe 1.96 to 2.36 ms slower (+168M instructions, +5.3% cycles) |
| 8 rotating sink rows for tokens of no row, to break the store-forwarding chain on one sink word | Board probe 2.7 ms slower |
| The block end's range prefetch distance 1 or 4 instead of 2 (step 49's design) | Test-server probes: 1: +0.12, −0.10, −0.52 ms; 4: −0.39, +0.18, −0.60 ms: noise |
| Sample sizes on step 49's design | Test-server probes: 512 KB 1.93 to 2.07 ms slower; 1.5 MB +0.19, +0.44, −0.90 ms; 2 MB −0.03, −0.42, +0.16 ms: 1 MB stays |
| Transparent huge pages left on (step 49 without `no_huge_pages`) | Test server: 74.40 → 73.60 ms median, but `memory.peak` 35.30-38.41 → 45.20-47.24 MB, past the 44 MB credit |
| v7–v11: streaming evaluation per document instead of an index (1cd58a4) | board run-5a81a3585198: T 0.517 s, M 104 MB, S 1,132, 56.0 pts (K 13.17): worse than step 4's K 13.57, reverted |
| Block engine: a per-block presence bitmap to avoid cold `presence` loads | 4.30G → 4.72G cycles (one thread) |
| Superblock threshold ALWAYS at 0.125 / 0.5 instead of 0.25 | 129–131 / 133 ms vs 127 ms (step 8's code, test server). After step 39 the box was asked again: 0.25 kept (steps 40 and 44) |
| One term table of 2^17 buckets | 135 ms vs 129 at 2^16 (kept 2^15 for memory) |
| Prefetching the entries' `presence` words at the block end | cycles 2.45G → 2.55G |
| Branching around a zero evaluation result | cycles 2.76G → 2.82G |
| Profile-guided build trained on the full scale instead of the sample | neutral on the test server, not pushed |
| Single-thread PGO training (step 27) pinned to the first allowed CPU instead of CPU 0 | not tried: the atomic profile counters of step 29 made the profiles repeatable instead |
| An intermediate version of step 32 (S 802, hot code identical too) | test server +1.1% against step 31 in one session (two builds each, 3 rounds of 21 runs) and +0.35% in the next; its PGO builds also moved `end_block` 16 bytes off step 31's 64-byte alignment. The final set (S 796) moves it too and ties, so the alignment is not the cause; the spread is the build-to-build variance of step 32's note |
| Bucket count from the distinct terms instead of the term occurrences | not built: at full scale the two give the same 16,384 buckets; before step 58's per-region retry, it would only have spared the tiny inputs their re-seeds |
| Document frequencies sampled into 2^16 hashed u16 counters, to overlap the sample with query parsing | simulated on seed 1: hash collisions move queries to the superblock path (5,882 → about 8,530), +138M instructions in total; not built |
| A per-segment index (1,024 documents, built at block ends, every non-`always` BOOL query checked per segment, after the box probe showed 39 ms of BOOL work) | exact; four versions +20, +15, +16 and +12 ms on the test server (86 ms base, 12 threads). One thread, version 4 against today's block path (0.28G cycles): recording and regrouping the 10.4M (term, block) words alone 0.17G, the checks 0.03G, the 2.14M live evaluations 0.29G. Killed |
| An index built inside the scan (bitsets for the top 180 terms, u16 document lists) | C microbenchmark on one laptop thread (i5-11320H, 1.25 MB L2 like the box), full corpus: tokenize + hash + probe 10.0 ns/token, presence words +2.3, the index instead +6.7 ns/token over presence words; scaled to the box about 30 to 40 ms, more than the 39 ms of BOOL work it would replace. Not built |
| Row terms written straight into superblock rows (no move at the block end) | not built: the block-end pass it saves measured about 7 core-cycles per slot, the row move 3 to 4, and it adds an address select to each of 27.6M BOOL tokens in a loop that is uop-bound (+1.5 cycles each); net 0 to −2 ms, under the build-to-build spread |
| Superblocks of 512 documents (two AVX2 vectors per term) | test server 87.5 / 87.8 ms against 87.0; with ALWAYS 0.125, 89.5 / 89.8 |
| ALWAYS 0.18 / 0.32 with the 1 MB sample | 82.6–83.3 / 85.1–85.4 ms against 82.0–82.5 (0.25 kept) |
| Phrase-anchor neighbour pre-check: compare the term next to the anchor before the whole phrase; with or without a branch-free anchor capture in `look_up` | exact; 12 threads −0.4% (pre-check only), +1.2% (both), +6% (capture only); one thread −1.8% / −3.7% / −0.3% cycles; +16 S. Not pushed |
| A thread pool for every parallel phase; threads spawned in a tree | no setup gain on the test server: the threads then started together but waited on page faults instead |
| glibc's mmap threshold fixed at 128 KB (`mallopt`) instead of `malloc_trim` | M 37.3 MB, but setup +2 ms of munmap |
| `MADV_POPULATE_READ` on each slice before its scan | +22 ms |
| No grouping of keys per split (each hash-build thread filters every split's keys by group) | +2 ms: the group test is a random branch |
| Ids and spans written by the hash-build threads straight into the final vectors (no serial concat) | setup −0.6 ms for +15 lines, a wash |
| `Vocab::finish` in parallel | slower: the spawn costs more than the 0.4 ms loop |
| Results written with `pwrite` from every formatting thread, and states, logs and scan tables released in parallel before exit | within noise |
| Entries aligned to 32 bytes (no entry across two cache lines) | step 34's build 84.7 ms against 82.4 without, on the block engine's scalar loop. Under step 66's gathers, which address word 8i + k, 32-byte entries won (−0.775 ms, 29 of 31) |
| Trigger-id band: the cover terms of block queries renumbered into one id range, so the block end loads entry ranges only for touched slots inside it | test server (beval-3 base, 12 threads, 3 copies × 4 rounds × 21 runs): 82.03 ms against 82.13, S +15: no gain |
| Trigger bitmap: one bit per term (whether it triggers entries) filters the touched slots before their entry ranges are loaded, without renumbering | test server 82.01 ms against 82.13 (an earlier session: 81.79 against 82.51), S +6: within noise |
| Bitmap filter plus a 2-term precheck: per entry the two rarest terms every match must contain; entries whose two words share no document are dropped branch-free before evaluation (1.37M of 4.26M block evaluations on seed 1) | test server 80.97 ms against 82.13 (earlier session 80.88 against 82.51): −1.4 to −2% elapsed, though cycles fell 4.7% (2.825G → 2.69G) and instructions 5.25G → 5.07G; M +0.5 MB, S +34. Not pushed: the time is worth about as much as the lines cost |
| Touched slots visited in ascending order (a bitmap of touched trigger slots, scanned word by word) | test server +1.5 to +3.5 ms: the scan and its branch misses cost more than the forward reads save |
| THP for the setup tables (`no_huge_pages` moved to after the plan is built) | test server, beval-1 base without the align flag: neutral (80.72 against 80.35 ms) |
| `ALWAYS` at 0.35 and 0.5, on top of the bitmap filter | test server, beval-1 base without the align flag: 82.18 and 84.44 ms against 80.35 at 0.25 |
| `add_wide` with one branch (count_positions always computed, branch only for an unsaturated answer with hits) | test server +1.3 ms |
| `always` term words packed into one stream (`row * 32 \| op`) with runs of equal length | test server about −0.6 ms (about −0.9% cycles) for S +15: break-even, not kept |
| `always` entries sorted by their term ids after length and operators, for row locality | test server +1.2 to +1.5 ms |
| Probes of the term table kept in its first 512 KB (token path only, answers wrong) | box: 42.0 ms (run-2cfcd3e991a4) against 41.0 ms for the full 2 MB table (run-76a286c818fc): an L2-resident table does not pay on the box |
| The token loop of step 39 as inline asm (23 instructions per token, every table pointer in a register) | instructions −84M, ops −57M, but test server +1.1 to +1.7 ms on three PGO builds |
| The table, pilots and seed read once per slice instead of once per run of tokens | instructions unchanged, cycles +2.5% |
| Info word by a volatile scalar load instead of vpextrd (one uop instead of two) | ops −37M, test server +0.6 to +0.8 ms on three builds |
| Phrase anchor-neighbour pair filter on step 39's code (phrase checks 1.30M → 0.29M) | test server −0.3, +0.25, −0.15 ms on three builds: noise |
| The rest of each block entry range prefetched one entry ahead (a quarter of the evaluations are not the first of their range) | test server −0.5 to +0.5 ms on three builds: neutral |
| Term ids from the stored hash, each thread writing its split's lines and terms straight into the shared arrays (on step 62) | Replica, setup alone (the binary exits once the vocabulary is built): −0.424 ms (37 of 41). Whole run, 2 builds a side: seed 1 −0.04, +0.18, −0.08 and +0.18 ms; fresh seed 109 +0.02, +0.12, −0.47 and +0.05 ms: the same speed |
| No `terms` vector and no copy by group: each key carries its group | Replica, setup alone −0.185 ms (31 of 41); whole run +0.28 ms (13 of 41) on seed 1 and +0.38 ms (7 of 21) on fresh seed 109 |
| `MADV_POPULATE_READ` of the corpus by the waiting pool workers | Replica +7.7 ms: eleven threads' `madvise` held `mmap_lock` and starved the main thread's allocations (the term order 0.65 → 9.2 ms) |
| Each slice populated in the sample round; or at the start of each scan thread | +1.7 ms in the sample round and −1.4 ms in the scan, net +0.3 ms; at the scan's start, no gain |
| `mallopt` with the pool: `M_MMAP_THRESHOLD` 1 MB with `M_TRIM_THRESHOLD` 256 MB; thresholds of 4 MB and 512 KB; `M_TOP_PAD` 32 MB | 1 MB: −0.43 ms (25 of 31) on step 56 with M +2.4 MB, −0.17 ms (18 of 31) on step 58; 4 MB: +0.15 ms and M 45.5 MB, over the credit; 512 KB: +0.92 ms, M −3.5 MB; `M_TOP_PAD`: the exit +2.4 ms |
| `M_ARENA_MAX=1` | Setup −0.3 ms, but the scan threads' tables share one heap: the scans end between 77 and 84 ms, +4.3 ms |
| The query splits leaked instead of freed | −0.07 ms: their unmapping only moves to the exit |
| `parse_queries` on half the threads | +1.4 ms |
| First-round chunk sizes weighted for the threads' start order (the workers start 0.37 to 0.98 ms after the main thread) | No gain |
| The slot prefetch with `_MM_HINT_T1` or `_MM_HINT_NTA` instead of T0 (on step 65) | Replica +0.421 ms (9 of 31) and +1.608 ms (0 of 31) |
| The trigger's `add_doc` only on a hit, behind a branch (on step 65) | Replica +3.869 ms (0 of 31): 52% of the 5.51M triggered evaluations hit on seed 1, so the branch is a coin flip |
| ALWAYS at 0.25, 0.35, 0.40, 0.45, 0.50 or 0.60 instead of 0.30 (on step 78; non-PGO screens) | 0.25 +0.75 ms, 0.35 −0.35, 0.40 −0.75, 0.45 −0.47, 0.50 −0.36, 0.60 +0.23 ms: 0.40 went in with step 79, whose PGO A/Bs gave about −0.1 to −0.5 ms |
| ALWAYS at 0.35, 0.45 or 0.50 instead of 0.40, on the plain build (on step 82; again on step 83 with step 85's entry loads) | 0.35 +0.194 (10 of 31), 0.45 +0.733 (5 of 31), 0.50 +0.664 ms (3 of 31); again +0.283 (6 of 31), +0.145 (11 of 31), +0.148 ms (8 of 31): 0.40 stays |
| ALWAYS at 0.25, 0.35, 0.60 or 0.80 instead of 0.30 (on step 66) | Replica +1.201 (2 of 31), +0.117 (14 of 31), +0.481 (9 of 31) and +1.545 ms (2 of 31); 17.8% of the `always` evaluations find no hit |
| One `_mm256_i64gather_epi64` per `always` term (on step 66) | Replica +1.666 ms (0 of 31) |
| SAMPLE_BYTES 2 MB instead of 1 MB (on step 68) | Replica +0.024 and +0.012 ms (15 of 31 each): its −0.65% on step 66 was gone after steps 67 and 68 |
| Sample sizes before step 70, on step 66's code with the setup work of steps 67 and 69 (replica; against 1 MB unless noted) | 512 KB +2.1 and +2.5 ms, 768 KB +0.4 and +0.8 ms (non-PGO screens, seed 1 and holdout 99); 1.5 MB, PGO: +0.07, +0.12, +0.09 and −0.31 ms; 2.5 MB, PGO: −0.64 and −0.44 ms on seed 1, −0.74 and −0.71 ms on holdout 99; with the stamps' dedupe, against 2.5 MB (non-PGO): 2 MB +0.01 and +0.50, 3 MB +0.17 and +0.38, 3.5 MB +0.11 and +0.02 ms |
| The `always` entries sorted by all their rows, not only the first (on step 69) | Replica +0.125 ms (10 of 31) |
| A phrase pre-filter on the term next to the anchor: 1.31M checks, 1.5% hits on seed 1 (on step 69) | Replica +0.627 ms (3 of 31) |
| A per-row stamp, so that `end_super` skips rows the superblock never set (43.8% of 15.55M term references are all-zero) (on step 66) | Non-PGO: with a branch +10.515 ms (branch misses 10.5M → 17.2M), branch-free to a zero row +5.058 ms (L1 misses −17M, instructions +209M), the store alone +3.681 ms (0 of 31 each) |
| The superblock's first-id logging restructured (its ceiling, with no first ids logged: −1.865 ms with PGO, timing only): the counts always by popcount then one loop for the missing ids, inline or in a cold function; the four `add` calls in a cold function; branch-free counts with the logging in a second pass (on step 66) | Replica, PGO: +0.735, +0.237, +0.233 and +2.790 ms |
| Two scans interleaved per thread, a 64-byte window or a document of each in turn (on step 62) | Replica +10.535 ms (+12.32%, 0 of 31) and +9.952 ms (+11.67%): two working sets per thread, four per core with SMT, overflow the 1.25 MB L2 (L3-miss loads 5×) |
| One document's triggered evaluation deferred into the next document's windows, 2 or 4 entries at a time; or the flat entry list evaluated at the document's end (on step 63) | Replica +3.150, +3.062 and +2.779 ms; the flat list without the deferral +1.885 ms (0 of 31) |
| Hit answers' state lines prefetched at pack time and updated at the next document's trigger (on step 66) | Replica +0.294 ms (11 of 31): L2-miss loads −17%, cycles equal: the state misses were already off the critical path |
| The group of every token prefetched at probe time; the trigger terms' first entries prefetched before the presence loop | Replica +2.592 ms (on step 65) and +2.093 ms; +0.789 ms, and +1.579 ms with the pass-1 prefetch removed |
| Constants swept on step 66 (non-PGO screens, PGO to confirm; every variant gave the reference output): GROUPS 32 or 128; SUPER 128; MIN_SLICES 16 or 24; SAMPLE_BYTES 0.5, 1.5, 3 or 4 MB | GROUPS +0.31% and +0.19% (9 of 31 each); SUPER 128 +12.4% (0 of 31): superblocks are four AVX2 words, so 128 evaluates half-empty vectors twice as often; MIN_SLICES 16 and 24 +131% and +274%: `par` runs the parts beyond its 12 workers one after another on the calling thread; SAMPLE_BYTES 0.5 MB +2.42%, 1.5 MB −0.46% with PGO, 3 MB −0.02%, 4 MB +0.18% |
| Scan-ahead: the workers idle during `Plan::new` pre-probe the first 5 to 15% of each slice, and the scan replays it (measured, not built) | Ceiling with a perfect rebalance −1.19 ms at 5% and −2.06 ms at 10%; the workers alone −1.46, −1.65 and −2.35 ms at 5, 10 and 15%. But replaying 5% costs 2.9 to 3.4 ms: split apart, the probes' misses no longer overlap the evaluation work in the out-of-order window. Not built: about +40 to 50 S for at most −1.2 ms within the idle window |
| A parallel scatter for the plan's entries and anchors, each thread placing a key range, with the `always` sort by cached keys (on step 65) | Replica +0.234, +0.200, +0.079 and +0.190 ms (11 to 14 of 31), +23 S: the waiting threads of the round it replaced were time for mapping the corpus in |
| The matches of 8 entries added by a `tzcnt` loop over the match mask, no left-pack (an earlier version of step 66) | Replica −0.807 ms against step 64 (22 of 31); the final version is −2.3 to −2.7 ms against step 65 |
| The left-pack by a 256-entry table of lane orders, the ranges kept in a work list (an earlier version of step 66) | Replica −1.11 to −1.34 ms against step 65 (28 to 30 of 31, four build pairings); the final `pdep`/`pext` pack, one flat index list and the presence bits shifted in were 0.697 ms faster again (26 of 31) |
| That version with a prefetch of each range's first answer state | Replica −0.870 ms against step 65 (26 of 31): the prefetch cost 0.24 to 0.47 ms |
| Entries of 28 bytes (7 words, no padding) under the 8-wide evaluation | 0.775 ms slower than 32-byte entries (29 of 31) |
| Each token's entry range (`group[t]`) prefetched when its slot is probed | Replica +2.59 ms on step 65, +2.09 ms on an earlier 8-wide version |
| `prefetchw` of the stamp's line in the probe loop, as soon as its id is known, instead of just before the store; and with the row word's line too (step 77 work) | Replica, non-PGO: +1.404 ms (1 of 31); +1.838 ms (0 of 31) |
| The stamps of phrase-only terms all written to one byte, `lastdoc[id.min(bool_terms)]` (on pw1, step 77's first piece) | Replica, non-PGO: +1.252 ms (0 of 31) |
| 16-byte `pending` entries, the slot offset in the key's spare lane 3 by `vpinsrd` (on pw1; and on c1, step 77's second piece) | Replica, non-PGO: +0.390 ms (8 of 31); +0.50 ms on c1. Step 78 keeps 16-byte entries, with the offset as a second 4-byte store |
| The probes and the presence stores in one loop again, with the `prefetchw` (on c1) | Replica, non-PGO: +2.156 ms (0 of 31) |
| The `pending` write pointer kept across windows; the next window's masks computed after the token loop; both (on h1, step 77's last piece) | Replica, non-PGO: +0.140 ms (12 of 31), +0.333 ms (4 of 31), +0.180 ms |
| A 64-bit `vpmovmskb` (no zero extension) and a wrapping end clip in `masks` (on h1) | Replica, non-PGO: +0.423 ms (6 of 31) |
| The probes in fixed batches of 32 tokens during the scan, instead of once per document (step 78 work) | Replica, non-PGO: +0.462 ms (3 of 31) |
| The probe fused into the extraction loop, with no slot prefetch and no `pending` (step 78 work) | Replica: +3.353 ms (0 of 31) |
| No slot prefetch at all (step 78 work) | Replica: +1.412 ms (0 of 31) |
| The probe loop unrolled by 8, `pending` padded with the last slot's entry (step 78 work) | Replica: −0.002 ms (16 of 31): the same speed |
| The token loop as a guarded do-while (step 78 work) | Not timed: PGO lays the loop out top-tested, with a jump back, either way, so the machine code is identical |
| malloc's trim off, `mallopt(M_TRIM_THRESHOLD, 1 GB)` (on step 77) | Replica −0.019 ms (16 of 31) and −0.269 ms (20 of 31), M +1.5 MB: within the spread of PGO builds of one source, for more memory. Step 82 keeps it, on the plain build: −0.218 and −0.507 ms on its own |
| The output written through a shared mapping of results.txt, from answer states put in line order, with two digits per division in `push_decimal` (on step 77) | Replica +0.430 ms (4 of 31) and −0.118 ms (21 of 31). Phase marks: the format took 0.33 ms less, but the ids pass 0.2 to 0.3 ms more for the copy into line order; the copy into the mapping took 0.12 ms instead of the write's 0.59 ms, but the page releases then came after it, and the tail ended at the same time; the exit took 0.25 ms longer, unmapping the shared output pages. `push_decimal` alone changed nothing measurable |
| The superblock rows 32-byte aligned (step 80 work, on step 78) | Not built: split loads 15.0M on step 77 and 16.5M on step 78, split stores 6.6M and 8.6M, of about 4G instructions: well under 0.1 ms |
| The next entries' rows prefetched in `end_super`, 4, 8 or 16 entries ahead (on step 78) | Replica, non-PGO: −0.065, −0.137 and −0.042 ms: noise |
| The anchors as bits, a byte per 8 tokens, listed in `end_doc` (on step 78) | Replica, non-PGO: +0.765 ms (3 of 31) |
| The sample's document frequencies counted per thread and added at the end (on step 78) | Replica, non-PGO: +0.180 ms (5 of 31) |
| One window bounds check in `masks` with a wrapping end clip; and with the stamp reset limited to the BOOL terms (on step 78) | Replica, non-PGO −0.317 ms; PGO seed 1 +0.02 and −0.32, holdout 99 +0.20 and −0.28 ms: noise. The limited reset: no gain |
| A `prefetchw` of the row word next to the stamp's; no `prefetchw` of the stamp at all (on step 78) | Replica, non-PGO: +0.186 ms; +1.467 ms: the stamp's prefetch is still needed |
| Every document looked up one document late, from two `pending` buffers, so that its slots have a document's time to arrive (on step 78) | Replica, non-PGO: +1.002 ms (1 of 31) |
| `malloc_trim(0)` in the release round (step 81 work) | On a worker after its releases: the exit 0.18 ms shorter, but 0.37 ms spent; alongside the other releases, the write and the releases +0.32 ms: it takes the heap's locks and, for the main heap, `mmap_lock` for writing |
| The query file and the plan's line tables released too (step 81 work) | Replica, non-PGO: +0.046 ms (15 of 31) |
| `term_order` without its keyed copy (step 81 work) | +0.28 ms in its phase: the keys are computed twice |
| The plan's grouping on three threads: the entries, the anchors and the `always` sort (step 81 work) | No gain: the entries' `csr` alone takes 1.0 ms; with the item above, +0.43 and +0.27 ms |
| `vocab.finish` in parallel (step 81 work) | −0.16 ms in the phase probe; A/B −0.030 (17 of 31) and −0.158 ms (19 of 31): too small to keep; on step 82's code −0.053 ms (17 of 31), for 5 more lines |
| SAMPLE_BYTES swept again (non-PGO, 21 pairs, against 2.5 MB; step 81 work) | 1.5 MB −0.04 (11 of 21), 2 MB +0.03, 3 MB −0.18 (14 of 21), 3.5 MB +0.07 ms: 2.5 MB stays |
| MIN_SLICES (step 81 work) | Not a lever at 12 CPUs: there are max(threads, 8) = 12 slices, and `par` runs any part beyond the pool on the main thread, after its own |
| Shared prefixes among the `always` entries (step 84 work) | Not built: 37 of the 2,429 entries of 3 or more terms share their first two terms and operators with the entry before them (sorted); 2,392 distinct prefixes |
| Skipping the rest of an `always` entry after a zero first row before an AND (step 84 work) | Not built: 0.9% of the row loads could be skipped |
| A 2-id phrase pre-check (step 84 work) | Not built: 3% of the checks pass it, and a phrase's cost is the anchor range lookup and the record fetch, not the 8-id compare; this filter lost twice before (above) |
| A per-thread list of the `always` entries still logging first ids, evaluated first, then every entry counted without a branch (step 84 work) | Replica +0.40 ms (8 of 31) |
| Every `always` evaluation counted by `count_positions`, then the logging only for answers below FIRST_IDS with hits (step 84 work) | Replica +0.744 ms (1 of 31) |
| `add_doc` one document late, the state lines prefetched when the matches are found; late without the prefetch (steps 83 to 86 work) | Replica +1.095 ms (0 of 31); +0.493 ms (6 of 31) |
| `group` prefetched for every trigger occurrence in a loop before the first pass (steps 83 to 86 work) | Replica +0.323 ms (5 of 31) |
| No prefetch of each range's first entry in the first pass (steps 83 to 86 work) | Replica +0.603 ms (5 of 31) |
| The stamp gathers masked to the real terms, padding 0xFFFF (steps 83 to 86 work) | Replica +0.170 ms (11 of 31): masked lanes cost the same |
| The phrase checks moved between the first pass and the entry evaluation (steps 83 to 86 work) | Replica +0.010 ms (15 of 31) |
| `trigger` not inlined into `end_doc` (steps 83 to 86 work) | Replica −0.037 ms (16 of 31): the same speed |
| The group bounds read as one unaligned u32 (steps 83 to 86 work) | Replica +0.169 ms (10 of 31) |
| Skipping the fifth stamp gather when no entry of the 8 needs it (steps 83 to 86 work) | Not built: worth at most one gather, about 0.36 ms, minus the mispredicts |
| Answers numbered by expected matches, from the sample's frequencies and each entry's truth table, sorted (steps 83 to 86 work) | Replica +6.491 ms (1 of 31), holdout 99 +6.311 ms: the scoring and the sort run in the single-threaded plan |
| The entries that match alone (truth bit 0) numbered first, in two passes without a sort (steps 83 to 86 work) | Replica +0.169 ms (9 of 31). It packs the hits better (the most-hit 4,096 states take 81% of the hits, against 65%; at best 85.5%), so `add_doc`'s state misses overlap, and they are not the wall time |
| First ids grouped by answer instead of line (steps 83 to 86 work) | Replica +0.492 ms (6 of 31): the format then reads ids and line ends out of order |
| The log's first quarter populated (`MADV_POPULATE_WRITE`) at `Scan::new` (steps 83 to 86 work) | Replica +1.007 ms (1 of 31): 12 threads populating at once pay more than the faults spread over the scan |
| A 900 KB first-id log from the heap (steps 83 to 86 work) | Replica +0.150 ms (12 of 31): the arenas had no free memory, so more faults |
| The scan's tables (states, `lastdoc`, the log's head) built and faulted in by the waiting workers during the plan (steps 83 to 86 work) | Replica +0.450 ms (8 of 31): all 12 were ready 2 ms before the scan, but the plan's `par` calls waited on workers busy with 1 ms jobs |
| Line-ordered states for the format (steps 83 to 86 work) | Replica +0.099 ms (11 of 31) |
| Two digits at a time in `push_decimal` (steps 83 to 86 work) | Replica −0.150 ms (22 of 31), for 13 more lines |
| The log's line prefetched for writing once a document, with `Plan::anchor` as u16, as one bundle (on step 86) | Replica +0.038 and −0.118 ms, holdout 99 +0.092 and +0.019 ms: a mean of +0.008 ms |
| Step 88's cuts with the padding chain written `.chain(&pad).copied()` (step 88 work) | The same hot code 16 bytes off step 86's offsets mod 64: +0.126 and +0.172 ms (means of four A/Bs, two builds) |
| Only one of step 88's two `Plan::with` cuts (step 88 work) | `classify`'s alone puts the hot code 48 bytes off: +0.096 ms (mean of 8 A/Bs, 6 positive); the records call's alone 32 bytes off: −0.074 ms, but 3 lines fewer saved than step 88 |
| `masks` moved onto a 64-byte boundary too, by an `extend` in `records` that drops a `RawVec::grow_one` copy placed between the hot functions and `masks` (c4b); the hot code 32 bytes off with `masks` at 32 (c4h) (step 88 work) | c4b against step 86: −0.127, −0.065, −0.295 and −0.316 ms, then −0.173, −0.011, −0.035 and +0.051: a mean of −0.121 ms over 8, so a gain from `masks`'s alignment is possible but not shown; c4h +0.033 ms (4 A/Bs) |
| `lines` inlined into its two uses; `run`'s `start` parameter as `mut at` (step 88 work) | No line saved: rustfmt then wraps `fits` onto two lines; the signature goes past 100 columns, +5 lines |
| Scan-ahead again, with a branch-free replay (on step 88; timing-only builds with the right output, each gain measured within one binary with and without its pre-pass; not built): workers 1 to 11 tokenize and probe their own slice in 16 KB units while the plan is built, and the scan replays it | The vocabulary is final at 3.3 ms and the plan at 8.1, but the waiting workers map the corpus in until 6.0 ms, so about 1.9 ms a worker is free (0.7 ms for worker 1, none for the main thread). The replay costs 2.21 ms for 5% of every slice (the earlier probe's above: 2.9 to 3.4 ms), and 1 ms of pre-pass saves 0.54 to 0.59 ms of scan. The pre-pass slows the main thread's plan by 0.33 to 0.60 ms at caps of 3 to 8%, and gains nothing unless slices 0 and 1 shrink and its buffers are released before the exit: then −0.574 and −0.747 ms (29 and 27 of 31) on seed 1 and holdout 99 at an 8% cap, against an A/A of −0.075 ms, with M 32.18 → 36.98 MB. Not built: a ceiling of 0.6 to 0.75 ms (+0.09 to +0.11 points) for about 65 to 70 lines (−0.2 points) and 5 MB |
| `#![no_main]` with the data paths still joined by `format!` (step 89 work) | The scan 32 bytes off step 88's offsets mod 64. Replica against step 88: seed 1 −0.276, −0.106, −0.061 and −0.070 ms, holdout 99 +0.079, +0.203, +0.038 and +0.052 ms: a mean of −0.025 ms over the first four, −0.018 over all eight; tiny seed 201 −0.134 and −0.136 ms (98 and 99 of 101). The `concat` build against it: −0.085 ms |
| Other forms of `main.rs` near the paths, to put the hot code back after `#![no_main]` (step 89 work): `format!("{}/{}")`, an untyped `\|name\|`, `data_dir.clone() + "/" + name`; `is_full` written three other ways; `collect::<Box<[_]>>()`, `.into_boxed_slice()` or `full.set(..)` instead of `get_or_init`; the output file by `File::create(&format!(..))` or with `.as_str()` | The layout of the `format!` build or worse: the three `is_full` forms compile to identical code, and the two output-file forms move the `par` jobs and the scan by −16 bytes. Only `concat` put the scan back, and no form found puts the `par` jobs back too |

- **A faster setup can leave the whole run as it was.** Two setup changes above measured faster on
  their own, −0.424 and −0.185 ms, and in the whole run the same speed and +0.28 ms. Phase marks
  show the later phases (the plan and the scan) slowing by about the setup's gain. Our
  explanation, inferred and not measured directly: glibc raises its mmap threshold when it frees a
  large mapped block, and freed arena memory stays faulted in for the thread's next allocation, so
  the setup's temporaries pre-fault the scan's per-thread tables (about 150 pages a thread).
  Remove a temporary, and the scan takes those page faults itself, under contention. Profile-guided
  builds of one source also differ by about ±0.3%, so a change under about 0.5 ms needs two builds
  a side. Step 81's plain build is the same binary on every build, so from then on one build a
  side is enough.
- **Byte-identical copies of one binary ran up to 2.5% apart on the test server** (for example 83.1 against 84.4 ms, the same sha256), so every comparison here averages several copies of each binary.

## Robust on unseen seeds

README, "Robust on unseen seeds", has the full argument. In short: the plan is made per input from
that input's own sample, the perfect hash places a region again or re-seeds rather than answer
wrongly, holdout seed 99 and fresh tiny and sample seeds match the reference at every gate, step
59 was byte-exact on 30 fresh full-scale seeds and in 1,505 more runs, step 66, whose 8-wide
BOOL evaluation the submitted code still runs, on fresh seeds 101-110, in 200 repeats and on two
corpora made to stress it, and steps 77 to 90, the last the submitted code, on the 30 fresh seeds at 1, 3,
7 and 12 CPUs (every run exact; all in "How the final number was verified"), and on the test
server the
quality-pass source (step 46b's code) times the same on three full-scale seeds: medians of two PGO
builds, 21 runs each, 12 threads.

| Input | Step 46b's builds | Step 44's builds | Superblock queries | Block-entry evaluations |
|---|---|---|---|---|
| Full seed 1 | 80.43 / 80.74 ms | 80.73 / 80.65 ms | 6,023 | 4.52M |
| Holdout seed 99 | 80.33 / 80.76 ms | 80.12 / 80.35 ms | 6,044 | 4.55M |
| Full seed 2 | 80.20 / 80.42 ms | 80.48 / 79.77 ms | 6,105 | 4.45M |

These timings are of the block engine (steps 44 and 46b). The per-document design of step 56 ran
full seed 1 in 86.93 ms and holdout seed 99 in 86.69 ms on the replica (medians of 21 runs), and
step 59 ran 30 fresh full-scale seeds in 84.73 to 88.18 ms (medians of 7 runs; seed 1: 85.47 ms). It makes its plan the same way (covers and ALWAYS from the input's own sample),
and every gate checks its output on the holdout and on fresh seeds.

## What we learned about the box

The box is the board's scoring machine; the test server is a 16-vCPU AMD VM. On Track B:

- **Cuts of uops on the token path pay.** Step 17 cut 6.8% of the instructions and the box went
  0.1619 → 0.1484 s (−8%); step 39 cut 6.2% and the box went from 0.116583 / 0.119467 to 0.113311
  s. The perfect hash (step 31) replaced four key compares with one probe: box −10.4%. But
  instruction cuts that did not shorten the token path's stalls did not show: steps 18 and 19 (each
  4 to 5% fewer instructions on the test server) ran 0.1478 to 0.1643 s against step 17's 0.1484.
- **L2-miss cuts do not pay.** Numbering the answers in evaluation order cut L2 misses 10% on the
  test server, and the one-word filter 19% more; the box did not move (0.1443 against 0.1459 s,
  steps 23 and 24). Two probes of the token path alone (answers wrong, by design): with the 2 MB term
  table it took 41.0 ms on the box (run-76a286c818fc), with every probe kept in the table's first
  512 KB 42.0 ms (run-2cfcd3e991a4). An L2-sized table buys nothing there.
- **Hiding random loads pays.** Prefetching the block end's entry ranges (step 20: 0.1478 →
  0.1404 s) and the two-pass block end (step 28: 0.1374 s against 0.1389 and 0.1420) both moved
  the box, more than the test server predicted.
- **Random state costs.** Step 46 did a third fewer evaluations (test server −1.9%) but added a
  bitmap, a pair per entry and two per-thread lists: the box ran 0.134041 s against 0.109 to 0.115,
  and M went to 45.52 MB, over the credit. Step 26's 4.5 MB copy of the answer state put M at
  44.09 MB.
- **Settings tuned on the test server can be wrong on the box.** ALWAYS: the server's sweep picked
  0.125; on the block engine the box ran faster at 0.25 (steps 40 to 44; 0.35, 0.5 and 0.30 did not
  beat it). On the per-document design one box probe read 0.30 2.3 ms faster, and the replica could
  not separate the two at first (step 53), so 0.30 went in; on step 66's code the replica put 0.25
  1.2 ms behind it, and on step 78's code 0.40 beat it (experiments; step 79). Loop
  alignment: on the server it removed a 1.5 to 2.5 ms layout gap; on the box B is not slower
  without it (step 38). Setup work, by contrast, transferred: spreading it over twelve threads took
  9 to 11 ms off the box (steps 15 and 16, against step 10's 0.1713 s), and a probe later put the
  box's setup at 16.5 ms, the same as the server's 16.4 to 16.8 ms.
- **Fewer random loads per document beat fewer instructions per block.** Step 49 evaluates more
  often than the block engine (4.89M per-document evaluations against 4.3-4.6M block evaluations),
  but each one is a table lookup over at most 5 stamps, with no presence words, touched lists or
  block ends. The box gained about 9%, more than the test server's 7.7%.
- **Smaller per-thread state pays on the box's CPU.** One byte per term instead of four (step 52,
  with no `malloc_trim`) gave −3.3 ms on the replica. The L1 bitmap of q4n, which needed a store
  per token and a clear per document, lost 4.1 ms in the box's probe.
- **Even in-process probes on the board are noisy.** Two probes run twice with identical code
  gave opposite answers (+1.4 then −6.2 ms; +1.2 then −1.1 ms). The other teams' builds on the
  E-cores share the L3 cache and memory with our run, and they come and go within a run's 1.6 to
  2 s. A replica with the box's CPU and the builds frozen settled what the board could not
  ("How we measured").

## How we measured

Four tools, from the coarsest to the finest.

1. **The board.** One run per push on the scoring box, an i5-13500.
   - *What shares the box.* Our program runs on P-cores 0-11 (2.5 GHz, turbo off). Meanwhile other
     teams' Docker builds run on E-cores 12-19 of the same chip: a track built with
     `target-cpu=native`, as ours are, is built on the box, two builds at a time
     (`board/README.md`). They share the L3 cache and memory with our run, and they come and go.
   - *The spread this gives.* Step 49's binary ran 0.097889 to 0.113384 s in 15 runs. Earlier,
     single runs of unchanged code took 35 to 45% longer (the note at the top).
   - *Use.* So board runs decide only large changes, in alternating dispatches.
2. **The test server**, a 16-vCPU AMD VM: interleaved runs of two PGO builds, medians, perf
   counters, and in-process probes. It misled on L2-miss cuts (steps 23-24), on ALWAYS (steps 40
   and 44), and on several changes that the box ran slower (steps 11-12, 46, 48).
3. **Probes on the box** ("Box probes since step 47", and "Timing probes" in "What each step
   taught"). Exit-code probes measure phases; in-process A/B probes compare two programs in one
   process, after a measured layout bias. Repeats showed that even these can flip sign under the
   board's load.
4. **The replica**, from Sun 13:20 UTC: a rented server with the box's CPU, set up as the harness
   sets up the box.
   - *CPUs.* P-cores 0-11 at 2.5 GHz with turbo off, E-cores 12-19, transparent huge pages
     `always` (as the box, by Track A's probe). Output to tmpfs, like `/out`.
   - *Timing.* The harness's own `sd-timer` (fork, exec and wait, timed with `CLOCK_MONOTONIC`), so
     T is exec to exit, as on the board.
   - *Builds.* The Dockerfile's `RUN` line in a chroot of the official image, on the E-cores. From
     about 14:25 UTC every build is frozen (cgroup freezer) while a measurement runs, as the final
     scoring has the box to itself.
   - *`ab.sh N old new`.* N pairs, one run of each binary per pair, in alternating order (old new,
     new old, ...), each run under a bench lock. It reports both medians, the median of the paired
     differences and how many pairs the new binary won.
   - *Memory.* `mem.sh` reads the cgroup v2 `memory.peak` of each run; we quote the max of 5, as
     the final scoring takes the largest of five peaks. Every peak includes the kernel's per-CPU
     charge batches: up to 63 pages per CPU, 0 to 3 MB at 12 CPUs, different every run (measured
     on the replica; Track A's PERF.md, "What we learned about the box"). So one binary's M varies
     by about a megabyte from run to run.
   - *Phases.* A `mark()` build prints the elapsed time at each phase boundary.

   Calibration against the board, on the binaries of 13:05 UTC, 21 runs each:

   | Track and binary | Replica median (min, mean) | Board runs of the same binary |
   |---|---|---|
   | A, Track A's step 48 | 17.17 ms (16.72, 17.59) | 16.836 and 17.041 ms (2 runs) |
   | B, step 49 | 96.92 ms (96.28, 97.05) | 97.889 to 113.384 ms, median 101.022 (15 runs) |
   | C, Track C's step 39 | 19.24 ms (18.98, 19.24) | 19.031 to 21.787 ms, median 19.321 (9 runs) |

   - *Memory.* Over 5 runs (mem.sh): B 37.06-37.95 MB, against the board's 36.06-37.96 MB for
     step 49.
   - *Offset and noise.* The replica's median sits just below the board's fastest run of the same
     binary, as expected when the board's noise only adds time. An A/A test (step 49 against
     itself) read +0.14%, 6 of 15 pairs won.
   - *Phase budget of step 49* (replica, `mark()` build, medians of 7 runs):

     | Phase | ms |
     |---|---|
     | Setup | 12.5: the query split 2.6, grouping 1.1, pilots 1.1, assembly 0.8, the sample 0.9, the plan 5.7, finish 0.4 |
     | Scan | 77.0 |
     | After the scan, per thread | 2.1-2.6 |
     | Format | 1.4 |
     | Write | 0.5 |
     | Exit | 2.0 |

## How the final number was verified

**The gate.** The exact Dockerfile build of each version, so the binary the board builds (PGO
until step 80, plain from step 81) is the one tested. Steps 52 to 55 each passed the same gate on the replica's build; for step 52:

- full seed 1, sha256 f86973c21022…, at 1, 3, 7 and 12 CPUs; sample seed 1, eeeec1e9319d…;
- sample seeds 2 to 4 and holdout seed 99, byte-equal to the reference output;
- the 144 tiny, fresh and adversarial runs of the gate script (below): GATE PASS;
- `objdump | grep -c vpinsrb` = 12. These are not the bit-by-bit mask rebuild that the inline
  `vpmovmskb` avoids. They are LLVM's gather of an entry's four stamp bytes in the per-document
  evaluation (`vmovd`, three `vpinsrb`, `vpcmpeqb`, `vpmovmskb`).

Steps 55 to 58 passed it too (full seed 1 at 1, 3, 7 and 12 CPUs, samples 1 to 4, holdout 99, the
144 runs), each with the same 12 `vpinsrb` of the stamp gather; their memory, max of 5 runs, is
39.67, 40.44, 36.91 and 31.06 MB. Steps 59 to 61 passed it too, with 30.28, 29.89 and 30.29 MB, and step 61 also gave the seed-1
hash at 1 and 3 CPUs. Step 62 passed it with 17 hand-made edge inputs added, at 1, 3 and 12 CPUs, and steps 63 and 64
with the five skewed corpora too (66 hand-made edge runs in all), and step 65 the same.

Step 66 passed it too: the 144 runs and the 66 hand-made edge runs, the five skewed corpora
among them, with `vpinsrb` at 0, since the stamp bytes are now read by `vpgatherdd`; memory, max of
10 runs, 33.55 MB. Its evaluation code, before the 6-term assert was added, gave the reference's
output on fresh full-scale seeds 101-110 (14 of 14 runs) and in 200 repeated runs. A non-PGO build
with `-C overflow-checks=on -C debug-assertions=on` (138 overflow-panic call sites in its binary)
passes the gate too, on an 8-CPU laptop (full seed 1 at 1, 3, 7 and 8 CPUs, samples 1 to 4, holdout
99, the 144 runs), so no arithmetic outside the explicit `wrapping_*` operations wraps on these
inputs. Two corpora made to stress the new evaluation give the reference's output at 1, 3 and 8
CPUs, from that build and from step 66's PGO binary built on the replica: 500,000 documents;
6,000 or 8,000 BOOL queries whose covers are 80 or 160 rare terms, and 350 other lines (common
BOOL queries, phrases, repeated lines); 0.3 or 0.5% of the documents carry up to 30 of the rare
terms (the second corpus also with repeats, upper case and 1% empty documents). That is 23,266 and
26,029 ranges, every one of over 8 entries (122 and 83 on average), and up to 3,836 entries in one
document, the same at 1, 3 and 8 threads. Steps 67 to 70 passed the same gate (the 144 runs and
the 66 hand-made edge runs), with `vpinsrb` at 0; memory, max of 10 runs, 31.53, 31.85, 32.73 and
31.69 MB. Steps 72 to 75 passed it too (31.81 and 32.07 MB; step 74, max of 5, 31.75 MB;
step 75, max of 10, 31.32 MB); steps 71 and 76 change comments only. Steps 77 to 90 passed it
within the final gate below.

**The submitted code's final gate** (step 90, replica, Mon 2 Nov, about 08:53 UTC): the exact
Dockerfile build, from the production Dockerfile, checked to be the plain one, and 798ff2d's six
source files (sha256-checked; binary 4baa1eec1837…), 15 checks, all passed. Every run that writes answers was byte-equal to the
reference:
- the gate above: full seed 1 (f86973c21022…) at 1, 3, 7 and 12 CPUs, samples 1 to 4, holdout 99,
  the 144 tiny, fresh and adversarial runs and the 66 hand-made edge runs; `vpinsrb` 0;
- 18 board-like edge cases at 1 and 12 CPUs, as root and as uid 65534: 72 of 72 runs;
- fresh full seeds 101-130 at 1, 3, 7 and 12 CPUs: 120 of 120;
- fresh tiny and sample seeds 201-210, as root and as uid 65534, at 1, 3, 12 and all 20 CPUs:
  160 of 160;
- holdout 99 at 1, 2, 3, 4, 6, 7, 8, 12 and all 20 CPUs (E-cores too): 9 of 9;
- 20 repeats of fresh seed 101 at 12 CPUs and 20 at all 20: 40 of 40;
- malformed queries with tiny seed 201's documents (a BOOL query of 7 terms, a phrase of 9 terms,
  an empty queries.txt): each stops with exit 134 and its panic message, the empty file with
  "queries.txt: empty, or no last LF"; checked apart from the gate on step 79, whose
  `parse_queries` this is, a last line without LF stops the same way; by hand on step 89, whose
  `main.rs` this is, a missing data directory stops with exit 134 and the OS error;
- memory, max of 20 runs, 32.00 MB; `rustfmt` clean on every file; S 1175 by the official count.

Steps 77 to 89 passed the same 15 checks, steps 77 to 80 as PGO builds: step 89 (about 07:49 UTC;
binary 353d8e73566c…; memory, max of 20 runs, 31.71 MB; S 1171), step 88 (about 07:08 UTC;
fe4603cf5dc3…; 32.11 MB; S 1169), step 87 (about 06:51 UTC;
8aae237c2759…; 32.04 MB; S 1177), step 86 (about 06:05 UTC; 2765de5e9baf…; 32.39 MB; S 1194), step
85 (about 05:32 UTC; 0842d9c49e0f…; 32.46 MB; S 1192), step 84 (about 05:10 UTC; 9ecf94c01d04…;
32.11 MB; S 1189), step 83 (about 05:00 UTC; 0c46473c1cb5…; 31.94 MB; S 1192), step 82 (about 04:28
UTC; 972f182bc910…; 32.78 MB; S 1178), step 81 (about 04:00 UTC; 4a661174044b…, the same binary as
the screening build; 30.91 MB; S 1174), step 80 (about 03:20 UTC; e23d0c52aa2f…; 30.93 MB; S 1172),
step 79 (about 03:00 UTC; 673926eee004…; 31.34 MB; S 1165), step 78 (about 02:40 UTC;
40e1403937f3…; 31.16 MB; S 1164) and step 77 (01:53 UTC; 4851c1fe3b5d…; 31.26 MB; S 1155), whose
empty queries.txt still stopped on the index out of bounds.

**Robustness runs** (replica, Sun 1 Nov, about 17:00 to 19:20 UTC), on the exact Dockerfile
build, with data generated on the replica by the organisers' generator (it reproduces the official
sample seed 1 byte for byte). README, "Robust on unseen seeds", says what each column ran. Every
run was byte-exact, with no crash, no nonzero exit and nothing on stderr:

| Version | Fresh full seeds 101-130 | Repeats, and CPU contention | Tiny and sample, uid 65534, up to 20 CPUs | Page-aligned files |
|---|---|---|---|---|
| Step 59 (b-stack-8) | 40/40 runs | 660/660 | 840/840 | 5/5 |
| Step 62 (b-stack-11) | 34/34 runs | 600/600 | 720/720 | — (and 72/72 edge runs) |
| Step 63 (b-stack-12) | 34/34 runs | 600/600 | 720/720 | — (72/72 edge runs, the five skewed corpora) |

Earlier versions (steps 49 and 53 to 58) passed too. Steps 60 to 62 change lines, checks and a
prefetch, not the output path, and passed the gate. On the 30 fresh seeds step 59 ran 84.73 to
88.18 ms (medians of 7 runs; seed 1: 85.47 ms), with a memory peak of 30.11 to 31.63 MB on seeds
101-110 (max of 5 runs).

Step 49's gate (two test-server builds):
- full seed 1 at 12, 3 and 1 CPUs;
- samples 1 to 4, and holdout 99 at 12 and 8 CPUs;
- 12 fresh sets (tiny 801-806 and 911-914, sample 51-52) against the official reference at 1, 3
  and 8 CPUs.

Before step 58 the tiny sets re-seeded the perfect hash, so the gates of that time also ran its
re-seed path; since step 58 a thread whose bucket finds no pilot places its regions again, that
bucket first (README, "Robust on unseen seeds").

**The box number.** Single board runs of one binary have spread up to 45% in disturbed
dispatches, so:

- **The replica first** (steps 52 to 90): paired runs, each part against the stack before it.
- **Board samples** then check. Step 52's last two runs were 0.093419 and 0.092845 s, and step
  53's runs 0.091413 to 0.092929 s, against step 49's median of 0.101022 s. An in-process probe on
  the box read step 52 8.7 ms faster than step 49. Step 54's four runs, 0.093673 to 0.114435 s,
  never beat step 53's, and step 55's best, 0.089795 s, did; steps 57 to 65 ran 0.085425 to 0.092338
  s in twenty clean runs, step 66 0.082048 and 0.082597 s (its second run, 0.094116 s, is
  unexplained: Track A ran slow in all three of its dispatches), steps 68 and 69 0.079675,
  0.081637 and 0.07961 s, step 70 0.078573 s, steps 71 and 72 0.078623 and
  0.080472 s, step 73 0.095266 s with B alone slow, step 74 0.078856 s, step 75
  0.078867 s, step 76 0.094031 s with B alone slow and 0.077853 s, step 77 0.074272, 0.075027
  and 0.074086 s, step 78 0.093093 s, with A at its best then and C normal, and 0.072974 s, step 79
  0.093217 s with A slow too, 0.092461 s with A and C normal, and 0.074609 s, step 80 0.093693 and 0.073165 s
  with A and C normal, step 81 0.072172 s, with A slow in that dispatch, step 82 0.071995 and
  0.071937 s (and 0.121495 s, logged as disturbed), step 83 0.06988 and 0.06982 s, step 84
  0.069944 and 0.069276 s (and 0.115912 s in a heavily disturbed dispatch), step 85 0.068502 s
  (and 0.143676 s with A slow too), step 86 0.070026 and 0.098043 s, both with A slow too, and
  0.079581 and 0.090154 s with every track slow, step 87 0.071064 s (and 0.082291 s with A
  slow too), step 88 0.067846 s (and 0.088981 and 0.088847 s with A slow too), step 89 0.067543,
  0.068272, 0.067464 and 0.067066 s (and 0.103705 s with A slow too, 0.135523 s with B alone
  slow), and the submitted step 90 0.066749 s (and 0.077266 s with B alone slow, 0.147443 s with A
  slow too). Steps 87 and 88 have step 86's machine code for the scan, and none of steps 86 to 89
  is separated from step 85 by its board runs: their gains rest on the replica, as does step 90's,
  whose best run is 0.47% under step 89's.
- **Two groups of B runs.** Since step 66, B's board runs have fallen into two groups. Of its 59
  runs, ten took over 0.1 s and one 0.098 s with Track A slow too (disturbed dispatches, and step
  89's third with B alone slow); of the other 48, 36 took 0.0667 to 0.083 s and 12, a quarter,
  took 0.0888 to 0.095 s: step 66's second and fourth runs, step 79's first two (the first with
  Track A slow in the same dispatch), one run each of steps 70, 73, 76, 78 and 80, step 86's third (every track
  slow) and step 88's first two (A slow too). The replica shows no such mode. Step 78's gate build, run 60 times back to back on full
  seed 1 at 12 CPUs, took min 71.32 ms, median 71.94 ms and mean 72.13 ms. A single run 20% slower
  would lift the mean by about 0.24 ms, so at most one of the 60 could have been slow.
- **Alternating dispatches**, before the replica: candidates pushed in turn, minutes apart, against
  the code they would replace, and a result had to repeat (steps 41 to 44: 0.12433 for the
  sort-free first ids, 0.114997 back on the sort, 0.112506 sort-free again; steps 45, 46b and 47
  above). A dispatch where Track A's or C's unchanged code was also slow is marked and not used.
- **Exit-code probes** measure phases the board does not report.
- **Points marked \*** are computed from T and S with exponents fitted to the logged runs (K ∝
  T^-0.5 · S^-0.15 while M is under the credit), which reproduce every logged B point value to
  within 0.01.

### What every gate checked

sha256 of the seed-1 output; byte comparison with the reference output for sample seeds 2–4; 30
generated `tiny` seeds against `reference/b_index.py` at 1, 3 and 8 CPUs; adversarial inputs (token
lengths 1, 12, 13, 16, 17, 32, 33 and 70, mixed case, separators, empty and token-less documents,
repeated and absent terms, a corpus whose size is an exact multiple of 4096); the same output at 1,
2, 3, 7, 12 and 16 threads; `objdump | grep -c vpinsrb` = 0 on the server build (12 for steps 52
to 65: LLVM's gather of the stamp bytes, above). For steps 8 and 9, also a full-scale seed 99
against the official reference. From step 31 the perfect hash's failure paths were tested by
forcing them, on the hash build of that time (step 58's per-region retry was forced again in 300
runs; the doubling path was not, README "Not covered"): four builds that fail seed 0 outright, fail
seeds 0 to 2 (so seed 3 doubles the table), mask seed 0's hash to 20 bits (two different keys with
one hash) and offer seed 0's buckets a single pilot (no free pilot); each must give the reference
output on the full, sample, holdout, tiny and adversarial inputs at 1 and 8 CPUs, and each did.
Seed 0 succeeds on the full and sample inputs; before step 58 the tiny inputs re-seeded, usually up
to seed 3, so the gates of that time also ran the re-seed and doubling path. From step 33 on,
every gate also runs fresh seeds the earlier checks had
not seen (tiny seeds 701 to 706 and sample seeds 41 to 43, against the reference at 1, 3 and 8 CPUs),
an adversarial set of 1-term phrases, absent terms and `x NOT x` queries whose answers are mostly
empty, holdout seed 99 byte for byte, and the seed-1 hash at 1, 3, 7 and 8 CPUs.
