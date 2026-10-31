# Track A: performance log

Every step from the reference to the current version, each with the live-board run that measured it.
Points are the board's 50·log10(K). The box is noisy on this track (reruns of one commit spread about
3% on most runs, with single outliers of up to about 30%: the current code has scored 0.0197 s and 0.0235 s),
so small steps were also timed on a 16-vCPU test server, 21 runs each, before pushing. The commit
column is the Track A code that was measured; some board runs come from a later push that carried
the same Track A code. A dash means the board value was not written down at the time.

| Step | Commit | Change | Run ID | T (s) | M (MB) | S | Points |
|---|---|---|---|---|---|---|---|
| 0 | | The reference, unchanged (the board's calibrated figures, not a run of ours) | | 32.05 | 228 | 56 | 0 |
| 1 | cf9c345 | mmap, one thread per hardware thread, packed u64 records, counting sort by (hour, endpoint), per-group sort | run-9042fdf66fed | 0.0796 | 57.0 | 402 | 64.7 |
| 2 | 060fea1 | AVX2 line parse: endpoint end by vector compare, 32-byte key straight from the register, SWAR latency digits | run-4e23aae357cc | 0.0749 | 59.4 | 505 | 64.5 |
| 3 | 144561a | Parallel glue: parallel prefix sums, scatter and output (pwrite into a pre-sized file); one-multiply hash | run-92e5d2af56e2 | 0.0510 | 62.2 | 575 | 68.0 |
| 4 | 64499b4 | Populate the scatter arrays in one call instead of 12 threads faulting them in | run-5f396cede289 | 0.0508 | 52.0 | 594 | 68.7 |
| 5 | 42acaa7 | Profile-guided build (training run on a generated sample, seed 7) | run-8d33686a4c41 | 0.0498 | 51.6 | 597 | 69.0 |
| 6 | 8acb7f5 | Aggregate by hour: each thread groups its records by hour in place, threads then take whole hours | run-36b7152f0bc2 | 0.0457 | 45.0 | 548 | 70.8 |
| 7 | c9daf53 | p50/p95/p99 by nested selection instead of a full sort; drop the input's page-table entries in each parsing thread instead of at exit | run-73fe01d551bc | 0.0417 | 44.7 | 559 | 71.7 |
| 8 | 9e3cfa8 | u32 records (latencies of 2^20 − 1 or more escape to a side list); hour runs keyed by the timestamp's first 13 bytes, so no calendar code; keep only per-group statistics, free the records, then write | run-6154ff9e18eb | 0.0377 | 24.3 | 548 | 75.5 |
| 9 | 96c87ac | Branch-free method and endpoint ends from one 64-byte space mask; 64-byte endpoint keys | run-10fd036ecbb9 | 0.0361 | 24.8 | 565 | 75.8 |
| 10 | 9b51547 | Output written through a shared mapping instead of one pwrite per hour | run-d92454f8ba54 | 0.0353 | 25.2 | 590 | 75.9 |
| 11 | 3ffca82 | Endpoint names copied out of the chunk before its pages are dropped | run-e557e7f64a7c | 0.0367 | 26.0 | 595 | 75.3 |
| 12 | 0030dfe | Exit as soon as the output is written (no frees) | run-bd4492babdc6 | 0.0307 | 26.1 | 595 | 77.2 |
| 13 | 1d7dbb3, f15bc76 | Each output line built in a stack buffer; groups of up to 32 sorted, not selected; endpoint keys compared inline instead of through `memcmp` | run-9ca9494b04b1 | 0.0299 | 26.1 | 599 | 77.4 |
| 13b | 90751e4, 85672d4, 27cf731 | One AVX2 parse path over a zero-padded mapping (no special case near the end of the file); count and errors in one packed counter per endpoint | run-f90c7c73ef1b | 0.0292 | 26.4 | 581 | 77.7 |
| 14 | 689fffc | Parse loop on raw pointers into preallocated records (CRC hash, interner probe inlined); pass 2 without bounds checks; write fused into pass 2 (hours in order); record pages released early | run-f16959eb717e, run-d79597111386 | 0.0296, 0.0226 | 21.7, 21.8 | 629 | 78.2, 81.1 |
| 15 | 1342bda | One endpoint table shared by the parse threads (CAS inserts), so no serial merge; statically linked binary | run-8e8d92a5861d | 0.0219 | 20.6 | 626 | 81.7 |
| 16 | a1cf30b | The log read through 128 KB buffers per thread instead of mapped; whole names kept in the shared table | run-54e9c5f0e0dd, run-d395d2691d9d | 0.0236, 0.0244 | 21.4 | 610 | 80.8 |
| 17 | f41c193 (revert) | Back to the mapped input of step 15: 1 ms faster on the test server, 2 ms slower on the box | | | | | |
| 18 | 6ca21c8 | Endpoint table with 8x as many slots as endpoints (11% load instead of 45%) | run-f67608cbf0d2 | 0.0227 | 20.9 | 626 | 81.2 |
| 19 | dc7357f, 58c7dc8 | Pass 2 threads no longer wait for the previous hour's end: each publishes its hour's length, keeps the hour pending and takes the next | run-a97b2e82d3c7, run-23cc1c0061e6 | 0.0201, 0.0198 | 20.7, 20.1 | 667, 659 | 82.45, 82.8 |
| 20 | 06454fe | The same engine in 156 fewer lines: errors panic with a message instead of threading a `Result`, one mmap helper, statistics as a plain array; the hot loops unchanged | run-658203ccf3af, run-f73fac56510d | 0.0261, 0.0198 | —, 20.05 | 503 | —, 83.6 |
| 21 | d8eeb54 | Endpoints whose space is past byte 92 (7.6% of lines) found by further 32-byte space windows instead of a byte loop: 12-thread user cycles −3% on the server | run-253bf424b270 | 0.0224 | — | 503 | — |
| 22 | f200302 | Pass 2 counts and scatters by endpoint id, and visits the groups in name order through one sorted id list: the rank table and its dependent load per record are gone | run-2ddd83b092ac | 0.0237 | 18.5 | 496 | — |
| 23 | ab0b4c3 | Names and hours sorted by their first 16 bytes as one big-endian number, so slices are compared only on ties: a shorter serial step between the passes | run-ad6ddaf4d8e4 | 0.0197 | 19.87 | 504 | 83.7 |
| 24 | 33cac7e (reverted in e6e4d9d) | Parse blocks taken from a shared counter, four per thread, so a slow thread holds up less | run-dee36d077800 | 0.0198 | — | 517 | — |
| 25 | d8e2636 (reverted in 1c8d136) | The endpoint key built inline (rebuilt in the cold insert) so it never spills, the latency shuffle control from one table load: 4% fewer instructions on one thread, the same at 12 threads on the server | run-b68029ac7816 | 0.0199 | — | 514 | — |

The current code is step 23 (1c8d136 is the revert of step 25). A later board run of the same code,
run-f5b4c1d48cd4, scored 0.0235 s, 20.72 MB, 81.6 points: one of the outliers mentioned above.

## What each step taught

- **Step 1.** The input is mapped, not read: pages the harness pre-warmed are charged to the host, so
  the 239 MB file costs nothing on the memory axis, where `read()` into one buffer would cost 239 MB.
- **Step 2** bought less than its 100 extra lines suggested: the parse was not yet the bottleneck,
  the serial glue between passes was. Step 3 made the glue parallel and took a third off T.
- **Step 6.** Global histograms and a scatter over a 12 MB array did not scale with threads
  (1 thread 195 ms, 12 threads 60 ms on the server). Grouping by hour first makes every later pass
  touch one hour's data, which fits in L1/L2.
- **Step 8** is the memory step: records went from 8 to 4 bytes, and nothing record-sized is alive
  when the 10 MB output is produced. M 44.7 → 24.3 MB is worth about +2.5 points on its own.
- **Step 9.** There were 2.2 branch mispredictions per line, from the method `match` (GET 46%,
  POST 37%) and a coin-flip test on endpoint length (49% are shorter than 32 bytes). One 64-byte
  window of space bits answers both with `tzcnt`. One thread: cycles −27%, mispredictions 6.9M → 2.6M.
- **Step 10** only won once the test server wrote to tmpfs, as the box does: on a disk-backed
  directory the same change measured 1.3 ms slower, on tmpfs 1.6 ms faster (pwrite calls serialise
  on the file's lock; page faults on a shared mapping do not).
- **Step 11** shows the limits of the test server: 36.2 → 29.9 ms there (released pages were
  faulting back in), no change on the box, where page faults are much cheaper than in the VM.
- **Step 12 came from measuring the box itself.** The board only reports T, so for one run the
  program exited right after a phase with the elapsed time as its exit code (0.25 ms units; the
  board shows a failed run's exit code). Three such runs split A on the box: parse 17.5 ms, merge
  1.25 ms, pass 2 about 6.75 ms, write about 3.5 ms, and then 4–5 ms of freeing the tables before
  the process ended. Exiting right after the write took 35.3 → 30.7 ms.
- **Step 13.** A profile showed 12% of one thread's cycles in libc's `memcmp`: `==` on the 64-byte
  key array compiled to a library call per line. An XOR fold over the eight words is inline.
- **Step 14** cut instructions per parsed line from 333 to 199 on one thread and the test server's
  time from 36.6 to 24.8 ms, yet its first board run was no faster than step 13b (0.0296 s). The
  second run of the same code, from the next push, gave 0.0226 s: one board run can hide a real
  step as easily as it can fake one.
- **Steps 16–17.** Reading through 128 KB buffers was 0.9 ms faster on the test server (19.3 → 18.4 ms)
  and 1.7–2.5 ms slower on the box, in two runs. Page faults cost much more in the test VM than on
  the box, so a win that comes from avoiding faults has to be confirmed on the box.
- **Step 19.** With hours taken in order and written in order, a thread that finished a small hour
  waited for the large hour before it. Publishing the length first and writing later took pass 2
  from 6.9 to 5.9 ms on the server, and gave the first board run under 20 ms.
- **Step 20** is worth almost a point with no change in speed: 659 → 503 lines took the board from
  82.8 to 83.6 points at the same T and M. The first run of it, 0.0261 s, was one of the box's
  outliers; the second was 0.0198 s.
- **Steps 21–23** went out within half an hour, one board run each. Steps 21 and 22 ran slower than
  step 20 (0.0224 and 0.0237 s) and step 23, which contains both, set the best time (0.0197 s). We
  read the two slow runs as box noise, but one run each cannot prove that steps 21 and 22 were
  neutral on their own.
- **Steps 24–25 (reverted).** Dynamic parse blocks and 4% fewer parse instructions both came out at
  the same board time as step 23 (0.0198 and 0.0199 s against 0.0197). Neither the balance between
  the fixed chunks nor the instruction count limits the 12-thread parse on the box. That points at
  memory traffic and the input's page faults, which we have not measured on the box itself. Each
  was reverted for its extra lines (+13 and +10).

## Experiments that did not make it (measured, reverted)

| Experiment | Result |
|---|---|
| Interner inlined into the parse loop | 2.4% faster on one thread, nothing at 12 threads |
| Releasing the input's pages in pass 2 instead of right after parsing | +1.6 ms: the TLB shootdowns interrupt the other pass-2 threads |
| Not releasing them at all | +1.3 ms: the exit then unmaps 58,000 pages on one core |
| `MADV_POPULATE_READ` on each thread's chunk before parsing | 36.4 → 49 ms |
| Groups up to 16 / 64 sorted instead of 32 | 32 was the best of the three (server, one thread) |
| LF found from a 128-byte window, so the next line does not wait for the endpoint | +29 instructions per line, same cycles (one thread), no gain at 12 threads |
| Software prefetch 1, 4 or 16 KB ahead of the parse | no gain |
| Log-linear histogram selection for groups of 256+, AVX2 rank counting for groups of up to 32 | 13M fewer cycles on one thread, nothing at 12 threads once the count and scatter loops were fixed; not worth 120 lines |
| Largest hours first in pass 2 | noise |
| Removing the endpoint-name copy before the pages are released (9e6ed95) | board run-87490db73ccd 0.0342 s and a second run at 0.0332 s, against 0.0292–0.0307 with it: restored in 27cf731 (run-f90c7c73ef1b, 0.0292 s) |
| Three nested selections read at the end | wrong output: each selection reorders the previous rank's slot. Each value is now read right after its own selection |
| Parse blocks from a shared counter, four per thread (step 24) | run-dee36d077800, 0.0198 s against 0.0197 s, +13 lines: reverted in e6e4d9d |
| Fewer instructions per parsed line (step 25) | run-b68029ac7816, 0.0199 s against 0.0197 s, +10 lines: reverted in 1c8d136 |

## Correctness checks run before every push

sha256 of the seed-1 output (495ca096…, as in the spec) and of sample seed 1 (ca6311bb…); byte
comparison with the reference output for sample seeds 2–4; generated `tiny` seeds 5–9 against
`reference/a_ingest.py`; a full-scale seed the board does not use (99) against the reference; an
empty `requests.log`; the same output at 1, 3, 7, 12 and 16 threads (`taskset`), because the thread
count changes the chunks, the runs and the order in which endpoint ids are handed out, and none of
that may change a byte. Each check uses the binary built by this directory's Dockerfile, so the
profile-guided build is the one tested.
