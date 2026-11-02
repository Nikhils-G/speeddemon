# Track A: performance log

**Result.** The reference runs 32.05 s at 228 MB and S 56. The submitted code is step 69 (am5-2,
abc7b44, S 456): step 68 with the endpoints' name order sorted after the merge's hand-off, while
the other threads start pass 2 (replica −0.92% and −0.86% against step 68). Its first board run,
run-a533307c210b (0.018707 s, 18.62 MB, 84.89 points), was a slow run: Track A alone was slow in
that dispatch, and B set its best time in the same dispatch (run-567ede8911b8, 0.074086 s). Its
second, run-b3a4984e4320 (0.015715 s, 18.22 MB, 86.88 points), is the best Track A run and the
fastest Track A time. Step 68 (am5-1, 9f2993f, S 452) is step 67 with the endpoint table in static
memory, a cheaper cold lookup and the space window 3 bytes later (replica −1.18% and −1.19%); its
run-be74feabcc76 (0.015722 s, 18.44 MB, 86.85 points) had them until step 69. Steps
64 to 67 are step 63's engine with a reviewed `main` (the worker defined once, named structs, a
lines pass, review comments), each at the same speed on the replica; until step 68, step 64 had
the best Track A run, run-1bda764d4b1e (0.016043 s, 18.67 MB, S 442, 86.65 points), and step 63
the fastest Track A time, run-219d1a5f89d0 (0.016017 s, 86.64 points). Eight of the nineteen board runs of steps 61 to 66 took 0.018 s or more, two of them
in DISTURBED dispatches, against four of the fifteen of steps 59 and 60. Three
ideas carry it: a parse that finds each line's fields from one AVX2 mask
and looks its endpoint up where it lies, with two slices side by side per thread (steps 43 and
51); 4-byte records grouped into hours as runs of lines, so no timestamp is parsed per line and no
disorder bound is assumed (step 8); and a pass 2 that counts ranks for small groups and writes
each hour in place once its start is known (steps 19 and 44). Steps 56 to 66 traded 27 lines
(0.20 points of S) for an exact id bound, a reviewed `main` with named structs, a lower memory peak
and a shorter tail, and the best points moved from step 55's run-ea674ef8448a (0.016399 s, S 428,
86.44) to step 64's, then to step 68's. Every run ID below is in the board's
history for team SALAAR; "probe" marks a timing build that reports through its exit code ("Box
probes").

| What moved the score most | Step | Board, before → after |
|---|---|---|
| Parallel glue between the passes | 3 | 0.0749 → 0.0510 s |
| 4-byte records, nothing record-sized alive while writing | 8 | M 44.66 → 24.3 MB, +3.8 points |
| Exit right after the write | 12 | 0.0367 → 0.03066 s |
| Pass 2 that does not wait for the previous hour | 19 | 0.0227 → 0.020053 and 0.019785 s |
| A third fewer lines, the same engine | 26-27 | 83.73 → 84.80 points at the same T |
| A parse that builds no key, two slices side by side | 43 | parse end 13.8 → 10.1 ms (probes) |
| Rank counting for groups of up to 64 values | 44 | six runs, 0.016848 to 0.018635 s |
| Two pairs of slices per thread | 51 | replica −1.2 to −1.4%; best run 0.016307 s |

Every step from the reference to the current version, with the live-board run that measured it.
Points are the board's 50·log10(K). The board is noisy on this track. Step 51's binary ran
0.016307 to 0.023882 s in four runs, and earlier steps had single outliers of up to about 40%.
Those outliers came in dispatches where the other tracks' unchanged code was slow too; our run
log marks those dispatches DISTURBED or SLOW BOX, and so does this table.

Before pushing, steps were timed in three ways ("How we measured", near the end):
- every step: on a 16-vCPU AMD test server (CPUs 0-11, `/out` on tmpfs, interleaved runs,
  medians);
- some steps: with exit-code probes on the box itself;
- from step 49 on (Sun 13:20 UTC): on a replica of the scoring box, with the same CPU and the
  board's own timer.

The commit column is the Track A code that was measured. Some board runs come from a later push
that carried the same Track A code. A dash means the value was not written in our run log; a
value read only from `board.py`'s output is credited as such in its row; * marks a value computed
with the board's formula from the logged T, M and S. Rows
without a run ID either had no board run of their own or their run is not in the log; their
numbers are left out rather than guessed.

| Step | Commit | Change | Run ID | T (s) | M (MB) | S | Points |
|---|---|---|---|---|---|---|---|
| 0 | | The reference, unchanged (the board's calibration figures, not a run of ours) | | 32.05 | 228 | 56 | 0 |
| 1 | cf9c345 | mmap, a thread per hardware thread, packed u64 records, counting sort by (hour, endpoint), sort per group | run-9042fdf66fed | 0.0796 | 57.03 | 402 | 64.72 |
| 2 | 060fea1 | AVX2 line parse: vector compares for the endpoint's end, a 32-byte key from the register, SWAR digits | run-4e23aae357cc | 0.0749 | 59.36 | 505 | 64.45* |
| 3 | 144561a | Parallel glue: prefix sums, scatter and output in parallel (pwrite into a pre-sized file) | run-92e5d2af56e2 | 0.0510 | 62.2 | 575 | 68.0 |
| 4 | 64499b4 | Scatter arrays populated in one call (board run not in our log) |  |  |  |  |  |
| 5 | 42acaa7 | Profile-guided build (training on a generated sample, seed 7) | run-8d33686a4c41 | 0.0498 | 51.6 | 597 | 68.96 |
| 6 | 8acb7f5 | Records grouped by hour in each thread; threads then take whole hours | run-36b7152f0bc2 | 0.0457 | 45.02 | 548 | 70.77 |
| 7 | c9daf53 | p50/p95/p99 by nested selection; input pages dropped by each parse thread | run-73fe01d551bc | 0.0417 | 44.66 | 559 | 71.72 |
| 8 | 9e3cfa8 | u32 records with an escape list; hours as runs of 13 timestamp bytes; records freed before the write | run-6154ff9e18eb | 0.0377 | 24.3 | 548 | 75.53 |
| 9 | 96c87ac | Method and endpoint ends from one 64-byte space mask, no branch; 64-byte keys | run-10fd036ecbb9 | 0.0361 | 24.79 | 565 | 75.82 |
| 10 | 9b51547 | Output written through a shared mapping, not a pwrite per hour | run-d92454f8ba54 | 0.03528 | 25.19 | 590 | 75.85 |
| 11 | 3ffca82 | Endpoint names copied before their pages are dropped | run-e557e7f64a7c | 0.0367 | 26.04 | 595 | 75.27 |
| 12 | 0030dfe | Exit as soon as the output is written (no frees) | run-bd4492babdc6 | 0.03066 | 26.05 | 595 | 77.20 |
| 13 | 1d7dbb3, f15bc76 | Lines built in a stack buffer; groups of up to 32 sorted; keys compared inline, not by `memcmp` (DISTURBED) | run-9ca9494b04b1 | 0.02994 | 26.11 | 599 | 77.43 |
| 13b | 90751e4, 85672d4, 27cf731 | One AVX2 parse path over a zero-padded mapping; count and errors in one packed counter | run-f90c7c73ef1b | 0.02924 | 26.37 | 581 | 77.74 |
| 14 | 689fffc | Parse on raw pointers into preallocated records; pass 2 without bounds checks; write fused into pass 2 | run-f16959eb717e, run-d79597111386 | 0.02959, 0.02257 | 21.65, 21.75 | 629 | 78.21, 81.13 |
| 15 | 1342bda | One endpoint table shared by the parse threads (CAS inserts); a static binary | run-8e8d92a5861d | 0.02188 | 20.58 | 626 | 81.72 |
| 16 | a1cf30b | The log read through 128 KB buffers instead of mapped (DISTURBED?; a second run, 0.0244 s, has no ID in our log) | run-54e9c5f0e0dd | 0.0236 | — | — | — |
| 17 | f41c193 (revert) | Back to step 15's mapped input |  |  |  |  |  |
| 18 | 6ca21c8 | Endpoint table with 8x as many slots as endpoints | run-f67608cbf0d2 | 0.0227 | — | — | — |
| 19 | dc7357f, 58c7dc8 | Pass 2 publishes each hour's length and does not wait for the previous hour | run-a97b2e82d3c7, run-23cc1c0061e6 | 0.020053, 0.019785 | 20.67, 20.05 | 667, 659 | 82.45, 82.76 |
| 20 | 06454fe | 156 fewer lines, same engine: panics instead of `Result`, one mmap helper, statistics as an array | run-658203ccf3af, run-f73fac56510d | 0.026108, 0.019836 | —, 20.05 | 503 | —, 83.62 |
| 21 | d8eeb54 | Endpoints past byte 92 found by 32-byte windows instead of a byte loop | run-253bf424b270 | 0.0224 | — | — | — |
| 22 | f200302 | Pass 2 by endpoint id, groups in name order through one sorted id list | run-2ddd83b092ac | 0.0237 | 18.49 | — | — |
| 23 | ab0b4c3 | Names and hours sorted by their first 16 bytes as one number; slices compared only on ties | run-ad6ddaf4d8e4 | 0.019691 | 19.87 | 504 | 83.73 |
| 24 | 33cac7e (reverted in e6e4d9d, reinstated in step 38) | Parse blocks from a shared counter, four per thread | run-dee36d077800 | 0.019777 | — | 517 | — |
| 25 | d8e2636 (reverted in 1c8d136) | The endpoint key built inline; the latency shuffle control from one table load | run-b68029ac7816 | 0.019916 | — | 514 | — |
| 26 | f8ccd39 | A third fewer lines, one file, same engine and hot loops | run-b5ca2d290934 | 0.019723 | 21.01 | 335 | 84.80 |
| 27 | a70edf7 | Each parse thread finds its own cuts; tighter imports and digit table | run-ad02131f83b0 | 0.020038 | — | 327 | 84.68 |
| 28 | fb34663 | Dockerfile only: PGO training with atomic profile counters, so the training run's threads no longer lose counter updates | run-ca06822d40f0, run-64be7f99b895 | 0.019331, 0.019387 | — | 327 | 85.08, 85.09 |
| 29 | 2148d5f | Malformed input aborts instead of reading past the pad (fuzzing: 68 of 300 segfaults → 0) | run-14334649ab5d | 0.019612 | — | 327 | — |
| 30 | 56c42bf | One set of threads for both passes, no join or respawn between them | run-666179516fda | 0.020152 | — | 338 | — |
| 31 | bbdb79e | Memory peak cut in pass 2: log unmapped, keys released, smaller pending statistics, largest hours first | run-d1362361f9e4 | 0.019837 | 18.24 | 355 | 85.16 |
| 32 | d8f31b6, 1261c8d (probes, removed in a067c63) | Two timing probes on the box ("Box probes") | run-7d4218a4cdba, run-be516aa463a5 | probe | probe |  |  |
| 33 | bf2aca8 | Endpoints sorted on 32 key bytes; unmap and key release moved off the serial merge | run-6cca232c0f6a | 0.018935 | 18.66 | 352 | 85.60 |
| 34 | b25326b | Dockerfile only: hot loops aligned to 64 bytes (`-align-loops=64`) | run-df8ecf640b7e | 0.019217 | 18.06 | 352 | 85.58 |
| 35 | f8206db (reverted in 51d3674) | apost-1: sort in worker 0, scratch kept mapped, output page tables dropped in parallel | run-7833033c46e9, run-4e7cded1b902 | 0.019312, 0.021607 | 18.52, 18.09 | 360 | 85.34, 84.22 |
| 36 | fcc1dd3 | Dockerfile only: the alignment flag off again, as a board A/B of step 34 | run-8f956a29e3c5 | 0.027124 | 18.55 | 352 | 81.72 |
| 37 | 16747e3 | 352 → 337 lines in setup and merge; loop alignment back on | run-c0c000c115eb, run-faf8de198217, run-62d04587cf21 | 0.019199, 0.018916, 0.01906 | 18.83, 18.77, — | 337 | 85.55, 85.72, 85.69 |
| 38 | (aspeed3-1, no board run of its own) | Four slices per thread from a shared counter, one records pair per thread (no board run of its own) | — | — | — | 351 | — |
| 39 | 067466b (board runs from 5afab2c, 8ee6e49, 6923e1e) | aspeed3-2: records released once per part; the final wait sleeps (first run: slow dispatch) | run-ad77dae6f7a8, run-f864a17b46cc, run-5e361abae820 | 0.020124, 0.019000, 0.018979 | 19.07, —, — | 346 | 84.90, —, 85.56 |
| 40 | 51f3e80 (board runs from b9237f3, 2c8ba1c, 1877866) | aspeed3-3: after the last write, each thread drops its share of the output's page-table entries | run-b273d14e2d15, run-d71d8b7f9981, run-73354c271737 | 0.018743, 0.018716, 0.018676 | 19.12, 19.15, — | 353 | 85.59, 85.60, 85.61 |
| 41 | 1806066 (runs from 7f08fe7, aa82492, 045a6b5, 9ce324f and a B sweep push) | Code-quality pass: comments, `unsafe fn` contracts, named constants (M and points of runs 3-4 from `board.py`) | run-414047b8285e, run-06d513a7635c, run-696e1acfaf1a, run-e4efcd67f121, run-38a2fba0decc | 0.020761, 0.018823, 0.020965, 0.018636, 0.021146 | —, —, 18.61, 18.96, 18.39 | 372 | —, —, 84.32, 85.52, 84.28 |
| 42 | 145795b, 8bcfa4c (probes, removed in 0c3a5e2) | Two parse-end probes: step 41's parse, then step 43's | run-bbf6e7ffe2bd, run-500e61325035 | probe | probe |  |  |
| 43 | c6eb62d | **A new parse**: two slices side by side, endpoints looked up without a key, digits converted in place | run-2d6a1338fdb0, run-07f44a5540cc | 0.018084, 0.019052 | 18.82, 19.43 | 445 | 85.29, 84.59 |
| 44 | afa31f5, f019f31, 1feac3a (last run from 81939f6) | Percentiles of groups of up to 64 values by vector rank counting (`small_ranks`) | run-0043d10b380f, run-fe32e261c61b, run-acd1a4f10e47, run-0cb9b6287711, run-263496e70d89, run-d638919e2aa2 | 0.018635, 0.01767, 0.018193, 0.017822, 0.016848, 0.018359 | 19.06, 19.93, 19.42, 19.24, 19.41, 18.59 | 477 | 84.69, 85.07, 84.87, 85.13, 85.70, 84.96 |
| 45 | 128785c, 3379197 (control, not kept) | Control: step 44's percentiles on step 41's parse, pushed in turn with step 44 | run-30865275a092, run-0574f8aa8919, run-ac75907c6d86 | 0.017936, 0.022111, 0.021109 | 18.77, 18.49, 18.42 | 404 | 85.71, 83.50, 84.02 |
| 46 | d992dda (probe, removed in 47895fc) | Probe of the box's huge-page settings | run-b06ce137b1e9 | probe | probe |  |  |
| 47 | efe48f0 (probe, removed in 81939f6) | Probe of memory bandwidth: a one-byte-per-64 touch at 12 and 6 threads | run-c08713cbca4a | probe | probe |  |  |
| 48 | 1fea871, 0a004fb | 18 fewer lines: parse setup tidied, one `Merged` hand-off, the error rate computed once | run-3b8b071a8077, run-e2fc9a65154e | 0.017041, 0.016836 | 19.09, 18.8 | 459 | 85.78, 85.97 |
| 49 | 26e8dcd, 53b3acf (not kept) | **Reverted.** Every latency through one digit parser (replica +3.76%) | run-d179dc3b3bab, run-8eaf3b5d15dc | 0.018935, 0.019025 | 18.92, 19.4 | 432 | 84.87, 84.71 |
| 50 | d7ceddb, 7386b07, 6e9c184 | Lines written backwards straight into the mapped file; group statistics in one function | run-d32a7ea98388, run-c3508c56354b, run-be6b3e3ecef3 | 0.016502, 0.01783, 0.018402 | 19.12, 19.5, 18.87 | 448 | 86.20, 85.27, 85.07 |
| 51 | 07661f2, 7d68514, dad7f0f (last run from 5290d47) | Two pairs of slices per thread instead of four (replica −1.19% against step 50) | run-746bb553b4c4, run-a5488d26b2e6, run-f5d0484af6d5, run-cab6a2cefe9b | 0.023882, 0.019369, 0.016954, 0.016307 | 19.12, 19.28, 18.8, 19.5 | 448 | 82.18, 84.42, 85.98, 86.24 |
| 52 | 6e11129 (reverted in dad7f0f) | **Reverted.** The line step as a macro, no call in the parse loop (replica −0.37%: noise) | run-3095c2120a1b, run-405ba795d8ee, run-2eb1965d8148, run-c333427ecdf7, run-67e929c99eac, run-7acf6da1d447, run-079c1450486c, run-c8e7373c5150 | 0.018045, 0.016601, 0.016844, 0.019543, 0.01696, 0.018962, 0.021073, 0.018455 | 19.3, 19.03, 19.21, 19.72, 19.12, 18.83, 20.87, 19.56 | 459 | 85.11, 86.07, 85.88, 84.15, 85.82, 84.68, 83.08, 84.80 |
| 53 | c8f98f2 (later runs from cfad1ec, 19a626a) | 15 fewer lines: plain `movemask` intrinsic, cold scalar `parse_digits`, literal digit tables | run-83c59987dc69, run-ed4b7fbffd5c, run-ea665f2d6dd0 | 0.016586, 0.017029, 0.016724 | 19.31, 18.82, 19.06 | 433 | 86.21, 86.04, 86.18 |
| 54 | 9d8aa6b (second run from 48ee977) | Shorter number formatting: digit pairs, the last one or two digits as one pair | run-d0c56533a439, run-e84901ed0253 | 0.016454, 0.016942 | 19.23, 18.9 | 428 | 86.35, 86.11 |
| 55 | d29ece2 (second run from 594fcee) | Comments only (second run: DISTURBED dispatch) | run-ea674ef8448a, run-b08d1b1d8a75 | 0.016399, 0.019527 | 19.0, 19.43 | 428 | 86.44, 84.45 |
| 56 | 99984b8 (pushed in 783e796; later runs from e2d0381, ffef5f8 and 15dd452, and from e466e5a and c25a4fc, which changed comments only) | Lost endpoint ids kept by their thread for its next insert: the id space is now a bound; `LABEL` named | run-363747825a0f, run-982e59dd03fc, run-ed78405aded2, run-3fbfca04d8d2, run-6689df89881f, run-705ea81010b2 | 0.016437, 0.016698, 0.016896, 0.020535, 0.01678, 0.016505 | 18.86, 19.14, 19.8, 18.79, 18.87, 19.35 | 436 | 86.39, 86.15, 85.88, 83.99, 86.16, 86.23 |
| 57 | 44e0f8f (pushed again in 6c74bed and 1d0d4d2; runs from 44e0f8f, d45040c, f44e04e, 4968eb1 and 2c3726c) | **Not kept.** A two-stage hand-off from the merge, statistics in endpoint-id order, `merge` in its own function, review fixes in `main` (replica −1.07%; three slow runs, the first in a DISTURBED dispatch) | run-985d6d613f50, run-4450635c0aa4, run-b52d7aae5afa, run-a7d4ecb54f55, run-5ebacd965273 | 0.020201, 0.019909, 0.016445, 0.01712, 0.020064 | 18.98, 18.6, 19.95, 18.6, 19.25 | 455 | 83.98, 84.23, 86.00, 85.87, 84.00 |
| 58 | 7b08d0e (pushed again in a106aad, 06dc6af, 807ce06 and 9551b98; runs from bd025d6, a106aad, 88af8ca, 53173d3, 807ce06, 33ecba3 and eb1258d) | **Not kept.** Comments only: a map of `main`'s six phases at its top, and a numbered marker at each (replica +0.06%; third and seventh runs DISTURBED) | run-1f3d437258fa, run-0d978e5039ea, run-73ac35eef2fb, run-e0d136168198, run-445e63a8cb62, run-b84b4205c810, run-b7d4ea68c951 | 0.016367, 0.018107, 0.018606, 0.024994, 0.016386, 0.018801, 0.018667 | 19.49, 18.95, 18.58, 18.6, 18.79, 18.85, 18.67 | 455 | 86.15, 85.18, 84.97, 81.76, 86.30, 84.79, 84.91 |
| 59 | 398eb51 (pushed again in 42de64e, 86b9c7c, 9a96e47 and 96502b4; runs from 86b9c7c, 30c2691 and 96502b4) | Step 56 with one hand-off as before, the review fixes in `main` (`merge` in its own function, named mapping flags, the 213-byte bound from `LABEL` and `MAX_NAME`) and step 58's phase map (replica −0.11% against step 56 with its comments, +1.22% against step 58) | run-fa59eb3110d4, run-e0198f0c5bb3, run-50a831ae6163, run-4c8b8c38042b, run-732cf8332ca6 | 0.021609, 0.016337, 0.016398, 0.016657, 0.01795 | 19.12, 19.53, 19.15, 19.35, 18.43 | 447 | 83.28, 86.22, 86.27, 86.05, 85.45 |
| 60 | 3aa95c6 (pushed again in 475aae6; later runs from 5fda0c0, 475aae6, 2f05cec, 854810e, ffb2e77, aa1fe77, 5216711, 97b4897 and b8b733a) | Step 59 with the source rustfmt-clean (`DIGITS` reflowed) and named patterns for the hour key and the part (replica: four 201-pair runs within ±0.52%, the same speed). Submitted until step 63 | run-4b5fb70e5ea3, run-a3c12461b2f4, run-572c43fab1c2, run-b161edfe5e4e, run-8968e0dda4bc, run-df119fbf8104, run-252c6a84434b, run-4f8350fdbe1a, run-9f79d54ff353, run-689f0017edc4 | 0.016872, 0.016349, 0.018029, 0.017182, 0.016905, 0.018828, 0.016292, 0.016341, 0.019578, 0.016896 | 19.56, 19.26, 19.0, 19.5, 19.18, 18.85, 19.61, 19.01, 19.02, 19.02 | 449 | 85.85, 86.26, 85.26, 85.67, 85.92, 84.82, 86.22, 86.32, 84.36, 85.96 |
| 61 | 3bd65f7 (second run from fabe12e) | Output writes gated on the records given back: an hour is written once they cover the output written, its own bytes and 3/4 of the values buffers' room, the last hours ungated; shorter `parse`, `Interner`, `parse_digits` and `release` (replica −0.69% against step 60, 124 of 201 pairs; expected highest of 5 memory peaks, 40 rounds, 19.49 → 18.50 MB) | run-9564a4f93564, run-e9e5a3117854 | 0.016683, 0.019116 | 18.38, 18.59 | 443 | 86.29, 84.76 |
| 62 | c36861d | A spare endpoint id kept in one line, `add` cold but no longer `#[inline(never)]` (replica −0.64%, and +0.30% in reverse order: the same speed) | run-490cf75dfffd | 0.020743 | 18.49 | 440 | 83.92 |
| 63 | 8a293ff (later runs from 1e8c3f0, 83a938d, 15388b9 and 2b59127) | The main thread is the 12th worker and the last one done with pass 1 merges, so no 13th thread shares the 12 CPUs (the change alone, 300 interleaved runs: runs over +8% 21-43 → 6-10, median −0.9%); every latency of 7 or more digits escapes; one loop runs both slices (replica −1.07% against step 62, 144 of 201 pairs) | run-df6e626d67b9, run-5f20ef221ffb, run-424d169e8d52, run-285034434470, run-219d1a5f89d0 | 0.019571, 0.01964, 0.022344, 0.016107, 0.016017 | 19.18, 18.68, 18.79, 18.55, 18.68 | 445 | 84.35, 84.43, 83.00, 86.61, 86.64 |
| 64 | 695cd44 (second run from fc30062) | The worker defined once in `main`: threads − 1 are spawned and this thread runs the last one, with the worker's machine code unchanged (first run DISTURBED). Its second run had the best Track A points until step 68 | run-a1a3f0c0e9f5, run-1bda764d4b1e | 0.019162, 0.016043 | 18.34, 18.67 | 442 | 84.80, 86.65 |
| 65 | 5719822 (second run from 883c315) | Named structs for the tuple data model (`Part`, `Start`, `Run`): `Part` is `repr(C)`, runs are copied at the loop head and parts built by a small closure, each choice kept for a measured code-generation reason; and 14 review comment fixes (the same code lines as the structs alone; replica equal) | run-1531e5c2ac59, run-aeb5c0425a96 | 0.016109, 0.016208 | 18.49, 18.15 | 462 | 86.50, 86.52 |
| 66 | a09d036 (later runs from 79fb02b, 14aa7c9, f37f273, 9b2ee21, 41e6cfe and 1eb28f0) | A lines pass in `main`: the six shared counters as one array, the room per thread inline, the hour key in one line, `counts` zeroed as each group is taken instead of a 16 KB fill per hour, the scope returning the total (replica −0.04, +0.29, −0.01 and +0.03% over four pairings: equal) | run-b46ef564c46b, run-ce29fb96c3ac, run-64335e22618d, run-ee262fe95b70, run-dd77010a62f8, run-d5416e8b5c12, run-870c44970b40 | 0.016216, 0.020944, 0.02058, 0.016158, 0.016152, 0.016473, 0.016041 | 18.35, 18.65, —, 18.21, 18.48, —, 18.54 | 455 | 86.51, 83.67, —, 86.59, 86.53, 86.31, 86.59 |
| 67 | 12e48b1 (later runs from bbce7e9 and 3305ff6) | jc5-a-2: review comments only: the safety arguments, the phases, and the Dockerfile's note on reproducible builds; the token stream is step 66's (first run slow, with B slow in the same dispatch; the second run's T, M and points from `board.py`) | run-0713194d9d4c, run-44c2757557ea, run-574c80fb3ed7 | 0.020705, 0.01611, 0.017024 | 18.48, 18.32, 18.92 | 455 | 83.83, 86.59, 85.85 |
| 68 | 9f2993f | am5-1: the endpoint table a zeroed static, with no 0.13 ms memset before the first spawn; the cold lookup's hash read from the line and its space mask built from two 64-bit halves; the space window from byte 31 instead of 28, so 6.6% of seed 1's lines end past it instead of 7.6% (replica −1.18% and −1.19% against step 67, 150 and 146 of 201 pairs). The best Track A run and the fastest Track A time until step 69 | run-be74feabcc76 | 0.015722 | 18.44 | 452 | 86.85 |
| 69 | abc7b44 | am5-2: the endpoints' name order sorted after the merge's hand-off, while the other threads start pass 2, and 4 lines cut at the same speed (replica −0.92% and −0.86% against step 68, 141 and 144 of 201 pairs). **The submitted code**. Its first run was slow, with Track A alone slow in that dispatch; its second is the best Track A run | run-a533307c210b, run-b3a4984e4320 | 0.018707, 0.015715 | 18.62, 18.22 | 456 | 84.89, 86.88 |

**The submitted code is step 69 (am5-2, abc7b44), S 456**: step 68 with the endpoints' name order
sorted after the merge's hand-off.
- *Bottleneck.* The merge is serial: the last thread done with pass 1 runs it while the other 11
  sleep on its hand-off. Phase marks on step 67's code put the merge at 0.275 ms, of which the
  endpoint sort and its collect took 0.16 ms (the sort alone 0.14 ms; the timeline is under "What
  each step taught").
- *Design.* `merge` now hands over the hours, the output mapping's address and a slot for each
  hour's length (`Merged`); the output's bound takes the id count from the table. The merging
  thread then unmaps the log, sorts the names (`names`) and publishes them in a second `OnceLock`
  (`order`) while the other threads start pass 2, and gives back the keys after the sort, which
  reads them (+8 lines). An hour waits for the order (`order.wait()`) only after its count and scatter, when
  it builds its groups in name order; its write reads the order too, set by then because the
  hour's groups used it. The step also cuts 4 lines at the same speed: the spawn loop, the two
  `OnceLock`s and the bound are one line each. Steps 57 and 58 handed the hours over first too,
  while the merge ran on a 13th thread, and were not kept (below); since step 63 the merging thread
  is one of the 12 workers.
- *The log's unmap before the sort.* With the sort first (two builds), the log's 0.5 MB of page
  tables were freed 0.16 ms later, into the pass-2 start burst, which is the memory peak in most
  runs ("What we learned about the box"): expected highest of 5 peaks +0.08 and +0.17 MB against
  step 68. With the unmap first, +0.03 and −0.01 MB, at the same speed (+0.20%, 91 of 201 pairs).
- *Why exact.* The order is set once, by one thread, and every reader goes through
  `order.wait()`, so every hour builds its groups from the whole sorted list. It is the same sort
  on the same keys and names: the parse is over, nothing writes the table any more, and the keys
  are released only after the sort. The bound's id count is the length of the list that `names`
  builds (ids 1 to `count`), so the bound is the same number as before. Only the order of the work
  changes, never a byte of the output.
- *Replica* (ab.sh, 201 pairs, two PGO builds a side). Against step 68: −0.92% (141 of 201) and
  −0.86% (144 of 201), and +0.71% in reverse order (step 68 won 52 of 201). The hand-off alone:
  −1.24% and −1.14% (152 of 201 each), and +1.06% in reverse order. The line cuts: +0.05% and
  −0.17% for the spawn loop, +0.12% and −0.01% for the others. Memory, expected highest of 5 peaks over 40 interleaved rounds: 18.47
  and 18.43 MB (two builds) against 18.39 and 18.44 MB for step 68, equal within the noise. At A's
  exchange rates, about +0.10 points of T and −0.03 of S (452 → 456): net about +0.07.
- *Board.* Its first run, run-a533307c210b, took 0.018707 s (18.62 MB, 84.89 points): a slow run.
  Track A alone was slow in that dispatch; B set its best time in the same dispatch
  (run-567ede8911b8, 0.074086 s). Its second, run-b3a4984e4320, took 0.015715 s (18.22 MB, 86.88
  points): the best Track A run and the fastest Track A time.

**Step 68 (am5-1, 9f2993f), S 452**: step 67 with three changes, in cold code and in the
constants of the parse loop.
- *Bottleneck.* Two costs outside the hot loop. A profile (perf, replica) put the cold lookup,
  `Interner::id`, at 9.6 to 11.2% of all cycles for 10.2% of seed 1's lines (7.6% end past the
  space window, 2.6% miss their home slot), about 120 cycles a call. And phase marks showed
  0.13 ms of memset on the main thread before the first spawn: `Interner` is aligned to 32 bytes
  (its keys are `__m256i`), and for an alignment above 16 the standard library's allocator serves
  `Box::new_zeroed` with an allocation and a memset, not a `calloc`.
- *Design.*
  - *Static table.* The table is a zeroed static (`TABLE`, in .bss), so nothing clears it and its
    pages are mapped as the parse first writes them; `parse` and `merge` lose their table
    parameter (−5 lines).
  - *Cold lookup.* Its hash reads the endpoint's first and last 8 bytes from the line, with
    `bzhi` keeping 8 × min(len, 7) + 8 bits, instead of reading them back from the key it had just
    stored as vectors, where each read waited on store forwarding. Its space mask is built from two
    64-bit halves: as one `u128` of four masks it had compiled to `vpinsrd` inserts (+2 lines).
  - *Window.* The 64-byte space window starts at byte 31 instead of 28, so 3 more bytes of
    endpoint end inside it: on seed 1, 6.64% of lines end past it instead of 7.60% (holdout seed
    99: 6.44% against 7.40%). The loop's instructions are the same; only displacements change.
- *Why exact.*
  - *Window.* Byte 27 is the space after the timestamp, and every method has 3 to 7 letters
    (spec), so bytes 28 to 30 are letters and the method's space is at byte 31 to 35: the window
    still starts at or before it, and its first two spaces are still the method's and the
    endpoint's.
  - *Bounds.* The endpoint now ends at byte 32 or later, so a line moves the parse on at least
    38 bytes whatever its bytes (`MIN_LINE`, 35 before), and a slice of n bytes still holds at most
    n / `MIN_LINE` + 1 records. The furthest load now ends 242 bytes after a line's start, at the
    end of the latency's 16 bytes (239 before), within the 256 bytes of `PAD`.
  - *Hash.* For an endpoint under 7 bytes, `bzhi` keeps its bytes and its space; from 7 bytes up,
    all 64 bits, which are then bytes of the endpoint and its space. Those are the key's bytes at
    the same offsets, so the hash, and with it the home slot, is the same as before for every
    length; and the hash only picks where to look.
  - *Table.* All-zero bytes are an empty table, as they were in the zeroed allocation: every field
    is an atomic, a SIMD vector or a null slice pointer of length 0.
- *Replica* (ab.sh, 201 pairs, two PGO builds a side). Against step 67: −1.18% (150 of 201) and
  −1.19% (146 of 201), and +1.16% in reverse order (step 67 won 46 of 201). The table and the cold
  lookup together: −0.59% and −0.63% (+0.89% in reverse); alone, the static table −0.20% and
  −0.13%, the hash from the line −0.32%, the mask from halves −0.18%. The window on top of both: a
  mean of −0.44% over five pairings. Memory, expected highest of 5 peaks over 40 interleaved
  rounds: 18.37 and 18.39 MB (two builds) against 18.35 and 18.51 MB for step 67, equal. At A's
  exchange rates, about +0.15 points (T +0.13, S +0.02).
- *Board.* run-be74feabcc76, 0.015722 s, 18.44 MB, 86.85 points: the best Track A run and the
  fastest Track A time until step 69.

**Step 67 (jc5-a-2, 12e48b1), S 455**: step 66 with the round-5 review comments (the safety
arguments, the phases, the Dockerfile's note on reproducible builds), the same code token for
token. Its board runs took 0.01611 s (run-44c2757557ea, 18.32 MB, 86.59 points), 0.017024 s
(run-574c80fb3ed7, 18.92 MB, 85.85 points) and 0.020705 s (run-0713194d9d4c), in a dispatch where
B was slow too (0.124942 s).

**Step 66 (a-20, a09d036), S 455**: step 63 with three changes that keep its
speed (replica, PGO, 201 pairs, two builds a side).
- *The worker defined once (step 64, a-18).* `main` defines the worker closure before the spawn
  loop, spawns threads − 1 workers and runs the last one on its own thread; the worker's machine
  code is unchanged. Replica +0.20%, −0.32% and +0.01% against step 63: equal. S 445 → 442.
- *Named structs (step 65, a-19).* The parts, run starts and runs are named structs (`Part`,
  `Start`, `Run`) instead of tuples. `Part` is `repr(C)`, a run is copied at the loop head, and the
  parts are built by a small closure: each of those choices keeps the hot loops' code as it was.
  The step also has 14 review comment fixes. Replica +0.13%, +0.09% and +0.00%: equal. S 442 →
  462.
- *A lines pass (step 66, a-20).* The six shared counters are one array; the room per thread is
  inline in its allocation; the hour key is one line; `counts` is zeroed as each group's errors are
  taken, instead of a 16 KB fill per hour; the scope returns the total. The write loop, the parse
  loops, the count loop, the prefix sums and the scatter compile as before; the 109 changed
  instructions are the removed fill, the `take`, spill reloads before cold calls and the returned
  total. Replica −0.04, +0.29, −0.01 and +0.03% over four pairings; memory, expected highest of 5
  peaks over 40 rounds, 18.49 and 18.37 MB (two builds) against 18.38. S 462 → 455.
- *Board.* Step 64 ran 0.016043 s (run-1bda764d4b1e, 18.67 MB, 86.65 points), the best Track A
  points until step 68, and 0.019162 s in a DISTURBED dispatch (C's unchanged code ran 0.028048 s); step 65
  0.016109 and 0.016208 s (run-aeb5c0425a96, 18.15 MB, 86.52 points). Step 66 ran
  0.016041 s (run-870c44970b40, 18.54 MB, 86.59 points), 0.016152 s (run-dd77010a62f8, 18.48 MB,
  86.53 points), 0.016158 s (run-ee262fe95b70, 18.21 MB, 86.59 points), 0.016216 and 0.016473 s
  (run-d5416e8b5c12, 86.31 points, its M not logged), and 0.020944 and 0.02058 s, both with B and C
  normal.

**Step 63 (a-17, 8a293ff), S 445**: step 60 with three changes, each
measured on the replica (ab.sh, 201 pairs unless noted).
- *Writes gated on the records given back (step 61, a-16).* While hours are still being computed,
  an hour is written only once the records given back so far cover the output already written,
  the hour's own bytes and three quarters of what the threads' values buffers can hold; the last
  hours are written ungated. When each hour was written as soon as its start was known, the output
  piled up on the records and the values buffers 2-3 ms into pass 2, at the memory peak. Expected
  highest of five peaks (40 interleaved rounds): 19.49 MB for step 60, 18.50 MB for step 61, which
  also has the line cuts below. That is about −1 MB, or +0.2 points. T −0.69% (124 of 201). The
  variants are in the experiments table.
- *Shorter parse code (steps 61 to 63).* Line cuts in `parse`, `Interner`, `parse_digits` and
  `release` (−13 lines; step 61, with the gate's 7, is S 443); one `SPARE.set` line in `add`
  (step 62, S 440); one loop for both slices; and every latency of 7 or more digits escaped, so
  `parse_digits` returns the marker without a compare (2,188 more escapes on seed 1, 8.7 KB in
  all). Each measured the same speed: means of −0.06% to −0.16% over four 201-pair runs with 2
  builds a side, and for step 62 −0.64%, then +0.30% in reverse order.
- *No 13th thread (step 63).* Until step 62, twelve workers parsed while the main thread waited
  to merge: 13 threads on 12 CPUs. Now the main thread is the 12th worker, and the last one done
  with pass 1 merges. The change alone on step 60's code, three sets of 300 interleaved replica
  runs: runs over 8% above step 60's median 43 → 10, 21 → 6 and 27 → 10; medians 16.85 → 16.71,
  16.93 → 16.77 and 16.87 → 16.73 ms. On step 62's code it measured a mean −0.71% over four
  201-pair runs (2 builds a side), with runs over 1.08× the pair median 13-24 → 2-7 of 201; step
  63 against step 62: −1.07% (144 of 201).
- *Board.* Step 61 ran 0.016683 s (run-9564a4f93564, 18.38 MB, 86.29 points) and 0.019116 s
  (run-e9e5a3117854); step 62 0.020743 s (run-490cf75dfffd); step 63 0.019571 s
  (run-df6e626d67b9, 19.18 MB, 84.35 points), then 0.01964 s, 0.022344 s in a DISTURBED dispatch
  (C's unchanged code ran 0.020852 s), 0.016107 s (run-285034434470, 18.55 MB, 86.61 points), and
  0.016017 s (run-219d1a5f89d0, 18.68 MB, 86.64 points), the fastest Track A time until step 68,
  and the best Track A points until step 64. B and C were normal in step 63's first
  dispatch. In the other two slow dispatches of steps 61 and 62, B (0.094116 s) or C (0.018676 s)
  was slow too, and in step 61's first run A was normal while B and C were slow. So the board has
  shown step 63's speed in two runs of five; the replica shows it, with builds frozen as in the
  final scoring. Our explanation of the board's remaining slow
  runs is untested: A reads 239 MB from DRAM (its parse floor is memory-bound, step 47), and the
  board builds other teams' `target-cpu=native` tracks on the box itself, on its E-cores (CPUs
  12-19, at most two builds at a time), while runs go strictly one at a time (the organisers'
  board README): those builds share the memory and the L3. The final scoring has the box to itself
  (the organisers' FINAL_SCORING.md).

**Where a slow run loses its time** (replica, builds frozen). A probe build of step 66 exits with
the time from `main`'s start until every part is parsed; 150 runs on full seed 1 at 12 CPUs, each
timed exec to exit by the board's timer:

| Runs | Exec to exit | Parse end | Pass 2 and exit |
|---|---|---|---|
| 147 normal | median 16.88 ms | median 10.2 ms | median 6.67 ms |
| 3 slow (over 1.08× the median) | 18.75, 18.99, 20.47 ms | 11.9, 12.2, 13.5 ms | 6.85, 6.79, 6.97 ms |

The slow runs are slow parses, the part whose floor is memory-bound (step 47); pass 2 stays within
0.3 ms of normal. That fits the contention hypothesis above, though it does not prove it: on the
board, A ran slow with B and C normal three times (step 63's 0.019571 s, and step 66's 0.020944 and
0.02058 s, when B ran 0.078623 and 0.080472 s and C 0.017356 and 0.017421 s), and in each of its
other slow runs of steps 61 to 64, B or C was slow too.

**Why a slow run's parse is slow** (replica, builds frozen). Over 300 interleaved pairs, 11 runs of
step 69 and 10 of step 67 were more than 8% over the median, and the 99th percentile was about 20%
over it (19.5 to 20.1 ms, against medians of 16.3 to 16.6 ms). A build of step 68 that prints its
phase marks, run 200 times back to back, had 3 such runs (1.5%), and all three show one signature:
- *The spawns stop partway.* The first 4, 6 or 8 threads start on time and the rest 3.6 to 4.7 ms
  later; in a normal run all 12 start within 0.6 ms of each other.
- *One or two early threads run at half speed.* They take 11.7 to 12.8 ms for their pairs of
  slices, against 8.4 to 9.0 ms normally, as two threads sharing a CPU would, and the parse ends at
  12.6 to 13.8 ms from `main`'s start instead of 10.0. Pass 2 is normal (5.2 ms).
- *Reading.* The signature fits the main thread, which spawns the workers (11 spawns of about
  57 µs, a window of 0.6 to 0.8 ms), losing its CPU for about one scheduler slice partway through.
  A page-cache or huge-page effect would slow every thread alike; here the other threads parse at
  normal speed.
- *Measured, not kept.* A start barrier (every worker spawned before any parses): runs over +8%
  10 → 4 of 300 and the 99th percentile 20.07 → 18.12 ms, but the median +1.7%. A spawn tree (the
  main thread starts 3 threads, each of which starts 3): the median −0.4%, but 34 of 300 runs over
  +8% against 11, and the 90th percentile 18.1 against 16.7 ms. Workers at nice +1, so that a new
  worker is less likely to preempt the main thread while it spawns: +0.16%, and the same tail.
- *Why it matters little for the final.* The final's T is the mean of the middle 3 of 5 runs, so
  one slow run is dropped; it takes two. At the replica's rates (1.5 to 3.7% of runs), two or more
  of five are slow in 0.2 to 1.3% of finals, and then one of the middle three is slow: a third of
  an 8 to 20% loss, so +3 to +7% on that final, an expected cost of about 0.1% of T or less,
  against the barrier's +1.7% in every run.

**Step 60 (a-15, 3aa95c6), S 449**: step 59 with the source rustfmt-clean
and named patterns for the hour key and the part, the same at run time (replica: +0.52%, −0.18%,
−0.23%, and +0.06% in reverse order, over four 201-pair runs). Its ten runs took 0.016292
to 0.019578 s: run-252c6a84434b, 0.016292 s (19.61 MB, 86.22 points), the fastest Track A time
until step 63;
run-4f8350fdbe1a, 0.016341 s (19.01 MB, 86.32 points); run-a3c12461b2f4, 0.016349 s;
run-4b5fb70e5ea3, 0.016872 s; run-689f0017edc4, 0.016896 s; run-8968e0dda4bc, 0.016905 s; run-b161edfe5e4e, 0.017182 s; and
0.018029, 0.018828 and 0.019578 s in slower windows, the last with B and C normal. Steps 56, 59 and 60 cost 21 lines against step 55,
0.16 points of S.

**Step 59 (a-14, 398eb51), S 447**: step 56's engine and hand-off, with the review fixes in `main`
and step 58's map of its phases.
- *Board.* run-e0198f0c5bb3, 0.016337 s, 19.53 MB, 86.22 points; run-50a831ae6163, 0.016398 s,
  19.15 MB, 86.27 points; run-4c8b8c38042b, 0.016657 s; and 0.021609 s (run-fa59eb3110d4), in a dispatch where B ran its fastest time so far (0.088068 s) and C its
  fastest (0.017095 s): Track A's slow runs can come alone. Step 56 ran 0.016437, 0.016698, 0.016896, 0.01678 and 0.016505 s, and
  once 0.020535 s in the same kind of dispatch.
- *Replica.* Step 59 measured −0.11% against step 56 with its comments (104 of 201 pairs): the
  same speed. Steps 53 to 56 each measured within the noise of the step before (+0.20, −0.22,
  −0.11 and +0.18% over 201 pairs, against an A/A control of −0.24%). Steps 53 and 54 were kept
  for their 20 fewer lines, step 56 for its bound, step 59 for its readability.
- *Points.* Step 55's run-ea674ef8448a (0.016399 s, 19.0 MB, S 428) had the best points, 86.44, until step 63.
  Steps 56 and 59 cost 19 lines, 0.14 points of S. The memory peak is the same: the replica's max
  of 5 runs was 19.40 MB for step 59, and over 20 interleaved runs 19.58 MB for step 56.

**Steps 57 and 58 (a-12, a-13), not kept.** They hand pass 2 the hours first and the name order
later ("What each step taught"). The replica measured step 57 1.07% faster than step 56 with its
comments (129 of 201 pairs) and 0.99% faster than step 56 (132 of 201), and step 58 the same
speed as step 57 (+0.06%, 100 of 201). On the board, eight of their twelve runs took 0.018107 to
0.024994 s, three of them in DISTURBED dispatches; the other four took 0.016367 to 0.01712 s. Step 56 ran slow in one of its six runs, step 59 in one of its five. Twelve runs
cannot separate a 1% effect from the box's slow runs, but they gave no sign of the replica's gain,
and step 59 kept the one hand-off that step 56 ran with. In the alternating dispatches of 16:29 to
16:55 UTC, after a slow window in which both ran slow (0.021609 and 0.024994 s), step 59 ran
0.016337, 0.016398 and 0.016657 s (mean 0.01646 s) and step 58 0.016386 and 0.018801 s, with
0.016367 and 0.018107 s before (mean of the four 0.01742 s).

**Step 51 (a-08, dad7f0f, S 448)** is the engine. It was built by the Dockerfile of step 37 (loop
alignment on), and it has four board runs:
- run-746bb553b4c4, 0.023882 s, in a dispatch where every track was slow: Track C's c-07 ran
  0.021546 s there, against 0.019026 s in its other run, and B's unchanged code 0.104119 s;
- run-a5488d26b2e6, 0.019369 s;
- run-f5d0484af6d5, 0.016954 s;
- run-cab6a2cefe9b, 0.016307 s, 19.5 MB, 86.24 points.

After its first two runs, both slower than step 50's best (0.016502 s), it was dropped. The
replica then measured it faster than step 50 in every session (ab.sh: one run of each binary per
pair, in alternating order, the median of the paired differences):

| Replica A/B | Pairs | Medians (ms) | Paired median | Pairs won by step 51 |
|---|---|---|---|---|
| step 50 → step 51 | 31 | 17.188 → 16.915 | −0.205 ms (−1.19%) | 23 |
| the same, each run in a fresh cgroup v2 (memory.max 8G) | 31 | 17.143 → 16.983 | −0.245 ms (−1.43%) | 21 |
| step 50 → step 51, another session | 41 | | −0.58% | 27 |
| step 51 → step 50, the same in reverse order | 41 | | step 50 +1.81% | 30 |
| step 48 → step 51 | 31 | 17.106 → 16.705 | −0.429 ms (−2.51%) | 24 |
| A/A control: step 51 against itself | 31; 201 | | +0.66%; −0.24% | 12; 111 |

- **Plain runs** (benchl.sh, 30 runs each): step 50, median 17.26 ms (mean 17.38); six slices per
  thread, 17.01 (17.24); step 51, 16.87 (16.99).
- **The A/A control** says that on this track a 31-pair difference under about 0.7% is noise. So
  step 51 is about 1.2 to 1.4% faster than step 50, and 2.5% faster than step 48, not more.
- **Memory**, max of 5 runs (mem.sh), four sets each: step 50 19.34, 19.05, 19.88 and 19.24 MB;
  step 51 19.98, 19.22, 19.45 and 19.73 MB. The peaks of one binary range over about 18.0 to
  20.0 MB from run to run, so these sets do not tell the two apart.

**Step 41's code** (the code-quality pass, 1806066) ran the five times in its row. It ran twice
more when probes were removed: run-ef2688505938, 0.018768 s (0c3a5e2), and run-d687cf63103b,
0.019182 s (eea4759). Of the 14 scored runs of that code from 10:10 to 12:07 UTC, 9 took 0.0196 to
0.0266 s and 5 took 0.0187 to 0.0192 s. Step 40's code, the same engine, ran 0.018743, 0.018716
and 0.018676 s (step 40's row). It also ran twice in slower dispatches: run-37da6cecde40, 0.020268
s, 84.91 points (45c369c, where our log calls B's run "likely slow dispatch"), and
run-56976cca74a9, 0.021599 s (d576688).

Same-day A/B of step 39 against step 37 (runs.log 08:10Z, one push each, alternating): step 39
run-f864a17b46cc 0.019000 s and run-5e361abae820 0.018979 s; step 37 run-110ec4581099 0.019041 s,
run-82ba58583726 0.019097 s and run-87a54d4680e2 0.020012 s, then run-5d9829328e5e 0.023404 s
(runs.log 08:12Z). Slow-dispatch samples of step 39: run-4d01e82b4cda 0.019555 s,
run-1ec0f81604f0 0.022575 s, run-4a1feadd518a 0.023573 s and run-79ff2bdff829 0.022788 s, in
dispatches where B's unchanged code ran 0.1209-0.1312 s.

## What each step taught

- **Step 1.** The input is mapped, not read: pages the harness pre-warmed are charged to the host,
  so the 239 MB file costs nothing on the memory axis, where `read()` into one buffer would cost
  239 MB.
- **Step 2** bought less than its 100 extra lines suggested (0.0796 → 0.0749 s): the parse was not
  yet the bottleneck, the serial glue between the passes was. Step 3 made the glue parallel and
  took a third off T (0.0749 → 0.0510 s).
- **Step 6.** Global histograms and a scatter over a 12 MB array did not scale with threads
  (1 thread 195 ms, 12 threads 60 ms on the server). Grouping by hour first makes every later pass
  touch one hour's data, which fits in L1/L2.
- **Step 8** is the first memory step: records went from 8 to 4 bytes, and nothing record-sized is
  alive when the 10 MB output is produced. M 44.66 → 24.3 MB, +3.8 points with T.
- **Step 9.** There were 2.2 branch mispredictions per line, from the method `match` (GET 46%,
  POST 37%) and a coin-flip test on the endpoint length (49% are shorter than 32 bytes). One
  64-byte window of space bits answers both with `tzcnt`. One thread: cycles −27%, mispredictions
  6.9M → 2.6M.
- **Step 10** only won once the test server wrote to tmpfs, as the box does: on a disk-backed
  directory the same change measured 1.3 ms slower, on tmpfs 1.6 ms faster (pwrite calls
  serialise on the file's lock; page faults on a shared mapping do not).
- **Step 11** shows the limits of the test server: 36.2 → 29.9 ms there (released pages were
  faulting back in), and on the box its one run was 4% slower (0.0367 s against 0.03528). It was
  kept because the copy is what makes releasing the pages safe: removing it later (9e6ed95) gave
  0.0342 and 0.0332 s against 0.0292–0.0307 with it (experiments table).
- **Step 12 came from measuring the box itself.** For a few runs the program exited right after a
  phase with the elapsed time as its exit code (0.25 ms units then). They split A on the box:
  parse 17.5 ms, merge 1.25 ms, pass 2 about 8 ms, write about 3.5 ms, then 4–5 ms of drops and
  exit after the write. Exiting right after the write took 0.0367 → 0.03066 s.
- **Step 13.** A profile showed 12% of one thread's cycles in libc's `memcmp`: `==` on the 64-byte
  key array compiled to a library call per line. An XOR fold over the eight words is inline.
- **Step 14** cut instructions per parsed line from 333 to 199 on one thread and the test server's
  time from 36.6 to 24.8 ms, yet its first board run was no faster than step 13b (0.02959 s). The
  second run of the same code gave 0.02257 s: one board run can hide a real step as easily as it
  can fake one.
- **Steps 16–17.** Reading through 128 KB buffers was 0.9 ms faster on the test server (19.3 →
  18.4 ms) and slower on the box in two runs (0.0236 and 0.0244 s against 0.02188). Page faults
  cost much more in the test VM than on the box, so a win that comes from avoiding faults has to be
  confirmed on the box.
- **Step 18.** One thread: cycles −7% on the server. Its one board run, 0.0227 s against step 15's
  0.02188, was read as noise (our log); step 19 built on it and ran 0.020053 and 0.019785 s.
- **Step 19.** With hours taken in order and written in order, a thread that finished a small hour
  waited for the large hour before it. Publishing the length first and writing later took pass 2
  from 6.9 to 5.9 ms on the server, and gave the first board run under 20 ms.
- **Step 20** is worth almost a point with no change in speed: 659 → 503 lines took the board from
  82.76 to 83.62 points at the same T and M. The first run of it, 0.026108 s, was one of the box's
  outliers.
- **Steps 21–23** went out within half an hour, one board run each. Step 21 cut 12-thread user
  cycles by 3% on the server: endpoints whose space is past byte 92 (7.6% of lines) took further
  32-byte windows instead of a byte loop. Step 22 removed the rank table and its dependent load
  per record. Steps 21 and 22 ran slower than step 20 (0.0224 and 0.0237 s), and step 23, which
  contains both, set the best time then (0.019691 s). One run each cannot prove that steps 21 and
  22 were neutral on their own.
- **Steps 24–25 (reverted).** Dynamic parse blocks and 4% fewer parse instructions both came out at
  the same board time as step 23 (0.019777 and 0.019916 s against 0.019691). Neither the balance
  between the fixed chunks nor the instruction count limits the 12-thread parse on the box. Each
  was reverted for its extra lines (+13 and +10). Step 32 later showed why: the parse sits close
  to the memory floor. Step 38 brought step 24 back for the tail, not the median.
- **Steps 26–27** cut 504 → 327 lines (+1.07 points at the same T and M: 83.73 → 84.80, then 84.68
  in a slower run). The cut put everything in one file: the endpoint table allocated zeroed and
  probed by one function, every endpoint's bytes copied when it is added, all runs sorted by
  (hour, slice) and cut into hours, and an hour's start summed from the published lengths. Speed was checked against step 23 at every stage, over three builds of each
  version, because the multi-threaded PGO training gave every build a different profile: three
  builds of the unchanged step-23 source spread 0.5 ms on the server.
- **Step 28** fixed that build-to-build spread at its cause. The instrumented training run has 12
  threads, and plain profile counters lose updates when threads increment them at once. Atomic
  counters stop the lost updates. Builds still differ a little: counts that depend on timing (the
  endpoint table's insert race, the merge's sort, the flush) vary from run to run, so two builds of
  one source can lay out blocks differently (17 to 120 worker instructions in step 65's builds),
  which is why every replica A/B uses two builds a side. A first attempt, training on one CPU with
  `taskset -c 0` (368aa64), broke every track's board build: CPU 0 is not in the build
  container's CPU set. It was reverted in 387ebb3. The two board runs after step 28 were the best
  A times at that point (0.019331 and 0.019387 s).
- **Step 29** costs nothing measurable (0.019612 s, S unchanged) and closes a real hole: 68 of 300
  mutated logs used to segfault. Malformed input now stops with a message.
- **Step 30** removed a thread exit and join (about 0.25 ms) and a serial spawn of 12 threads
  (0.25–0.5 ms) between the passes: −0.4 to −0.65 ms on the server (31 interleaved runs, two PGO
  builds). Its one board run, 0.020152 s, was not faster than step 29. It went to the board again
  inside steps 31 and 33.
- **Step 31** is the second memory step. Where the peak was (instrumented run on the server): mid
  pass 2, records about 5–6 MB, the output 4–5 MB, the scratch arrays 2.5–3 MB and the pending
  statistics 0.3 MB, above a 4.7 MB empty-input floor. The changes went after what could shrink:
  - The parse threads drop the log's pages, but its page tables stayed (about 0.47 MB, charged to
    us); one `munmap` after the parse frees them.
  - Pending statistics hold 20 bytes per endpoint instead of 40: no name slice, and the rate is
    computed when printed.
  - The scratch array is allocated once, for the largest hour, instead of growing.
  - In time order, a large hour started last in its block held up the writing of the small hours
    after it, so up to 30 hours of statistics waited at once. Largest first in each block, a large
    hour ends with its block.

  Docker `memory.peak` on the server, 21 interleaved runs: 21.02 → 18.21–18.38 MB median, and
  20.94–21.06 → 20.34–20.46 ms. On the box: M 21.01 MB (step 26) → 18.24 MB, 85.16 points.
- **Step 32.** The probes split the box's time after step 31: parse end 11.0 ms, floor 7.8 ms. So
  the parse code costs about 3.2 ms over the memory floor, and about 8.8 ms of the 19.8 ms run
  came after the parse. Faster parse code has little left to win; the time after the parse is the
  larger lever. The first two probe builds (2deb384, 073fb0a) failed the board's build: the
  Dockerfile's PGO training run executes the binary, and the probe's nonzero exit stopped the
  build. The second pair exits early only on inputs over 100 MB, so the training run on the sample
  exits 0.
- **Step 33.** With 16-byte keys, 600 of the 1,842 endpoints tied and needed a name compare. The
  sort took 0.20 ms of the 0.27 ms serial merge on the server, about 0.1 ms with 32-byte keys.
  The log's unmap (about 0.1 ms) moved from the serial path to the main thread, which is idle
  during pass 2. Server: −0.13 ms. Box: 0.018935 s, the best Track A run then, 85.60 points.
- **Steps 34 and 36.** Loop alignment is a Dockerfile flag, meant to make the layout of the hot
  loops depend less on where unrelated code lands. A's own runs do not show that it helps: the
  clean runs with it are 0.019217 and 0.019471 s, against 0.018935 s for step 33 without it, all
  within the box's spread. C set its best run with the flag and keeps it. The two runs in disturbed
  dispatches, run-3feab32d2b8f (0.026024 s, DISTURBED) and run-69d0c9f98c0e (0.026627 s, SLOW BOX),
  came with the other tracks' unchanged code running slow in the same dispatches. Step 36 built A without the flag again, as a board A/B test: 0.0271 s against 0.0192-0.0195 s for the flagged builds in normal dispatches, so the flag is back on from step 37. A's box times are bimodal (about 0.019 s or 0.022-0.027 s for the same code), which a single run cannot separate from a flag effect.
- **Step 35 (reverted).** −0.70 ms on the test server, not faster on the box in two runs: see the
  next section and the experiments table. The second run was in a dispatch our log calls "a bit
  slow".
- **Step 37** cut setup and merge from 352 to 337 lines at the same speed on the test server
  (21.04 / 21.10 against 21.17 / 21.11 ms, 2 PGO builds each, output on disk for this A/B).
- **Step 38** found the likely cause of A's bimodal box times. With one fixed slice per thread, the
  scheduler sometimes puts two of the 12 threads on one CPU at spawn, and they stay there for the
  whole parse (busy CPUs rebalance rarely, and no CPU goes idle before the parse ends). That pair
  runs at half speed, and the merge waits for it.
  - Server probe, dev build, 30 runs: 4 runs ended the parse on 11 distinct CPUs, with a parse
    spread of 4.9-8.0 ms and a run of 18.1-20.8 ms, against 14.6-15.6 ms for the rest.
  - Server probe, PGO, 40 runs (exit code = 100 × stacked + spread / 0.25 ms): 6 stacked runs
    read 121-131 at 22.5-24.5 ms; normal runs read 2-8 at 18.0-19.6 ms.
  - Fix: slices from a shared counter, so a slowed thread just parses fewer of them. Server, 100
    interleaved runs, 2 PGO builds each:

    | | median | max | runs above median + 1.5 ms |
    |---|---|---|---|
    | before (step 37) | 18.79 / 18.69 ms | 25.98 / 24.79 ms | 12 / 6 |
    | after (step 38) | 18.78 / 18.73 ms | 21.69 / 21.99 ms | 4 / 8 |

  - One buffer pair per thread, not per slice: with a pair per slice (two `mmap` calls each),
    8 slices per thread cost 2.3 ms (dev build). Pinning each thread to its own CPU made the tail
    far worse: half of 40 runs had a 2-10 ms parse spread, because a pinned thread cannot leave a
    busy CPU. Slices per thread (dev build, 30 runs, median run end): 1: 15.18 ms; 4: 14.92 ms;
    8: 15.39 ms.
  - Step 24 measured only the median, where this is invisible. On the box the slow mode still
    appears in some dispatches (step 39's slow samples), so the fix is not proven there.
- **Step 39.** perf on the server (10 runs, step 38) showed `madvise` at 14.9% of all cycles and
  IPIs at 5.2% sent plus 3.6% received (TLB shootdowns), and `sched_yield` at 3.0%. With parts a
  48th of the log, a release per part frees records about as early as a release per long run, with
  142 fewer calls. A futex wait (`OnceLock::wait` on the lengths it needs) replaces the yield loop,
  which spun on the hyperthread siblings of threads still computing. Server, 61 runs: 19.34 / 19.48
  → 18.86 / 18.84 ms; M 18.08 → 18.39 MB (records of long runs now wait for their part). On the box
  it came out level with step 37 in the same-day A/B above.
- **Step 40.** A fork/exec/wait timer split the server run (25 runs, means): exec to `main` 0.59 ms;
  `main` 14.70 → 14.91 ms (+0.2 ms for the barrier and the zap); `main`'s end to the exit seen by
  the parent 1.12 → 0.61 ms. A serial zap of the same range on the main thread took 0.63 ms, so the
  exit spent about 0.6 ms on the output's 2,500 page-table entries. Server medians, 2 PGO builds,
  three repetitions (61, 81 and 61 runs): 18.45 / 18.43 → 18.00 / 18.13, 18.14 / 18.08 → 17.79 /
  17.75 and 18.13 / 18.08 → 17.78 / 17.72 ms. M 18.53 → 18.52 MB. On the box, its three normal runs
  (0.018676-0.018743 s) were the fastest A times until step 41's 0.018636 s, the same engine. This is apost-1's fourth item (step 35), whose
  two box runs came before the stacked-thread mode was known.
- **Step 41** changes no instruction in the hot loops (parse, `parse_digits`, pass 2, writer),
  checked with `objdump` on deterministic builds. Two forms of a checked `OnceLock::set` (with
  `expect` or an `assert!`) were tried and dropped: each changed the instructions of the parse
  loop, which shares a function with the slice loop that sets the part. Test server, 2 Dockerfile
  builds each, 61 interleaved rounds: 19.95 / 19.75 against 19.78 / 19.77 ms (medians), within the
  build-to-build noise. On the box it ran 0.018636 to 0.021146 s in five runs, the same two modes
  as step 40.
- **Steps 42-43: the box rewards a uop cut that the AMD server barely shows.**
  - *Where the old loop's work went.* The endpoint lookup: masking the key with `KEEP`, folding
    64 bytes into a hash, then two dependent table loads, about 34 uops a line. On the test
    server, removing the lookup outright (id = length, wrong output) took the 12-thread parse end
    from about 9.2 to 7.65 ms.
  - *The fix.* The new lookup hashes what is already in registers: the length and the two 8-byte
    ends, then compares the line once against the stored 128-byte key (endpoint, space, zeros).
    Latencies of up to 6 digits (98.8% of lines) are converted where they lie, and escapes are
    pushed only on the cold path. One thread on the test server: parse instructions 354M → 253M
    (−29%), cycles 170M → 119M.
  - *Two cursors need the lean loop.* On the old loop they were worse at 12 threads (9.53 against
    9.15 ms). Test-server parse end with the new loop: the lean loop alone −0.36 ms, the cursors
    alone +0.38 ms, both together −0.89 to −1.07 ms.
  - *Whole program.* Test server −0.38 ms (19.65 / 19.45 → 19.29 / 19.06 ms, 41 runs). Intel
    laptop −14% (30.14 → 25.99 ms, built for alderlake).
  - *The box.* Its probes read the parse end at 13.8 ms on step 41's code and 10.1 ms on the new
    parse. The 13.8 is probably high: the same probe read 11.0 ms on step 31's code (step 32), and
    the whole runs moved less, 0.018084 s for step 43 against 0.018636 s for step 41's best run.
  - *The cost.* +73 lines (S 372 → 445).
- **Step 44: counting ranks paid once the parse was lean.** 95% of the groups have 64 values or
  fewer. The value at rank r is the largest value with at most r values below it. So for a group
  of up to 64 values, one vector compare per
  value and vector counts the values below every lane, with no branch on the values. An earlier
  version for groups of up to 32 (step 40 work, experiments table) gained 0.21-0.26 ms on the
  server for 22-25 lines, about 0 points. On the new parse, groups of up to 64 took the test server
  from 17.37 / 17.58 to 16.94 / 16.81 ms. The box's six runs (0.016848 to 0.018635 s) were all
  faster than step 43's two (0.018084, 0.019052 s). +32 lines (S 445 → 477).
- **Step 45 isolated the parse.** It ran the same percentiles on the old parse, pushed in turn with
  step 44: 0.017936, 0.022111 and 0.021109 s, against step 44's 0.016848 to 0.018635 s in the same
  alternation. The old parse kept its slow runs (9 of 14 since 10:10 UTC, above). The new one had
  none in its first eight runs (steps 43 and 44: 0.016848 to 0.019052 s). Later steps on the new
  parse had slow runs again (0.023882 s in step 51's row), in dispatches where other tracks were
  slow too. So the board alone cannot say how much of the slow mode the new parse removed.
- **Step 46: no huge pages for the output.** Exit code 13 = 1 + 24 × 0 + 4 × 3 + 0:
  - anonymous THP is `always`, so turning it off with `prctl` matters for a stable peak;
  - shmem THP is `never`, and `/out` has no `huge=` option.

  So the output, on tmpfs, cannot get huge pages. Its 2,500 page faults (step 40 work) stay, and
  `MADV_HUGEPAGE` on the output mapping would do nothing.
- **Step 47: reading the log is memory-bound.** Exit code 105: with 6 threads, touching one byte per
  64 of the log took only 5% longer than with 12. Twice the threads buy 5%, so the floor of the
  parse (7.8 ms, step 32) is set by memory, not by the cores. The new parse, at 10.1 ms, is close
  to it, and most of what remains comes after the parse.
- **Step 48** is the line count: −18 lines at the same speed on the test server (41 rounds:
  17.03 / 17.03 → 17.15 / 17.08 ms; instructions 447.7M → 444.5M). The parse's setup and teardown
  were tidied, with one `Merged` hand-off that carries the endpoint order; the error rate is
  computed once in pass 2, so the pending statistics hold six numbers, 24 bytes per endpoint; and
  the rank reduction is split. Its two runs, 0.017041 and 0.016836 s, were the best Track A times
  then (85.97 points).
- **Step 49: a fast path that earns its lines.** Without the 6-digit fast path, every latency
  goes through the shuffle-based parser: +28M instructions (+9 per line) and 17.07 / 17.07 →
  17.41 / 17.38 ms on the test server, and on the replica +3.76% (6 of 31 pairs won), about
  +0.6 ms. At A's exchange rates (−1% T = +0.109 points, −10 lines =
  +0.072 at S 448), 27 lines are worth about 0.19 points and 3.8% of T about 0.41.
- **Step 50: a store-forwarding stall per line.** Step 41's writer built each line backwards in a
  stack buffer and then copied it into the mapping. The copy's wide loads read bytes that had just
  been stored one or two at a time, and such a load waits until those stores reach the cache.
  Writing each line from its end straight into place removes the buffer and the copy. The replica
  measured the same speed (+0.61, −0.81 and −0.70% in three sessions), so the step was kept for
  its 11 fewer lines. Its first board run was the best Track A run then (0.016502 s, 86.20).
- **Step 51: fewer slices, less per-slice work.** Each slice costs a cut search, a part with its
  run list, and one `madvise` release of its pages. Each `madvise` is a round of TLB-shootdown
  interrupts to the other threads. Step 38 chose four slices per thread so that a slowed thread
  could not hold up the merge. With pairs parsed side by side (step 43), four pairs meant eight
  slices a thread.
  - Test server, 61 interleaved runs, 2 builds each: four pairs 16.96 / 16.93 ms, three pairs
    16.78 / 16.71, two pairs 16.55 / 16.73.
  - Replica: −1.2 to −1.4% (above).
  - Board: the first two runs (0.023882, 0.019369 s) were slower than step 50's best, and the
    next two (0.016954, 0.016307 s) were the best Track A runs so far. Board runs could not tell a
    1.2% change from the board's spread; the replica could.
- **Step 52: fewer spills, no gain.** Two changes:
  - the per-line step as a macro (a closure of that size was not inlined once the long way was);
  - the cold lookup inlined.

  Together they removed every call from the parse loop and 5 of its 11 stack references:
  +9.6M instructions and −2.5% cycles on the test server, at the same time there (17.02 / 16.82
  against 16.94 / 17.02 ms). The replica measured −0.37% (16 of 31 pairs won), so it was not
  worth its 11 lines.
- **Steps 53-55: lines at the same speed.** At 201 pairs the replica resolves about 0.3%, and the
  three steps measured +0.20, −0.22 and −0.11% (97, 108 and 104 of 201 pairs won). They cut 20
  lines (S 448 → 428), worth about 0.15 points at A's rate.
  - *Step 53.* The 7- to 10-digit latencies (1.2% of lines) are parsed digit by digit up to the LF
    by a cold, out-of-line `parse_digits` that pushes the escape itself; inlined into the loop's
    cold branch it cost +0.29%. The digit weights are a literal table instead of a const generator.
  - *Step 54.* Numbers are written in digit pairs while 100 or more, then the last one or two
    digits as one pair; the space before a number overwrites a lone digit's leading 0, so odd
    lengths need no special case.
  - *An old workaround can expire.* The inline-asm `vpmovmskb` had been needed since step 9,
    because LLVM rebuilt the mask bit by bit. In step 53's code the plain intrinsic compiles to the
    same hot loop (registers aside, objdump), and the binary has no `vpinsrb`.

- **Step 56: the id space as a bound.** Until step 55, a thread whose insert lost the
  compare-and-swap threw its id away, so the README had to call the 205 spare ids "a measured
  margin, not a bound" (at most 13 lost in 100 runs, measured before the pair parse). Now the
  thread keeps that id for its next insert, so at most one id per thread is ever unpublished: at
  12 threads, 1,842 endpoints need at most 1,854 of the 2,047 ids, and any thread count up to 205
  fits. `add` is cold and out of line, so the hot loop does not change; the replica read +0.18%
  (94 of 201 pairs), within the noise. It costs 8 lines (S 428 → 436), with the `LABEL` constant
  that names the line's 21-byte prefix.
- **Step 57 (not kept): hand the hours over first.** The merge used to publish everything at once: the runs
  cut into hours, the endpoints' name order and the output mapping. Pass 2 needs only the hours to
  start counting and selecting, so step 57 hands those over about 30 µs after the last part comes
  in (the runs are gathered by hour while the parts arrive), and the name order and the mapping
  next, while pass 2 places the first hours' values. The statistics are kept in endpoint-id
  order: an hour's length needs the names but not their order, so only the write waits for the
  sort. An earlier try at sorting on the main thread after the hand-off (apost-1 work,
  "Experiments") lost 0.3 ms on the test server; there the statistics still needed the name order.
  The step also takes review fixes in `main`: the merge in its own functions (`merge`,
  `name_order`), named constants (`PROT_WRITE`, `MAP_SHARED`, `ERROR_SHIFT`), named tuple fields
  instead of `.0` and `.1`, and the 213-byte line bound written from `LABEL` and `MAX_NAME`. On
  the replica the fixes alone measured −0.04% (102 of 201 pairs, 11 lines), the hand-off on top of
  them −0.90% (128 of 201, 8 lines), and step 57 as merged −1.07% against step 56 with its comments
  (129 of 201), for 19 lines (S 436 → 455), and −0.81% and −0.93% on fresh seeds 105 and 113.
  The memory peak did not change: over 20 interleaved runs each, a mean of 18.75 MB and a
  max of 19.94 MB with the hand-off, against 18.94 and 19.58 MB without it. On the board it ran
  slow in eight of twelve runs (above). A likely cause, not measured: every write step started by
  waiting for the name order, so when the merge's thread, the 13th on 12 CPUs, was descheduled
  behind the workers, a worker that finished an hour slept on that wait and woke late. On the
  replica the order was always ready about 0.1 ms before the first write, so the wait never
  showed.
- **Step 58: a map of the phases.** Comments only: `main` starts with its six phases in order (map
  the log; the pool; pass 1; the merge; pass 2; teardown), and each phase starts with its number.
  The replica read +0.06% (100 of 201 pairs). The map was kept in step 59. Two splits into more functions were measured and not
  taken: the hour's write (+0.39%, the mean of four 201-pair runs) and the log's mapping (+0.54%,
  90 of 201), 3 lines each. The write is inlined either way, but the code shape moves the
  profile-guided layout: two rebuilds of the same source differ by only −0.02% and +0.06%.
- **Step 59: one hand-off, with the review fixes.** Step 56's merge, as one function (`merge`),
  with the named mapping flags, `ERROR_SHIFT`, the 213-byte bound from `LABEL` and `MAX_NAME`, the
  SAFETY wording, and step 58's map of the phases. S 447 (step 56: 436). Replica: −0.11% against
  step 56 with its comments (104 of 201), +1.22% against step 58 (75 of 201).
- **Step 60: formatting and two names.** The source is now rustfmt-clean (the digit tables
  reflowed, free in S), and the hour key and the part are named by patterns instead of `.0.0` and
  `.1` (+2 S). Replica: +0.52%, −0.18%, −0.23%, and +0.06% in reverse order, over four 201-pair
  runs: the same speed. S 447 → 449.
- **Steps 64 to 66: `main` in fewer lines and named structs, the hot code unchanged.** The worker is
  defined once; the tuples that `merge` and pass 2 read as `.0` and `.1` are named structs; and a
  lines pass folds six counters into one array. Each choice was checked against the machine code
  of the hot loops, since moving the worker or its data changes the register allocation of the
  whole loop: splitting the worker into `#[inline(always)]` functions measured +0.64% and +0.21%
  ("Experiments"). Each step measured the same speed as the one before. S 445 → 455.
- **Steps 68 and 69: time outside the hot loop.** A profile found the first cost, phase marks the
  other two. Cold code that runs for one line in ten took a tenth of the cycles, about 120 a call;
  part of that was a wait on store forwarding, as each call read back bytes it had just stored as
  vectors, and part a `u128` built from four masks, which compiled to vector inserts. A zeroed allocation of a type aligned to more than 16
  bytes is a memset, not a `calloc`, while a zeroed static costs nothing until it is touched. And
  a sort that every other thread waited for moved after the hand-off, because no hour needs the
  name order before its count and scatter are done.

Pass 2 per thread (replica, profile and phase timers, steps 56-57's code): counting 0.41 ms,
placing the values 1.11 ms (bound by L1 store misses: 7.8 cycles per record per thread),
statistics 2.09 ms, writing 1.20 ms.

Where step 67's run goes (replica, phase marks in ms from `main`'s start, medians of 7 runs, each
with a fresh output directory):
- the endpoint table's memset, 0.13 ms, before the first spawn (gone in step 68);
- 11 spawns of about 57 µs each: the workers start between 0.34 and 1.10 ms;
- the parse ends at 9.12 to 9.48 ms;
- the serial merge takes 0.275 ms: the endpoint sort 138 µs (after the hand-off since step 69),
  the sort of the 1,101 runs 70 µs, the collects 46 µs;
- pass 2 ends at 14.39 to 14.89 ms (the threads run out of hours within 0.5 ms of each other), the
  barrier at 14.95 ms, and the output's page-table teardown ends at 15.05 ms.

Pass 2 is 56.4 thread-ms: counting 5.3, the scatter and the release of parts 12.9, the statistics
(the selections, `small_ranks` and `line_len`) 25.1, and writing 13.1. That is 4.7 ms a thread
against about 4.9 ms of wall time, so the threads are balanced to about 0.3 ms; the tail is the
last block of hours.


Where pass 2's time goes on the replica (steps 54 to 56's code; builds that skip one part, wrong
output, timing only; 201 pairs each against the full program):

| Part skipped | Time saved |
|---|---|
| The selections of the groups of more than 64 values | 1.15 ms, of which p50's selection 0.70 |
| Writing the lines | 1.10 ms: output page faults and stores 0.76, formatting 0.35 |
| Rank counting for the groups of up to 64 values (`small_ranks`) | 0.75 ms |
| The scatter of each hour's values | 0.47 ms |

Where the server time goes (step 39, PGO, 41 interleaved runs, medians; probes, not candidates):

| Run | Median |
|---|---|
| Full run | 18.0-18.2 ms |
| Exit right after the merge (startup, parse and merge) | 13.3 ms |
| Percentiles made free | −1.26 ms (about 2/3 of it in the 115K groups of 32 values or fewer) |
| No output writes at all | −2.07 ms (formatting about 0.5 ms; the rest is 2,500 tmpfs page faults, 5.6 µs each with 12 threads, 2.3 µs alone) |
| No release of the parsed log (the exit zaps 58K page-table entries on one core) | +8.5 ms |

Pass 2 cannot be hidden inside the parse: all 12 CPUs are busy there, so only less total work
helps.

## What we learned about the box

The box is the board's scoring machine; the test server is a 16-vCPU VM. What held on Track A
(Tracks B and C have their own evidence):

- **Cutting work the box waits on pays; cutting work it does not wait on does not.** Branch-free
  field ends (step 9: 0.0377 → 0.0361 s), the inline key compare (step 13), 333 → 199
  instructions per line (step 14, 0.02257 s on its second run) and a shorter serial merge (step
  33: 0.018935 s) all moved the box. 4% fewer parse instructions (step 25: 0.019916 against
  0.019691 s) and dynamic parse blocks (step 24: 0.019777 s) did not, and the probes of step 32 say
  why: the 12-thread parse ends at 11.0 ms against a 7.8 ms memory floor, so it waits on memory,
  not on instructions.
- **State and teardown cost real time.** Freeing the tables one at a time cost 4-5 ms on the box
  (step 12: exiting right after the write took 0.0367 → 0.03066 s). 8-byte records alive during the
  write held M at 44.66 MB; 4-byte records freed before the write gave 24.3 MB (step 8).
- **Kernel-side gains on the test server need a board run.** In the VM, page faults, the mmap lock
  and TLB-shootdown interrupts cost much more than on the box:

  | Change | Test server | Box |
  |---|---|---|
  | Names copied before release (step 11) | 36.2 → 29.9 ms | 0.0367 s against 0.03528, 4% slower |
  | pread through 128 KB buffers (step 16) | −0.9 ms | 0.0236 and 0.0244 s against 0.02188 |
  | apost-1: sort in worker 0, no `munmap` of the scratch, parallel output teardown (step 35) | −0.70 ms (−3.8%) | 0.019312 and 0.021607 s against 0.018935 and 0.019217 |

  Changes that remove plain CPU work from the critical path (steps 9, 13, 14, 19, 33) moved both
  machines the same way.
- **Settings tuned on the test server can be wrong on the box.** Four slices per thread (step 38)
  removed the stacked-thread tail on the server (slowest of 100 runs 25.98 → 21.69 ms), but on the
  box three of step 41's five runs are still in the slow mode. Loop alignment fixed a layout gap for
  Track B on the server and shows nothing on A's box runs (steps 34 and 36).
- **The box's CPU rewards uop cuts far more than the AMD server.** The new parse of step 43 cut 29%
  of the parse's instructions. The test server ran 0.38 ms faster. The Intel laptop ran 14% faster,
  and the box's parse-end probe moved 3.7 ms (13.8 → 10.1 ms, probably less in fact, see step 43).
  From Sun 13:20 UTC such questions went to a replica with the box's own CPU ("How we measured").
- **A's box times were bimodal with the old parse**: about 0.0187 s, or 0.0196 to 0.027 s, for the
  same binary (9 of step 41's 14 runs from 10:10 UTC were slow). Since then the board has also shown
  slow runs of the new parse (0.023882 s in step 51), in dispatches where the other tracks were slow
  too. Other teams' builds share the box's L3 and memory while we run ("How we measured"). One board
  run cannot tell a 3% change from that, so decisions used alternating dispatches, and from step 49
  on the replica.

- **The parse is core-bound, though its floor is memory-bound.** A build of step 60's code
  (the submitted code when this was measured) that prints its phase ends, run on the replica 15 times each way, interleaved: with one thread
  per core (6 threads, on CPUs 0, 2, 4, 6, 8 and 10) the parse ended at 12.62 ms against 9.63 ms
  with 12 threads (medians; minima 12.40 and 9.47), 31% later, and the rest of the run took 6.97
  against 5.86 ms, 19% longer. The touch-only probe of step 47 lost only 5% with half the
  threads. So the floor is set by memory, and the parse's time above it by the cores. An earlier
  whole-run measurement agrees: 28% longer (21.64 against 16.93 ms, medians of 21 runs), with a
  memory peak 2 MB lower (max 17.35 against 19.39 MB).
- **Where the memory peak is** (replica, step 51's engine, 8 runs). A sampler on an idle CPU read
  the run's cgroup `memory.current` every 2 µs or so, against phase marks printed by the binary:
  - *The parse.* Memory rises linearly through it, to 16.5-16.95 MB at its end (9.6-10.2 ms): the
    records, 12.34 MB, and about 2.2 MB of fixed costs (the endpoint table, 12 stacks, the heap,
    and the log's page tables, 0.68 MB).
  - *The merge.* The log's unmap and the key release give back 0.7 MB.
  - *The peak.* 18.16 to 19.31 MB, 2 to 3 ms into pass 2, while the 12 largest hours (the daytime
    ones, in the first block) are written and the next block starts. At that point about 8.2 MB of
    records are still alive (32 of 48 parts not yet released), with 2.2 MB of output, about 3 MB of
    values buffers and about 1 MB of pending statistics.
  - *The exit.* About 15.3 MB, 10.1 MB of it the output.
- **The peak as a model** (replica, the memory sampler and 20 interleaved mem.sh runs per variant).
  M = (L + O) + (V + S) + F + stock:
  - *L*, the records alive: 12.4 MB at the parse's end;
  - *O*, the output written: 10.1 MB at the end (tmpfs, charged);
  - *V*, pass 2's values buffers: about 3 MB, as each of the 12 threads keeps the pages up to the
    largest hour it did;
  - *S*, the statistics of hours computed but not yet written: 20 to 22 hours of 44 KB at the peak;
  - *F*, fixed: about 1.5 to 2.2 MB (the endpoint table, 12 stacks of 32 KB, the heap and its
    arenas, and the log's page tables, 0.68 MB until the merge);
  - *stock*, the kernel's per-CPU charge batches (below): 0 to 3 MB, about 1.5 on average.

  L + O hardly moves (12.4 MB, about 11.6 at the peak, 10.1 at the end): the output replaces the
  records at 81% of their size. The peak, 2 to 3 ms into pass 2 after the 12 large daytime hours,
  is about 3.5 MB above the end state (about 15.3 MB) because V + S ride on top of L + O there.
  So freeing records earlier only lowers L + O by what the early hours' output does not replace:
  about 1.2 MB even if half the hours ran during the parse. The variants that tried are in the
  experiments table, from "Values pages given back after each hour" down.
- **M has its own noise: the kernel's per-CPU charge stock.** cgroup v2 charges memory to a
  cgroup's counter in batches of 64 pages per CPU, and keeps the unused rest of a batch in a
  per-CPU stock; `memory.current` and `memory.peak` include it.
  - *Microbenchmark* (replica): 12 threads pinned to 12 CPUs, each touching K fresh pages. K = 100
    and K = 120 gave the same 6.84 MB plateau, K = 64 gave 6.31 MB, and K = 128 jumped to 9.46 MB
    (12 × 192 pages).
  - *So* every CPU that charges anything carries 0 to 63 pages that nothing uses: 0 to 3 MB at 12
    CPUs, about 1.5 MB on average, different every run.
  - *Consequence.* This explains much of the spread of one binary's M from run to run (18.2 to
    19.4 MB here), and why a smaller peak in one run proves little. The final scoring takes the
    largest of five peaks.
  - *At the peak* (replica, step 68's code, 10 runs: a sampler on `memory.current` with
    `memory.stat` snapshots). Of each peak, `memory.stat` accounts for 14.7 to 15.8 MB; the other
    2.3 to 3.1 MB is the stock. A peak comes right after a round of batch charges.
  - *Its ceiling.* Without any stock, a run's peak would be its largest live total (anonymous,
    file and kernel memory) in any snapshot: 1.85 MB below the measured peak over the 10 runs of
    step 68's code, 2.02 MB over 6 runs of step 67's. After the start of pass 2 the gap stays at a
    median of 1.5 to 1.9 MB a run (at most 2.1 to 3.2 MB), from the output writers' charges.
  - *Why A keeps it.* The stock goes only if no worker's CPU charges a single page: any charge
    leaves a remainder of 0 to 63 pages, and a CPU's remainder depends only on its total charges
    mod 64, so no partial scheme helps. One CPU would have to fault in every page: the records
    (12.4 MB, about 3,030 pages, at places known only as the parse goes, since each pair's records
    start after the previous pair's), the values buffers (about 3 MB), the output (10.1 MB of tmpfs
    pages, about 2,470, faulted by each hour's writer; on the board `/out` is a bind mount of a
    directory on the host's tmpfs, so they are always charged), the stacks, the arenas and the page
    tables. That is about 6,200 faults, 3 to 5 ms of work on one CPU, with no idle CPU to hide it
    in: +5% T or more (−0.55 points) against at most +0.45 points of M. Not built.
- **Where the peak is now** (replica, step 68's code, the same 10 runs). In 7 of 10 runs the peak
  is the start of pass 2, 0.5 to 0.8 ms after the hand-off, when the first hours' scatters fault
  in each thread's values buffer at about the same time. Without that burst, each run's peak would
  be the rest of pass 2: the highest of the 10 runs 18.17 → 17.99 MB, the mean −0.15 MB. Not
  built. Resident at the end of the second block of hours (smaps): records 8.69 MB, values buffers
  2.98, the arenas (the pending statistics) about 1.0, output 1.15, stacks 0.41, heap 0.21, the
  endpoint table 0.10 MB.

## Box probes (failed runs on the board, by design)

The board reports only T, M and S of a correct run, and the exit code of a failed one. So a
temporary build can report a phase time by exiting with it. Each probe was removed in Track A's
next commit and shows on the board as "✗ correctness". They are measurements, not failures of the
solution. A probe exits early only on inputs over 100 MB, so the Dockerfile's PGO training run (on
a sample) exits 0 and the build succeeds:

| Run ID | Probe | Exit code | Reading |
|---|---|---|---|
| run-7d4218a4cdba | d8f31b6, step 31's code: exit when every part is in (0.1 ms units) | 110 | parse done at 11.0 ms |
| run-be516aa463a5 | 1261c8d: the parse loop replaced by one byte touched per 64 | 78 | memory floor 7.8 ms |
| run-bbf6e7ffe2bd | 145795b, step 41's code: exit when every part is in | 138 | parse done at 13.8 ms (probably high, see step 43) |
| run-500e61325035 | 8bcfa4c, step 43's new parse: the same | 101 | parse done at 10.1 ms |
| run-b06ce137b1e9 | d992dda: 1 + 24 × anonymous THP mode + 4 × shmem THP mode + `/out`'s `huge=` option | 13 | anonymous THP `always`, shmem THP `never`, no `huge=` on `/out` |
| run-c08713cbca4a | efe48f0: 100 × (6-thread time) / (12-thread time) of a one-byte-per-64 touch of the log | 105 | twice the threads buy 5%: memory-bound |

The probe of step 46 decodes as 13 = 1 + 24 × 0 + 4 × 3 + 0 (mode lists in d992dda: anonymous
`always`, `madvise`, `never`; shmem `always`, `within_size`, `advise`, `never`, `deny`, `force`).
The parse-end probes of steps 32 and 42 ran in different dispatches, so they carry the board's
noise between dispatches.

The earlier probes behind step 12 used 0.25 ms units; their run IDs are not in our log.

## Experiments that did not make it (measured, reverted)

| Experiment | Result |
|---|---|
| apost-1 (f8206db, reverted in 51d3674): the endpoint sort moved from the merge into worker 0 (serial merge 250 → 115 µs on the server); runs gathered as each part comes in; the scratch arrays released with `MADV_DONTNEED` and leaked instead of unmapped (their `munmap` took the mmap write lock during the final writes); after a barrier, every thread drops its share of the output mapping's page-table entries (exit 1.07 → 0.57 ms) | Server: −0.70 ms median (−3.8%), 41 interleaved runs, two PGO builds each; memory unchanged. Box: run-7833033c46e9 0.019312 s and run-4e7cded1b902 0.021607 s (a slightly slow dispatch), against run-6cca232c0f6a 0.018935 s and run-df8ecf640b7e 0.019217 s for the code of steps 33 and 34. Not faster on the box, +8 lines: reverted |
| Sorting the endpoints on the main thread after publishing the merge (apost-1 work) | Server: pass 2 +0.3 ms; the main thread is a 13th thread and got preempted |
| Dropping output page-table entries while other threads still wrote (apost-1 work) | Slowed the remaining writes by as much as it saved |
| Global largest-first hour order (apost-1 work) | Server +0.9 ms: all writes bunch at the end. Blocks of 2 × threads: +0.1 ms |
| `sort_by_cached_key` for the endpoint sort | 160 against 105 µs on the server |
| `munmap` of each slice in its own parse thread (step 31 work) | Server +7.5 ms: the threads queue on the mmap lock |
| `M_ARENA_MAX=1` (step 31 work) | −0.75 MB, +1.1 ms (457 futex calls on the arena lock) |
| Any thread writes any ready hour (step 31 work) | +0.4 MB: the output is written earlier |
| Releasing scratch pages past the current hour (step 31 work) | −0.6 to −1.2 MB, +0.4 ms (+1.8%): break-even |
| 10 pass-2 threads instead of 12 (step 31 work) | −0.75 MB and +0.25 ms on top of the other changes; with largest-first hours no further gain, +0.5 ms |
| `MADV_POPULATE_WRITE` per output hour (step 30 work) | +0.5 ms on the server |
| `MADV_POPULATE_WRITE` of each thread's expected record pages (step 33 work) | +2.2 ms on the server |
| pwrite of the output in 64 KB chunks (step 33 work) | +0.67 ms on the server: the tmpfs inode lock |
| `MADV_DONTNEED` of each written hour's output pages (step 33 work) | +0.37 ms on the server |
| Endpoint keys stored by slot, to cut one dependent load (step 33 work) | +0.6 ms on the server |
| Threads spawned as a tree; all workers spawned before any parse starts | No gain on the server |
| PGO training pinned to one CPU with `taskset -c 0` (368aa64) | The board's build failed on every track (CPU 0 is not in its build container's set): reverted in 387ebb3, replaced by atomic counters (step 28) |
| Interner inlined into the parse loop | 2.4% faster on one thread, nothing at 12 threads |
| Releasing the input's pages in pass 2 instead of right after parsing | +1.6 ms: the TLB shootdowns interrupt the other pass-2 threads |
| Not releasing them at all | +1.3 ms: the exit then unmaps 58,000 pages on one core |
| `MADV_POPULATE_READ` on each thread's chunk before parsing | 36.4 → 49 ms |
| Groups up to 16 / 64 sorted instead of 32 | 32 was the best of the three (server, one thread) |
| LF found from a 128-byte window, so the next line does not wait for the endpoint | +29 instructions per line, same cycles (one thread), no gain at 12 threads |
| Software prefetch 1, 4 or 16 KB ahead of the parse | No gain |
| Log-linear histogram selection for groups of 256+, AVX2 rank counting for groups of up to 32 | 13M fewer cycles on one thread, nothing at 12 threads; not worth 120 lines |
| Removing the endpoint-name copy before the pages are released (9e6ed95) | Box run-87490db73ccd 0.0342 s and a second run at 0.0332 s, against 0.0292–0.0307 with it: restored in 27cf731 (run-f90c7c73ef1b, 0.02924 s) |
| Three nested selections read at the end | Wrong output: each selection reorders the previous rank's slot. Each value is now read right after its own selection |
| Parse blocks from a shared counter, four per thread (step 24) | run-dee36d077800, 0.019777 s against 0.019691 s, +13 lines: reverted in e6e4d9d. Reinstated in step 38 for the tail, not the median: it removes the stacked-thread runs (slowest of 100 server runs 25.98 / 24.79 → 21.69 / 21.99 ms) |
| Fewer instructions per parsed line (step 25) | run-b68029ac7816, 0.019916 s against 0.019691 s, +10 lines: reverted in 1c8d136 |
| Pass 2 in two phases: every thread computes its hours, waits at a barrier, then writes them | +2 ms on the server. The write phase alone took 2.7 ms: twelve threads faulting in the shared output at once most likely serialise in the kernel |
| Endpoint table entries as one `OnceLock<(key, name)>` per id | Parse +0.4 ms at 12 threads: the lock's state word sits on a second cache line, read on every lookup |
| Each line written straight into the output from its known length, without a stack buffer | +14M instructions on one thread (552M → 566M): the length is then computed twice per line |
| The first digit of every number written as a pair from the table, and the hour label copied as a slice of unknown length | +15M instructions on one thread (540M → 555M) |
| Only long runs released, not a slice's remaining records once no hour reads them | Memory peak +0.7 MB (20.04 → 20.76 MB, 15 runs each) |
| Branch-free AVX2 rank counting for groups of up to 8 values, 63% of groups (step 40 work) | Server −0.10 / −0.03 ms for +27 lines. Checked against `sort_unstable` on 10M random groups (ties, 0, `u32::MAX`, every rank). In place, those groups went from 26-28 to 22-23 M TSC cycles |
| Counting ranks for all groups of up to 32 values, up to four vectors (step 40 work) | Server −0.21 to −0.26 ms (a compact form −0.19 / −0.12 ms) for +22 to +25 lines: about 0 points net at the line rate. Microbenchmark on the groups' real sizes: 58 → 27 ns a group. Checked on 5M groups. Taken up later, for groups of up to 64 values on the new parse (step 44) |
| Counting-sort buckets in name order, the order the statistics read them (step 40 work) | Server 18.16 / 18.20 against 18.15 / 18.09 ms: neutral, so small groups do not miss the cache on their values |
| Output written by `pwrite` from a private 64 KB buffer instead of stores into the mapping (step 40 work) | Server +1.4 ms (19.53 / 19.66 against 18.09 / 18.27). perf: `pwrite` 18.8% of cycles, 17.2% waiting on the tmpfs inode rwsem; the copy itself 2.4% |
| `fallocate` of the output during the parse | Not run: it charges about 10 MB to the memory peak (about −1.9 points), so it would need about −3 ms on the box |
| `MADV_POPULATE_WRITE` of each hour's output range before formatting (step 40 work) | Server +0.7 ms (18.65 / 18.73 against 17.96 / 17.89) |
| Output prefaulted by the idle main thread (step 40 work) | Serial before pass 2: +5.7 ms; concurrent with pass 2: +0.3 ms |
| Pool started as a tree, each new thread starting two more (step 40 work) | Server +0.1 ms median and a worse tail: spawned threads land next to their parent |
| Guided parse slices, each a 2 × threads-th of what is left and at least 1 MB, about 78 slices (step 40 work) | Server 18.18 / 18.18 against 18.08 / 18.08 ms over 100 runs; tail not better |
| Threads pinned to CPUs; 8 slices per thread; a buffer pair per slice (step 38 work) | See step 38 |
| `values` kept mapped to the exit; the log not unmapped (step 39 work) | Server 18.28 / 18.31 and 18.37 / 18.27 against 18.21 / 18.17 ms: neutral or worse |
| The parsed log released every 2 MB during the parse instead of once per slice (step 39 work) | Release 1.0 → 0.1 ms, but parse +0.5 ms; PGO A/B neutral |
| Memory variants on step 37: values released on shrink, a shared values pool, long runs released in 64 KB pieces, the 12 smallest hours first, no per-part reader count | Peak unchanged or worse: 18.23, 18.65, 18.24, 18.59 MB and +0.7 MB, against 18.24-18.31. The peak sits at the start of pass 2: the parse plateau is 17.0 MB (probe: exit after the merge), plus about 1.2 MB of values |
| Every latency through one digit parser (step 49) | Replica +0.644 ms (+3.76%), won 6 of 31 pairs; board 0.018935 and 0.019025 s against step 48's 0.017041 and 0.016836. The 27 lines it saves are worth less than the time |
| The per-line step as a macro, the cold lookup inlined (step 52) | Replica −0.37% (16 of 31 pairs), within the A/A noise; +11 lines |
| Three pairs of slices per thread instead of two (step 51 work) | Test server 16.78 / 16.71 against 16.55 / 16.73 ms (61 runs, 2 builds each); replica benchl.sh medians 17.01 against 16.87 ms (30 runs each) |
| Two cursors on the old parse loop (step 43 work) | One thread −21% cycles, but 12 threads slower: parse end 9.53 against 9.15 ms on the test server. The cursors pay only with the lean loop |
| Three cursors on the new loop (step 43 work) | Parse end 9.29 ms on the test server, slower than two: register spills |
| Exact percentiles for all groups by one AVX2 multi-rank quickselect with sorting-network leaves | Test server −0.53 ms median (−3.0%, 41 rounds, 2 builds each), memory +1.0 MB at 12 CPUs, +164 lines: net negative. Compact forms kept about 45% of the gain for +68 lines, or most of it for +144. 400,000 groups checked against a full sort |
| One writer thread writing each hour in order with `write()`, no output mapping (the same work) | Test server +5.1 ms: the 13th thread gets no CPU until the workers finish, then 74 writes take 5.7 ms |
| SWAR 8-digit number formatting (the same work) | Byte-identical but slower: 5.2 against 3.6 ms for all 127K lines on one thread; the pair loop is cheap for numbers of 1 to 6 digits |
| Percentiles by a radix sort of each hour's values | Not built: our estimate was +2.9 MB of peak memory, about −0.6 points at A's rate (−0.5 MB = +0.11) |
| Three cursors (three slices side by side, 6 slices per thread; step 53 work) | Replica −0.25% (112 of 201 pairs), within the noise, for +9 lines |
| Counting ranks for groups of up to 96 or 128 values instead of 64 (step 53 work) | Replica −0.08% and −0.11% (103 and 108 of 201 pairs): no gain |
| The long-latency parser inlined into the loop's cold branch (step 53 work) | Replica +0.29% (87 of 201 pairs); out of line, −0.03% |
| Timing only, wrong output: the selections of groups over 64 values skipped (step 55 work) | Replica −6.88% (−1.15 ms, 185 of 201 pairs): `select_nth_unstable` on the 5.9K groups of more than 64 values, 63% of the values, is the largest block left after the parse |
| Values pages given back after each hour, `MADV_DONTNEED` (memory work) | −1.0 MB but +2.07% T (201 pairs): the pages fault back in at about 1 µs each, and each `madvise` costs about 15 µs with 12 threads |
| The same, and the long runs' record pages given back as soon as they are placed | −1.23 MB, +4.03% T |
| The values buffer's tail given back only when the next hour needs under half, or under 0.8, of it | −0.45 MB with +0.66% T; −0.5 MB with +1.28% T |
| Hours queued in blocks of 1, 4, 6 or 24 instead of one per thread | Memory worse: 19.06-19.51 against 18.84 MB (means of 20 runs) |
| A per-thread pool that reuses the statistics vectors | Memory worse, 18.93 against 18.63 MB: glibc's arena already reuses the freed chunks |
| One thread per core (6 threads) | −2 MB of peak, +28% T for the whole run; the parse +31% |
| The endpoint sort with cached keys; a stable sort of the runs | The first slower (its key vector faults in on the main thread); the second −8 µs |
| The name order built by the first worker that needs it, not by the main thread (step 57 work) | Replica −0.35%: less than the main thread's version gained |
| The hour's write as its own function; the log's mapping as its own function (step 58 work) | Replica +0.39% (mean of four 201-pair runs) and +0.54% (90 of 201), 3 lines each |
| Workers spinning (`pause`) on the merge's hand-off instead of sleeping on the futex (step 58 work) | Replica +0.01% and −0.28%: noise, for 6 more lines |
| `MADV_POPULATE_WRITE` of each hour's output range before writing it (step 58 work) | Replica +3.30% (won 40 of 201 pairs), and 3.41% slower again with the pair order reversed: one call costs more than the hour's page faults (about 35) |
| Steps 57-58's hand-off without the wait: a write step skips until the name order is ready, and the last one computes it if needed (step 59 work) | Replica −0.70% against step 59, for 11 more lines and more synchronisation to trust; not on the board |
| A radix select for the groups of more than 64 values | Replica +1.67% and +1.69% in the full program |
| An AVX2 quickselect for the groups of more than 64 values | 45-85% slower than the chained `select_nth_unstable` in a microbenchmark |
| Groups of 65 to 128, or 65 to 256, values sorted instead of selected | Replica +0.83% and +1.96% |
| Bracketed sample selection for the groups of more than 64 values | Replica +0.54% / +2.98% |
| Output writes deferred to the end of pass 2 (step 61 work) | Memory peak −1.00 MB against step 60 (expected highest of 5; the kept gate −0.84 MB), T +1.90% |
| The same, and runs of 4,096 or more records given back right after their scatter | −1.35 MB, +2.84%; the long-run release alone −0.18 MB and +2.18% (16,384 or more: −0.35 MB, +0.68%) |
| The write gate with all of the values buffers' room, or half of it, instead of three quarters (step 61 work) | All: −0.78 to −1.20 MB, +0.86% and +0.95%; half: −0.29 MB, +0.01% |
| The all-room gate with blocks of 6 or 4 hours instead of one hour per thread; blocks of 6 without a gate | −1.21 and −1.39 MB, but +2.03% and +2.54%; without the gate +0.54 MB |
| Non-temporal stores for the records (step 61 work) | Replica +0.71% (80 of 201 pairs): a record page is freshly zeroed by its fault, so its lines are already in the cache and there is no read-for-ownership to save |
| The worker split into `#[inline(always)]` functions with `# Safety` sections, +14 S (step 64 work) | Replica +0.64% (78 of 201) and +0.21% (89 of 201): registers move |
| Line cuts that changed code: the hour's write loop as a fold; the three selections through one closure (step 66 work) | Not kept: the fold changed the write loop's instructions (an index-based reverse walk, `bswap` instead of `movbe`), and the closure became a function of its own |
| A miss in `find` calling `id` itself (step 61 work, −3 lines) | Replica +0.75% (77 of 201) and +0.08% (95 of 201) |
| Digits without a weight table: shifted to the end of the low 8 bytes (`vpsllvq`), then fixed weights (step 63 work, −9 lines; exact on all 1,111,111 strings of 0 to 6 digits) | Replica +0.80%, +1.71%, +0.83% and +0.66% (mean +1.0%): the table loads fold into `maddubs` and `madd`, and the shift costs about 4 more uops a line |
| Constants swept on step 63 (rows below; replica, ab.sh 101 pairs, then 201 for anything under −0.4%) | Nothing beat the current settings; on step 62 the same knobs gave the same picture |
| Slices: 2 × threads, 6 × threads or 8 × threads instead of 4 × | −0.24%; −0.58% at 101 pairs, then +0.34% at 201; +1.43% (on step 62: 6 × +0.18%, 8 × +1.54%) |
| Pass 2's hour blocks of threads / 2 or 2 × threads hours, instead of one hour a thread | +0.88% and +0.86% (on step 62: +0.94% and +1.65%) |
| `small_ranks` with more vector widths: 16 values (V = 2); 16 and 24; 16 and 48; every step of 8 up to 64 | +0.26% (it had screened −0.79% on step 62); −0.12%; +0.23%; −0.59% at 101 pairs, then +0.20% at 201 |
| `small_ranks` up to 48 values, or up to 96 (V = 12), instead of 64 | +0.35%; +0.04% |
| The write gate holding 1/2 or 7/8 of the values buffers' room instead of 3/4 | 1/2: T +0.03%, max of 10 runs 19.05 against 18.69 MB; 7/8: T +0.17%, max 18.67 against 18.80 MB over 40 runs, equal medians (on step 62: −0.06% and +0.77%) |
| Track C's CPU claim (a thread that finds another of ours on its CPU moves) on step 60 (step 63 work) | At each worker's start: slow runs (over +8%) 27 → 56 pinned, 40 → 61 moved then released, median +0% to +0.7%; at pass 2 only: 36 → 24, median +0.24%; on top of step 63's change: 6 → 12. Not kept |
| A start barrier: every worker spawned before any parse starts (step 68 work) | Replica +1.15% (50 of 201 pairs), and +2.34% with the static table. Over 300 interleaved pairs, runs over +8% of the median 10 → 4 and the 99th percentile 20.07 → 18.12 ms, but the median +1.7% ("Why a slow run's parse is slow") |
| A spawn tree: the main thread starts 3 threads, each of which starts 3 (step 69 work) | Median −0.4%, but 34 of 300 runs over +8% of the median against 11, and the 90th percentile 18.1 against 16.7 ms |
| Workers at nice +1, so that a new worker is less likely to preempt the main thread while it spawns (step 69 work) | Replica +0.16%, and no change in the slow-run tail |
| `M_ARENA_MAX` 2 or 1, set by `mallopt`, +3 lines (step 68 work) | 2: expected highest of 5 peaks −0.30 MB (their standard deviation 0.32 → 0.18 MB), T +0.63%; 1: −0.11 MB, T +3.45%, as every thread's allocations then fault in one heap mapping. About 0 points net |
| The endpoint sort on cached keys: (key, name, id) tuples sorted in place, +2 lines (step 68 work) | Replica −0.04% and +0.36%: no gain |
| 16 × `MAX_ENDPOINTS` slots instead of 8, 64 KB more (step 69 work) | Replica −0.07% and −0.07%: the same speed |
| The last two blocks of hours queued as one, largest first, so that the run ends on small hours, +2 lines (step 69 work) | Replica +0.70% and +0.80%, and −0.78% in reverse order against step 69: slower. As with a global largest-first order, the big hours' writes then wait for the small hours before them and bunch at the end |
| The name sort before the log's `munmap` instead of after it (step 69 work) | The same speed, but expected highest of 5 peaks +0.08 and +0.17 MB against step 68, where the kept order reads +0.03 and −0.01 MB: the log's page tables were freed into the pass-2 start burst (step 69) |
| 3-byte records that keep the 11-bit id, with 12 bits of latency | Not built: on seed 1, 26.9% of latencies fit 12 bits (65.6% fit 14, 85.4% 16 and 98.8% 20), so 73% of lines would escape |
| Records grouped by (part, hour within the part, endpoint) during the parse, 3 bytes each with 0.1% escapes | Not built. Our estimate: 9.3 MB of records and about 0.9 MB of sparse bucket directory, −2.2 MB at the merge and about −1.7 MB at the peak (+0.4 points), against a counting sort per part in the core-bound parse (+6 to 12 thread-ms, −0.3 to −0.7 points) and +60 to 80 lines (−0.45 to −0.6 points): net negative |
| An exact two-level, variable-length latency code | Not built: a latency carries about 12 bits of entropy even given its endpoint, and variable-length records make the count and scatter loops a dependent chain (our estimate: about +1 ms) |
| Hours computed during the parse, so that not every record is held at once | Not possible in spec: the disorder is unbounded, so no hour is complete before the last part is parsed |
| The pending statistics in 20 bytes instead of 24 | Not built: about −0.15 MB |
| Lines looked at and not cut (step 69 work): `map` inlined, the shape of `DIGITS`, the `File` options, `Part`'s literal | Not cut. Inlining `map` would save 4 lines but leave two raw `mmap` calls in its place |

## Lessons

- **Only the board decides kernel-side changes.** Steps 11, 16 and 35 all won on the test server
  and not on the box. CPU-side changes (fewer branches, fewer instructions on the critical path,
  less serial work) transferred.
- **Measure the box itself.** Exit-code probes found the 4–5 ms of teardown (step 12) and showed
  that the parse is within about 3 ms of the memory floor (step 32), which moved the work to what
  comes after the parse.
- **One run is not a result.** The same code has scored 0.02959 and 0.02257 s (step 14), and
  0.019217 and 0.026627 s (step 34). Where a decision rested on one run, the table says so.
- **Make the build repeatable before comparing builds.** Until step 28, two builds of one source
  could differ by 0.5 ms, more than most of the steps being measured.
- **Lines are points.** Steps 20 and 26–27 gained 0.86 and 1.07 points with no change in speed.
  Steps 26–27 cut 177 lines for 1.07 points, about 0.006 points per line.
- **A copy of the scoring machine settles what the board cannot.** Step 51 was dropped on two
  board runs and restored by the replica's paired runs. Its next two board runs were the best
  Track A runs. Steps 49 and 52 were settled the same way, at differences of 0.4 to 3.8%.
- **Know the noise floor first.** The replica's A/A control (step 51 against itself) read +0.66% at
  31 pairs and −0.24% at 201 pairs. A 31-pair result under about 0.7% is therefore read as "the
  same", as for steps 50 and 52.
- **The same change can be small on one CPU and large on another.** Step 43's 29% fewer parse
  instructions were −2% on the AMD test server and −14% on an Intel laptop, and the box (Intel)
  followed the laptop.
- **A 13th thread costs the tail, not the median.** Steps 57 and 58, 1.07% faster on the replica,
  depended on the merge's thread, the 13th on 12 CPUs, finishing the name order in time, and they
  ran slow in eight of twelve board runs. Step 63 took that thread away: on the replica, runs more
  than 8% over the median went from 21-43 to 6-10 of 300, while the median moved 0.9%. The replica
  does show such effects, but only if you count the slow runs instead of reading the median.
- **Bound a lever before building it.** The kernel's charge stock (2.3 to 3.1 MB of each peak) and
  the pass-2 start burst (the peak in 7 of 10 runs) looked like the largest memory levers left.
  The sampler's data bounded both first: removing the stock needs every charge on one CPU, about
  −0.55 points of T for at most +0.45 of M, and removing the burst lowers the mean peak by 0.15 MB.
  Neither was built ("What we learned about the box").

## How we measured

Four tools, from the coarsest to the finest.

1. **The board.** One run per push on the scoring box, an i5-13500.
   - *What shares the box.* Our program runs on P-cores 0-11 (2.5 GHz, turbo off). Meanwhile other
     teams' Docker builds run on E-cores 12-19 of the same chip: a track built with
     `target-cpu=native`, as ours are, is built on the box, two builds at a time
     (`board/README.md`). The builds share the L3 cache and memory with our run, and they come and
     go.
   - *The spread this gives.* One binary's board times spread a lot. Step 51's binary ran
     0.016307 to 0.023882 s in four runs; step 52's binary 0.016601 to 0.021073 s in eight.
   - *Bursts.* A slow run in one track does not always come with slow runs in the other tracks of
     the same dispatch. At 13:58 UTC Track A ran 0.021073 s while Track C ran its normal time: each
     track's run lasts milliseconds, at a different moment.
   - *Use.* So board runs decide only large changes, and only in alternating dispatches.
2. **The test server**, a 16-vCPU AMD VM: interleaved runs of two PGO builds of each version, with
   medians. It is good for instruction counts and CPU-side changes. It misleads on kernel-side
   changes (page faults, TLB shootdowns: "What we learned about the box") and on uop cuts, which
   the box's Intel cores reward several times more (step 43).
3. **Exit-code probes on the box** ("Box probes"): a temporary build exits with a phase time.
   Probes measure what the board does not report. But two probes in two dispatches still carry the
   board's noise between them (step 42).
4. **The replica**, from Sun 13:20 UTC: a rented server with the box's CPU, set up as the harness
   sets up the box.
   - *CPUs.* P-cores 0-11 at 2.5 GHz with turbo off (the same sibling pairs), E-cores 12-19.
     Transparent huge pages `always`, as the box's probe read (step 46). Output to tmpfs, like
     `/out`.
   - *Timing.* The harness's own `sd-timer` (`harness/timer/sd-timer.c`: fork, exec and wait, timed
     with `CLOCK_MONOTONIC`), so T is exec to exit, as on the board.
   - *Builds.* Each binary is built by the track's Dockerfile `RUN` line, in a chroot of the
     official image, on the E-cores. From about 14:25 UTC every build is frozen (cgroup freezer)
     while a measurement runs, as the final scoring has the box to itself. Earlier replica numbers
     were taken with builds possibly running on the E-cores, as on the board.
   - *`ab.sh N old new`.* N pairs, one run of each binary per pair, in alternating order (old new,
     new old, ...), each run under a bench lock. It reports both medians, the median of the paired
     differences and how many pairs the new binary won.
   - *Other tools.* `benchl.sh` for plain series, and `mem.sh` for the cgroup v2 `memory.peak` of
     each run (we quote the max of 5, as the final scoring takes the largest of five peaks).
   - *A caveat found late* (step 68 work). `ab.sh` and `benchl.sh` write every run's output to the
     same directory, so each run first truncates the previous run's 10 MB summary.txt when it opens
     it: 1.2 ms on the main thread before any spawn. The board gives each run a fresh `/out`.
     Paired differences are unaffected, but those replica times are about 1.2 ms high, so the same
     gain in ms is about 7% larger as a share of the board's T (−1.00% on the replica is about
     −1.07%). Step 67's timeline ("What each step taught") was taken with a fresh directory per
     run.

   Calibration against the board, on the binaries of 13:05 UTC, 21 runs each on the replica:

   | Track and binary | Replica median (min, mean) | Board runs of the same binary |
   |---|---|---|
   | A, step 48 | 17.17 ms (16.72, 17.59) | 16.836 and 17.041 ms (2 runs) |
   | B, Track B's step 49 | 96.92 ms (96.28, 97.05) | 97.889 to 113.384 ms, median 101.022 (15 runs) |
   | C, Track C's step 39 | 19.24 ms (18.98, 19.24) | 19.031 to 21.787 ms, median 19.321 (9 runs) |

   - *Memory.* Over 5 runs (mem.sh): A 18.37-19.30 MB, B 37.06-37.95, C 9.41-10.78. The board's
     runs of the same binaries read 18.80-19.09, 36.06-37.96 and 10.10-10.72 MB.
   - *Offsets.* On B and C the replica's median is at or below the board's fastest runs. On A it is
     1-2% above the two board runs of that binary. A/B decisions use paired differences, where an
     offset like that cancels.
   - *Noise, A/A* (a binary against itself): Track A's step 51 read +0.66% at 31 pairs (12 won)
     and −0.24% at 201 pairs (111 won); B's step-49 binary read +0.14% at 15 pairs (6 won). So on
     Track A a 31-pair difference under about 0.7% is not read as a result, and from step 53 on
     A/Bs use 201 pairs, where differences under about 0.3% count as the same.
   - *Rule.* A change is kept only if the replica shows it faster than that noise, or equal with
     fewer lines or less memory. Board samples then confirm it.

## How the final number was verified

**The gate**, before every push, runs the binary that this directory's Dockerfile builds
(`docker build`, then `docker create` and `docker cp` of `/app/solution`), so the PGO build the
board makes is the one tested. Since step 43 the gate script checks, on the laptop:

- full seed 1, sha256 495ca09682b9… (the spec's), at `taskset -c 0`, `0-2`, `0-6` and `0-7` (1, 3,
  7 and 8 CPUs);
- sample seeds 2-4 and holdout seed 99, byte-equal to the reference output;
- the empty input, which gives an empty summary.txt;
- S, by the harness's `static_axes.py`.

On top of that:
- **Step 43, the new parse**, also passed:
  - the seed-1 hash at 1, 3, 12 and 16 CPUs on the test server, and sample seed 1
    (ca6311bbeb53…);
  - fresh tiny and sample seeds 41-43 at 1 and 8 CPUs, against `reference/a_ingest.py`;
  - the malformed logs of step 29, with the same exit codes as before (0, or 134 at a tripwire);
  - 300 mutated logs: 111 exit 0, 189 stop at a tripwire, none segfaults;
  - `objdump | grep -c vpinsrb` = 0.
- **Step 51** also passed the seed-1 hash at 1, 3, 7, 12 and 16 CPUs. It changes only the slice
  count from step 50, and the thread sweep varies exactly that: the slices, the runs and the order
  of endpoint ids all change with the CPU count, and the output may not.
- **Steps 53, 54 and 56** passed the gate script; step 55 changes only comments. Step 56 also
  gave the seed-1 hash 495ca09682b9… at 1, 12 and 20 CPUs, and its gate
  build (the Dockerfile's PGO binary, sha256 cce022cfe4db…) has `objdump -d | grep -c vpinsrb` = 0
  and 542 `vpmovmskb`: since step 53 `movemask` is the plain intrinsic, and LLVM emits the
  instruction itself, with no bit-by-bit rebuild.
- **Steps 57 and 58** passed the gate on the replica: the seed-1 hash at 1, 3, 7 and 12 CPUs,
  sample seeds 1-4 and holdout seed 99, the empty input, and 250 malformed logs (10 stop at a
  tripwire, none crashes).
- **Step 60** passed the gate on the replica, with `rustfmt --check` clean;
  it changes names and formatting only.
- **Steps 64 and 65** passed the replica gates on the exact Dockerfile builds of their code (step
  65 adds only comments to the gated code): the seed-1 hash 495ca09682b9… at 1, 3, 7 and 12 CPUs;
  sample seeds 1-4; holdout seed 99; the empty input; 250 malformed logs, none of which crashes;
  and the 13 in-spec edge inputs, 52 of 52. Their robustness runs are in the table below.
  **Step 66** passed the gate script on its exact Dockerfile build, and its machine code differs
  from step 65's only outside the hot loops (above).
- **Step 59** passed the gate on the replica (the seed-1 hash at 1, 3, 7 and
  12 CPUs, sample seeds 2-4, holdout seed 99) and on the laptop. A build of it (sha256
  aa7ed65b991b…) has no `vpinsrb` and 542 `vpmovmskb`, as step 56's.
- **Steps 61 to 63** passed the gate on the replica's exact Dockerfile builds: the seed-1 hash
  495ca09682b9… at 1, 3, 7 and 12 CPUs; sample seeds 1-4; holdout seed 99 at 12 and 5 CPUs (step
  63 also at 1); the empty input; and 14 edge inputs at 1 and 12 CPUs, as root and as uid 65534.
  The 13 in-spec inputs passed 52 of 52, and the out-of-spec log with 2,100 endpoints stops at the
  id assert.
- **Steps 68 and 69** passed the gate on the replica, each on two Dockerfile builds and on a build
  of the file as committed (comments only after the gate, the same tokens): the seed-1 hash
  495ca09682b9… at 1, 3, 7 and 12 CPUs; sample seed 1 (ca6311bbeb53…) and sample seeds 2-4;
  holdout seed 99; the empty input; 250 malformed logs (9 stop at a tripwire, none crashes); and
  the 13 in-spec edge inputs at 1 and 12 CPUs, as root and as uid 65534, 52 of 52, while the
  out-of-spec log with 2,100 endpoints stops at the id assert. Both also passed the laptop gate,
  at 1, 3, 7 and 8 CPUs. Their robustness runs are in the table below.

**Robustness runs** (replica, from Sun 1 Nov about 17:00 UTC; steps 68 and 69 on Mon 2 Nov), on
the exact Dockerfile builds, with data generated on the replica by the organisers' generator (it
reproduces the official sample seed 1 byte for byte). README, "Robust on unseen seeds", says what
each column ran. Every run was byte-exact, with no crash, no nonzero exit and nothing on stderr:

| Version | Fresh full seeds 101-130 | Repeats, and CPU contention | Tiny and sample, uid 65534, up to 20 CPUs | Page-aligned files |
|---|---|---|---|---|
| Steps 68 and 69 (am5-1, am5-2) | 101-130 at 12 threads, 101-102 at 1 and 3 CPUs: 34/34 | 600/600 (20 repeats of 30 seeds) | 400/400 (seeds 201-210 at 1, 3, 7, 12 and 20 CPUs, as root and as uid 65534, 2 repeats) | — |
| Steps 64 and 65 (a-18, and as-1b with a-19's code lines) | 101-110 at 12 threads, 101-102 at 1 and 3 CPUs: 14/14 | 200/200 (20 repeats of 10 seeds) | 208/208 (seeds 201-205; full 111-112) | — |
| Step 63 (a-17) | 101-110 at 12 threads, 101-102 at 1 and 3 CPUs: 14/14 | 200/200 (20 repeats of 10 seeds) | 208/208 (seeds 201-205; full 111-112) | — |
| Step 59 (a-14) | 40/40 runs | 640/640 | 840/840 | 5/5 |
| Step 58 (a-13) | 34/34 runs | 640/640 | 840/840 | 5/5 |

Earlier versions (steps 54 to 57) passed too. On the 30 fresh seeds step 59 ran 16.71 to 18.22 ms
(medians of 7 runs; seed 1: 17.36 ms), with a memory peak of 18.97 to 20.19 MB on seeds 101-110
(max of 5 runs).

The gate is a shell script kept outside the repo (scc would count it as code): for each input, run
`solution <data dir> <out dir>` and compare `summary.txt` with `cmp` or its sha256.

**The box number.** One binary's board runs spread by up to 40% (above). So:

- **The replica first** (from step 49): paired runs against the code a step would replace (step
  51's table above).
- **Board samples** then confirm. Step 51's last two runs were 0.016954 and 0.016307 s. Step
  56 ran 0.016437 s (run-363747825a0f, 86.39 points), step 55 run-ea674ef8448a (0.016399 s, 19.0
  MB, S 428, 86.44 points), the best Track A points until step 63, step 59 ran 0.016337 to 0.016657 s in
  three runs, 0.01795 s and 0.021609 s, step 60 0.016292 to 0.019578 s in ten runs, step 63 0.016017
  s (run-219d1a5f89d0, 86.64 points) and 0.016107 s, and 0.019571 to 0.022344 s in three slower
  runs, step 64 0.016043 s (run-1bda764d4b1e, 86.65 points, the best Track A points until step 68) and 0.019162
  s, step 65 0.016109 and 0.016208 s, step 66 0.016041 (run-870c44970b40), 0.016152, 0.016158, 0.016216 and 0.016473 s, and 0.020944 and
  0.02058 s, step 67 0.01611 s (run-44c2757557ea), 0.017024 and 0.020705 s, step 68 0.015722 s
  (run-be74feabcc76, 86.85 points, the best Track A run until step 69), and the submitted step 69
  0.018707 s in its first run (run-a533307c210b), with Track A alone slow in that dispatch, and
  0.015715 s in its second (run-b3a4984e4320, 18.22 MB, 86.88 points, the best Track A run).
- **Alternating dispatches**, before the replica: a candidate and the code it would replace were
  pushed in turn, a few minutes apart, so both met the same box state, and a result counted only
  if it repeated. Example (runs.log 08:10Z): step 37, step 39, step 37, step 39, step 37 gave
  0.019041, 0.019000, 0.019097, 0.018979 and 0.020012 s, so step 39 was kept as level-or-better.
  A run in a dispatch where the other tracks' unchanged code was slow too is marked DISTURBED or
  SLOW BOX and not used.
- **Exit-code probes** measure phases the board does not report ("Box probes"): parse end 10.1 ms
  on the new parse, memory floor 7.8 ms.

### What every gate checked (steps 8 to 41)

- **Seed hashes.** sha256 of the full seed-1 output, 495ca09682b9… (as in the spec), and of
  sample seed 1, ca6311bbeb53….
- **Samples.** Byte comparison with the reference output for sample seeds 2–4.
- **Holdout.** A full-scale seed the board does not use (99), regenerated and compared with the
  reference output.
- **Thread sweep.** The same seed-1 hash under `taskset` with 1, 3, 7 and 8 CPUs (laptop) and 1,
  3, 7, 12 and 16 CPUs (test server). The thread count changes the slices, the runs and the order
  in which endpoint ids are handed out, and none of that may change a byte.
- **Fresh seeds.** Newly generated `tiny` and `sample` seeds at every checkpoint (21–24 for step
  30, 21–23 for step 31), compared with `reference/a_ingest.py` at 1 and 8 or 12 CPUs.
- **Edge inputs.** An empty `requests.log` gives an empty file. Malformed logs (a tail without LF,
  a long method, random bytes, a 300-byte endpoint) abort with a message or exit 0, and never
  segfault (step 29).
- **Code check.** `objdump | grep -c vpinsrb` = 0: the bit-by-bit mask rebuild that the inline
  `vpmovmskb` of these steps avoided is not in the binary.