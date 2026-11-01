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
| 7 | 20856e2 | Exit right after writing; each slice's pages dropped by its thread. Rerun on a4e4404 (B unchanged), run-cc0e62d06c76: 0.2650 s, 67.64 | run-94bcf1f00dfc | 0.2658 | 38.64 | 922 | 67.61 |
| 8 | 196434d | Queries with common cover terms evaluated once per 256 documents with AVX2; lookups gathered per document with their buckets prefetched; one term table instead of hot table + Bloom filter | run-1a4ed151ee61 | 0.199 | 41.5 | 1,028 | 70.4 |
| 9 | 15bc6b2 | Leaner tokenizer; branch-free table probe (inline-asm cmov). Rerun run-b33ff9c0bd9c: 0.1716 s, 71.84 | run-a6b773e07cad | 0.1864 | 41.25 | 1,081 | 70.94 |
| 10 | 68e6990 | Static linking (`crt-static`): less dynamic-loader work before `main`. Its first run, run-6e66fcd85faa (0.252 s), was a disturbed box: A, C and D were slow in the same dispatch | run-058dad4dcdce | 0.1713 | | 1,081 | 71.85 |
| 11 | 8246f98 | **Reverted** (cf64a47). One worker pool, parallel query parsing, per-block presence bitmaps: test server 118.2 → 109.6 ms, box no faster, 144 more lines. First run run-9be281b6c1b8 (0.241 s) was noise | run-7c5098eab231 | 0.1766 | 38.4 | 1,225 | 71.1 |
| 12 | 9711c01 | **Reverted** (cf64a47). Step 11 with touched lists again and the presence words set in their own prefetched loop: no box gain either | run-fe77dad5e332 | 0.1789 | | 1,235 | 70.95* |
| 13 | 0fe0395 | The answer state written when it is allocated (a lazily zeroed page read before it is written faults twice). Box-neutral, kept: 2 lines | run-c33e91b88799 | 0.1792 | | 1,083 | 71.36* |
| 14 | 56f8b0a | **Reverted** (1421702). Six threads, one per P-core, instead of twelve: memory 26.8 MB, but M already scored in full, and time lost 7% | run-0022d3e17006 | 0.1921 | 26.8 | 1,088 | 70.59* |
| 15 | 1421702 | Twelve threads again; an all-zero superblock result returns early while its query still logs first ids; block evaluation without bounds checks; slices start at any line instead of a 256-document boundary. One slow run, not repeated: step 16 builds on it | run-37b3b2a1ace8 | 0.2131 | | 1,084 | 69.48* |
| 16 | 091e35b | Setup spread over the threads (query lines split and keyed in parallel, the plan's per-line passes in parallel); each token carries its bucket index. Rerun run-5a8826a5f10b: 0.1601 s, 72.43 | run-3682cf4f995c | 0.1619 | 38.3 | 1,134 | 72.31 |
| 17 | 26b5cb5 | Leaner token lookup: long tokens on a cold path, the bucket probe compares dwords, the scan's probe and the table's `get` share one function that keeps the table in registers, the CRC stays in 32 bits | run-b16e2000c75d | 0.1484 | 38.6 | 1,140 | 73.24 |
| 18 | e24c58d | Superblock queries evaluated over their real terms only, sorted by term count and operators so the operator branch predicts. A second run, on 33cac7e, took 0.1643 s | run-6820636c30fc | 0.1529 | | 1,142 | 72.91* |
| 19 | 34dd399 | No bounds checks in the block-end loops (9b7b6f9, which had no board run of its own); then no per-token prefetch of the presence word. First run run-13cc611d4761: 0.1518 s, 72.96* | run-2bc33f24a19f | 0.1478 | | 1,151 | 73.26 |
| 20 | 8fdef14 | The block end prefetches the entry range of the slot 16 ahead and the entries of the slot 8 ahead (one thread: cycles −5.2%). Rerun run-f0af75ddb8ab: 0.1416 s | run-4b9ff571a190 | 0.1404 | 38.46 | 1,162 | 73.78 |
| 21 | 831097c | Same engine in fewer lines; hot loops unchanged. First run run-bb8fc84e151e: 0.1466 s, M 39.68, 73.81, in a dispatch where A was slow too | run-72157fe9a7e1 | 0.1435 | 39.51 | 998 | 74.04 |
| 22 | a3a700b | Fewer lines again, hot code reformatted with the same operations; SAFETY arguments for every prefetch and unchecked load | run-1ed2ef15f7af | 0.1459 | 38.54 | 854 | 74.37 |
| 23 | 8274ba4 | Answers numbered in evaluation order (README, design 5): test server −3.4%, L2 misses −10%. Its only run was a disturbed dispatch (A 0.0263 s against about 0.0195) | run-36419fde7616 | 0.1550 | | 880 | 73.61* |
| 24 | d8e7a08 | **Reverted** (cb193e5). Step 23 plus a 64 KB one-word filter in front of the term table, so most tokens no query uses skip the 2 MB table: L2 misses −19% on the test server, box equal to step 22 (0.1443 against 0.1459) for 19 more lines | run-cda99758e008 | 0.1443 | | 899 | 74.32 |
| 25 | cb193e5 | Step 23's numbering kept in a shorter form, filter dropped. No board run of its own: step 26 followed six minutes later | | | | 859 | |
| 26 | 9a61acf | Fewer lines in setup and the result gather. M 44.09 is just over the 44 MB credit; The cause, found in step 28: step 23's formatter copied every thread's state into line order, about 4.5 MB | run-d00be2692c6c | 0.1389 | 44.09 | 844 | 74.93 |
| 27 | 368aa64 | **Reverted** (387ebb3). PGO training on one thread (`taskset -c 0`), so that builds are repeatable: two local builds then differed by 5 KB instead of 1.1 MB. The board's builder may not use CPU 0, so the Docker build failed on every track and there is no scored run | | | | 844 | |
| 28 | 841e809 | The block end in two passes (README, design 3): ranges gathered and prefetched first, then words and state fetched 4 ranges ahead of the evaluation (test server −4.5 to −5.7%); the formatter reads each thread's state through `answer_of` instead of a copy (M back to 39 MB). Rerun on 2515d02, run-65497002e714: 0.1381 s, 38.68 MB, 74.88 | run-3814466b62c2 | 0.1374 | 39.22 | 875 | 74.94 |
| 29 | 7fc8af9 | Fewer lines with the hot loops unchanged (Rust 864 → 778 code lines, the token and LF masks from one load; test server −1.3%), and the PGO training run with LLVM's atomic profile counters (fb34663, all tracks) | run-59b2e0a8fdd5 | 0.1353 | | 789 | 75.44 |
| 30 | 31cbd48 | Query parsing relies on the spec's query grammar (2.3) instead of re-checking it; malformed lines still abort on a slice bound. Neutral on the test server | run-ff1d9b72da23 | 0.1338 | 39.21 | 781 | 75.60 |
| 31 | 776e1dc | A perfect hash of the query terms instead of the 4-way bucket table (README, design 2): two AES rounds as the hash, one 16-byte slot per term, a branch-free probe, built on 12 threads. Test server −8.4%, 12-thread cycles −11%; box −10.4%. | run-4a28a650e9ac | 0.1199 | 39.62 | 818 | 76.64 |
| 32 | bdf6d27 | Fewer lines in cold code only: the hot functions compile to the same instructions as step 31's. Test server: two PGO builds 84.96 and 85.28 ms (median of 31 runs, mean of 3 interleaved rounds) against step 31's three builds 85.98, 85.25 and 85.96. Rerun on bbdb79e (B unchanged), run-a361a4d1c316: 0.1189 s, 40.13 MB (M from the board's best-B record, `board.py`), 76.82, then the best B run | run-0741f63811ab | 0.1205 | 39.67 | 796 | 76.67 |
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
| 44 | d576688 | ALWAYS back at 0.25, as a board A/B of step 43's 0.125. The best B run | run-4bb3867f9472 | 0.108588 | 42.02 | 856 | 77.57 |
| 45 | 0613dd1 | The formatter prefetches the 12 threads' states of the line 8 ahead: they are stored in answer order and read in line order, so each line's 12 reads are random. Test server: format phase 1.9 → 1.35 ms, −0.35 to −0.56 ms on three PGO builds. Second sample on ade4b30, run-6b284372d9e8: 0.109028 s, 41.98 MB, 77.52 | run-2e7482eb1235 | 0.115166 | 41.06 | 858 | 76.92 |
| 46 | 631d15b | **Reverted** (ade4b30). Step 45 plus a block end that filters touched slots by a per-term trigger bitmap and drops entries whose two rarest required terms share no document before evaluating them (1.37M of 4.26M block evaluations on seed 1): test server −1.9% on three builds, but the box ran 23% slower and over the 44 MB credit | run-e007250809db | 0.134041 | 45.52 | 892 | 75.00 |
| 47 | | Plan::new groups the anchors and sorts the `always` entries on a second scoped thread while the main thread groups the entries (serial before: about 1.45 ms on the test server). On step 45's code: −0.9 to −1.3 ms on three PGO builds; merged here with the quality pass. Board run pending | | | | 905 | |

The submitted source is step 44's code with code-quality changes only (renames, SAFETY arguments,
setup split into named functions, named constants): `Scan::run`, `end_block` and `end_super` have
the same instructions at the same offsets as step 44's in local PGO, local plain and test-server
PGO builds (objdump, 0 differing), and on the test server two PGO builds of each time the same
(medians of 31 interleaved rounds of 11 runs: 80.90 and 81.02 ms for step 44, 81.05 and 80.89 for
the submitted source), so step 44's run measures its speed. Its S is 896 (`static_axes.py`) against
step 44's 856. Later board runs (run-991a3ad938c2, run-5fd89506515e, run-2e7482eb1235: ALWAYS
0.35 and 0.5, and a formatter prefetch, 0.112231 to 0.117245 s) are of experiments not in this
source.

Later runs of step 37's binary (with the flag): run-d97dcca08b7d, 0.12489 s, 40.61 MB, 76.20 (5d6d235, a dispatch our log
calls "a bit slow"); run-7bbf842f335a, 0.121849 s, 41.73 MB, 76.47 (51d3674, "box slower than
morning"); run-8addd6917ec9, 0.121414 s, 42.24 MB, 76.51 (3e7d9a4, SLOW BOX); run-749fda72c1d3,
0.118445 s, 41.5 MB, 76.78 (1f8d96a, normal box). S is the harness's `static_axes.py` count, which matches the board for
every run where the board's S was logged. M is blank where it was not logged. Points marked * are
computed from T and S with the board's formula: every M logged since step 6 is under the 44 MB cap
(26.8 to 43.8 MB) except step 26's 44.09 MB, which costs 0.01 points, and the formula reproduces every
logged point value to within 0.01. M is blank for steps 29 and 35 to 37 because runs.log did not
record it, and for steps 10 and 19 because the values once written there are not in runs.log;
steps 33 to 44 are at 40.6 to 42.2 MB wherever it was logged.
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
  40 to 43), with 0.25 0.113311 and 0.108588 (steps 39 and 44), so 0.25 stays. A board sweep since (0.35: 0.112231 s in a slow
  dispatch; 0.5: 0.117245) did not beat it.
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

## Experiments that did not make it (measured, reverted)

Board-measured reverts are in the table above (steps 11, 12, 14, 24 and 27). These were measured elsewhere.

| Experiment | Result |
|---|---|
| v7–v11: streaming evaluation per document instead of an index (1cd58a4) | board run-5a81a3585198: T 0.517 s, M 104 MB, S 1,132, 56.0 pts (K 13.17): worse than step 4's K 13.57, reverted |
| Prefetched lookups in batches per document | within noise on the test server; number not kept |
| A larger hot table in step 4's design | within noise on the test server; number not kept |
| Block engine: a per-block presence bitmap to avoid cold `presence` loads | 4.30G → 4.72G cycles (one thread) |
| Block engine: early return when an evaluation's result is zero | within noise on the test server; number not kept |
| Block engine: hot table of 2^10 to 2^14 buckets | 2^13 fastest on the test server; numbers not kept |
| Prefetching the term table while parsing queries | within noise on the test server; number not kept |
| Superblock threshold ALWAYS at 0.125 / 0.5 instead of 0.25 | 129–131 / 133 ms vs 127 ms (step 8's code, test server). After step 39 the box was asked again: 0.25 kept (steps 40 and 44) |
| One term table of 2^17 buckets | 135 ms vs 129 at 2^16 (kept 2^15 for memory) |
| Prefetching the entries' `presence` words at the block end | cycles 2.45G → 2.55G |
| Branching around a zero evaluation result | cycles 2.76G → 2.82G |
| Profile-guided build trained on the full scale instead of the sample | neutral on the test server, not pushed |
| Single-thread PGO training (step 27) pinned to the first allowed CPU instead of CPU 0 | not tried: the atomic profile counters of step 29 made the profiles repeatable instead |
| An intermediate version of step 32 (S 802, hot code identical too) | test server +1.1% against step 31 in one session (two builds each, 3 rounds of 21 runs) and +0.35% in the next; its PGO builds also moved `end_block` 16 bytes off step 31's 64-byte alignment. The final set (S 796) moves it too and ties, so the alignment is not the cause; the spread is the build-to-build variance of step 32's note |
| Bucket count from the distinct terms instead of the term occurrences | not built: at full scale the two give the same 16,384 buckets; it would only spare the tiny inputs their re-seeds (README, design 2) |
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
| Entries aligned to 32 bytes (no entry across two cache lines) | step 34's build 84.7 ms against 82.4 without |
| Trigger-id band: the cover terms of block queries renumbered into one id range, so the block end loads entry ranges only for touched slots inside it | test server (beval-3 base, 12 threads, 3 copies × 4 rounds × 21 runs): 82.03 ms against 82.13, S +15: no gain |
| Trigger bitmap: one bit per term (whether it triggers entries) filters the touched slots before their entry ranges are loaded, without renumbering | test server 82.01 ms against 82.13 (an earlier session: 81.79 against 82.51), S +6: within noise |
| Bitmap filter plus a 2-term precheck: per entry the two rarest terms every match must contain; entries whose two words share no document are dropped branch-free before evaluation (1.37M of 4.26M block evaluations on seed 1) | test server 80.97 ms against 82.13 (earlier session 80.88 against 82.51): −1.4 to −2% elapsed, though cycles fell 4.7% (2.825G → 2.69G) and instructions 5.25G → 5.07G; M +0.5 MB, S +34. Not pushed: the time is worth about as much as the lines cost |
| Touched slots visited in ascending order (a bitmap of touched trigger slots, scanned word by word) | test server +1.5 to +3.5 ms: the scan and its branch misses cost more than the forward reads save |
| THP for the setup tables (`no_huge_pages` moved to after the plan is built) | test server, beval-1 base without the align flag: neutral (80.72 against 80.35 ms) |
| `ALWAYS` at 0.35 and 0.5, on top of the bitmap filter | test server, beval-1 base without the align flag: 82.18 and 84.44 ms against 80.35 at 0.25 |
| `add_wide` with one branch (count_positions always computed, branch only for an unsaturated answer with hits) | test server +1.3 ms |
| `always` term words packed into one stream (`row * 32 \| op`) with runs of equal length | test server about −0.6 ms (about −0.9% cycles) for S +15: break-even, not kept |
| `always` entries sorted by their term ids after length and operators, for row locality | test server +1.2 to +1.5 ms |

| Probes of the term table kept in its first 512 KB or 128 KB (token path only, answers wrong) | box: 41.0 ms (run-76a286c818fc) against 42.0 ms (run-2cfcd3e991a4) for 512 KB: an L2-resident table does not pay on the box |
| The token loop of step 39 as inline asm (23 instructions per token, every table pointer in a register) | instructions −84M, ops −57M, but test server +1.1 to +1.7 ms on three PGO builds |
| The table, pilots and seed read once per slice instead of once per run of tokens | instructions unchanged, cycles +2.5% |
| Info word by a volatile scalar load instead of vpextrd (one uop instead of two) | ops −37M, test server +0.6 to +0.8 ms on three builds |
| Phrase anchor-neighbour pair filter on step 39's code (phrase checks 1.30M → 0.29M) | test server −0.3, +0.25, −0.15 ms on three builds: noise |
| The rest of each block entry range prefetched one entry ahead (a quarter of the evaluations are not the first of their range) | test server −0.5 to +0.5 ms on three builds: neutral |

- **Byte-identical copies of one binary ran up to 2.5% apart on the test server** (for example 83.1 against 84.4 ms, the same sha256), so every comparison here averages several copies of each binary.

## Other seeds

The submitted build on the test server (12 threads, `taskset -c 0-11`, output on tmpfs), medians
of 21 runs for each of two PGO builds; query and evaluation counts from a counting build at 12
threads. The output is byte-equal to the reference on seed 99, and the same as step 44's on seed 2.

| Input | Median (two builds) | Step 44's builds | Superblock queries | Block-entry evaluations |
|---|---|---|---|---|
| Full seed 1 | 80.43 / 80.74 ms | 80.73 / 80.65 ms | 6,023 | 4.52M |
| Holdout seed 99 | 80.33 / 80.76 ms | 80.12 / 80.35 ms | 6,044 | 4.55M |
| Full seed 2 | 80.20 / 80.42 ms | 80.48 / 79.77 ms | 6,105 | 4.45M |

## Correctness checks run before every push

sha256 of the seed-1 output; byte comparison with the reference output for sample seeds 2–4; 30
generated `tiny` seeds against `reference/b_index.py` at 1, 3 and 8 CPUs; adversarial inputs (token
lengths 1, 12, 13, 16, 17, 32, 33 and 70, mixed case, separators, empty and token-less documents,
repeated and absent terms, a corpus whose size is an exact multiple of 4096); the same output at 1,
2, 3, 7, 12 and 16 threads; `objdump | grep -c vpinsrb` = 0 on the server build. For steps 8 and 9,
also a full-scale seed 99 against the official reference. From step 31 on, the perfect hash's
failure paths are tested by forcing them: four builds that fail seed 0 outright, fail seeds 0 to 2
(so seed 3 doubles the table), mask seed 0's hash to 20 bits (two different keys with one hash) and
offer seed 0's buckets a single pilot (no free pilot); each must give the reference output on the
full, sample, holdout, tiny and adversarial inputs at 1 and 8 CPUs, and each did. Seed 0 succeeds on the full and sample inputs; the tiny
inputs re-seed, usually up to seed 3 (README, design 2), so every gate run also exercises the
re-seed and doubling path. From step 33 on, every gate also runs fresh seeds the earlier checks had
not seen (tiny seeds 701 to 706 and sample seeds 41 to 43, against the reference at 1, 3 and 8 CPUs),
an adversarial set of 1-term phrases, absent terms and `x NOT x` queries whose answers are mostly
empty, holdout seed 99 byte for byte, and the seed-1 hash at 1, 3, 7 and 8 CPUs.
