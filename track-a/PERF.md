# Track A: performance log

Every step from the reference to the current version, with the live-board run that measured it.
Points are the board's 50·log10(K). The box is noisy on this track. Reruns of one commit usually
spread about 3%, with single outliers of up to about 40% in dispatches where the other tracks'
unchanged code was slow too. Our run log marks those dispatches DISTURBED or SLOW BOX, and so does
this table. Small steps were also timed on a 16-vCPU test server first (CPUs 0-11, `/out` on
tmpfs, interleaved runs, medians), before pushing.

The commit column is the Track A code that was measured. Some board runs come from a later push
that carried the same Track A code. A dash means the value was not written in our run log. Rows
without a run ID either had no board run of their own or their run is not in the log; their
numbers are left out rather than guessed.

| Step | Commit | Change | Run ID | T (s) | M (MB) | S | Points |
|---|---|---|---|---|---|---|---|
| 0 | | The reference, unchanged (the board's calibration figures, not a run of ours) | | 32.05 | 228 | 56 | 0 |
| 1 | cf9c345 | mmap, one thread per hardware thread, packed u64 records, counting sort by (hour, endpoint), per-group sort | run-9042fdf66fed | 0.0796 | 57.03 | 402 | 64.72 |
| 2 | 060fea1 | AVX2 line parse: endpoint end by vector compare, 32-byte key straight from the register, SWAR latency digits | run-4e23aae357cc | 0.0749 | 59.36 | 505 | 64.45 |
| 3 | 144561a | Parallel glue: parallel prefix sums, scatter and output (pwrite into a pre-sized file); one-multiply hash | run-92e5d2af56e2 | 0.0510 | 62.2 | 575 | 68.0 |
| 4 | 64499b4 | Populate the scatter arrays in one call instead of 12 threads faulting them in (board run not in our log) | | | | | |
| 5 | 42acaa7 | Profile-guided build (training run on a generated sample, seed 7) | run-8d33686a4c41 | 0.0498 | 51.6 | 597 | 68.96 |
| 6 | 8acb7f5 | Aggregate by hour: each thread groups its records by hour in place, threads then take whole hours | run-36b7152f0bc2 | 0.0457 | 45.02 | 548 | 70.77 |
| 7 | c9daf53 | p50/p95/p99 by nested selection instead of a full sort; the input's page-table entries dropped in each parsing thread instead of at exit | run-73fe01d551bc | 0.0417 | 44.66 | 559 | 71.72 |
| 8 | 9e3cfa8 | u32 records (latencies of 2^20 − 1 or more escape to a side list); hour runs keyed by the timestamp's first 13 bytes, so no calendar code; only per-group statistics kept, records freed, then the write | run-6154ff9e18eb | 0.0377 | 24.3 | 548 | 75.53 |
| 9 | 96c87ac | Branch-free method and endpoint ends from one 64-byte space mask; 64-byte endpoint keys | run-10fd036ecbb9 | 0.0361 | 24.79 | 565 | 75.82 |
| 10 | 9b51547 | Output written through a shared mapping instead of one pwrite per hour | run-d92454f8ba54 | 0.03528 | 25.19 | 590 | 75.85 |
| 11 | 3ffca82 | Endpoint names copied out of the chunk before its pages are dropped | run-e557e7f64a7c | 0.0367 | 26.04 | 595 | 75.27 |
| 12 | 0030dfe | Exit as soon as the output is written (no frees) | run-bd4492babdc6 | 0.03066 | 26.05 | 595 | 77.20 |
| 13 | 1d7dbb3, f15bc76 | Each output line built in a stack buffer; groups of up to 32 sorted, not selected; endpoint keys compared inline instead of through `memcmp` (DISTURBED dispatch: B's unchanged code ran 0.3609 s in it) | run-9ca9494b04b1 | 0.02994 | 26.11 | 599 | 77.43 |
| 13b | 90751e4, 85672d4, 27cf731 | One AVX2 parse path over a zero-padded mapping (no special case near the end of the file); count and errors in one packed counter per endpoint | run-f90c7c73ef1b | 0.02924 | 26.37 | 581 | 77.74 |
| 14 | 689fffc | Parse loop on raw pointers into preallocated records (CRC hash, interner probe inlined); pass 2 without bounds checks; write fused into pass 2 (hours in order); record pages released early | run-f16959eb717e, run-d79597111386 | 0.02959, 0.02257 | 21.65, 21.75 | 629 | 78.21, 81.13 |
| 15 | 1342bda | One endpoint table shared by the parse threads (CAS inserts), so no serial merge; statically linked binary | run-8e8d92a5861d | 0.02188 | 20.58 | 626 | 81.72 |
| 16 | a1cf30b | The log read through 128 KB buffers per thread instead of mapped; whole names kept in the shared table (dispatch marked "DISTURBED?" in our log; a second sample, 0.0244 s, has no run ID in the log) | run-54e9c5f0e0dd | 0.0236 | — | — | — |
| 17 | f41c193 (revert) | Back to the mapped input of step 15: 1 ms faster on the test server, 2 ms slower on the box | | | | | |
| 18 | 6ca21c8 | Endpoint table with 8x as many slots as endpoints (11% load instead of 45%) | run-f67608cbf0d2 | 0.0227 | — | — | — |
| 19 | dc7357f, 58c7dc8 | Pass 2 threads no longer wait for the previous hour's end: each publishes its hour's length, keeps the hour pending and takes the next | run-a97b2e82d3c7, run-23cc1c0061e6 | 0.020053, 0.019785 | 20.67, 20.05 | 667, 659 | 82.45, 82.76 |
| 20 | 06454fe | The same engine in 156 fewer lines: errors panic with a message instead of threading a `Result`, one mmap helper, statistics as a plain array; the hot loops unchanged | run-658203ccf3af, run-f73fac56510d | 0.026108, 0.019836 | —, 20.05 | 503 | —, 83.62 |
| 21 | d8eeb54 | Endpoints whose space is past byte 92 (7.6% of lines) found by further 32-byte space windows instead of a byte loop: 12-thread user cycles −3% on the server | run-253bf424b270 | 0.0224 | — | — | — |
| 22 | f200302 | Pass 2 counts and scatters by endpoint id, and visits the groups in name order through one sorted id list: the rank table and its dependent load per record are gone | run-2ddd83b092ac | 0.0237 | 18.49 | — | — |
| 23 | ab0b4c3 | Names and hours sorted by their first 16 bytes as one big-endian number, so slices are compared only on ties: a shorter serial step between the passes | run-ad6ddaf4d8e4 | 0.019691 | 19.87 | 504 | 83.73 |
| 24 | 33cac7e (reverted in e6e4d9d) | Parse blocks taken from a shared counter, four per thread, so a slow thread holds up less | run-dee36d077800 | 0.019777 | — | 517 | — |
| 25 | d8e2636 (reverted in 1c8d136) | The endpoint key built inline (rebuilt in the cold insert) so it never spills, the latency shuffle control from one table load: 4% fewer instructions on one thread, the same at 12 threads on the server | run-b68029ac7816 | 0.019916 | — | 514 | — |
| 26 | f8ccd39 | The same engine in a third fewer lines, one file: the endpoint table allocated zeroed and probed by one function, every endpoint's bytes copied when it is added; all runs sorted by (hour, slice) and cut into hours; an hour's start summed from the published lengths. Hot loops unchanged | run-b5ca2d290934 | 0.019723 | 21.01 | 335 | 84.80 |
| 27 | a70edf7 | Each parse thread finds its own cuts; tighter imports and digit table. Same engine as step 26 | run-ad02131f83b0 | 0.020038 | — | 327 | 84.68 |
| 28 | fb34663 | Dockerfile only: PGO training with atomic profile counters, so every build gets the same profile (two board runs of the same code) | run-ca06822d40f0, run-64be7f99b895 | 0.019331, 0.019387 | — | 327 | 85.08, 85.09 |
| 29 | 2148d5f | Malformed input aborts instead of reading past the padding: the long-endpoint window loop is bounded, one cold-path assert checks the endpoint length, and the record capacity holds for any input. Fuzzing: 68 of 300 mutated logs segfaulted before, 0 after | run-14334649ab5d | 0.019612 | — | 327 | — |
| 30 | 56c42bf | One set of threads for both passes: workers publish their parse, wait for the merge and go straight into pass 2, instead of being joined and 12 new threads spawned | run-666179516fda | 0.020152 | — | 338 | — |
| 31 | bbdb79e | Memory peak in pass 2: the log unmapped after the parse (frees its page tables), endpoint keys and a part's escapes released, pending hour statistics in 20 bytes per endpoint, the scratch array sized once for the largest hour, and hours taken largest first in each block of one per thread | run-d1362361f9e4 | 0.019837 | 18.24 | 355 | 85.16 |
| 32 | d8f31b6, 1261c8d (probes, removed in a067c63) | Two temporary timing probes on the box (see "Box probes" below). The board shows both as failed correctness, by design | run-7d4218a4cdba, run-be516aa463a5 | probe | probe | | |
| 33 | bf2aca8 | Endpoints sorted on the first 32 bytes of their keys (fewer ties); the log's unmap and the keys' release moved off the serial merge, onto the idle main thread during pass 2 | run-6cca232c0f6a | 0.018935 | 18.66 | 352 | 85.60 |
| 34 | b25326b | Dockerfile only: hot loops aligned to 64 bytes in the profile-use build (`-align-loops=64`). Two more runs of this code were in a DISTURBED and a SLOW BOX dispatch: run-3feab32d2b8f 0.026024 s, run-69d0c9f98c0e 0.026627 s | run-df8ecf640b7e | 0.019217 | 18.06 | 352 | 85.58 |
| 35 | f8206db (reverted in 51d3674) | apost-1: endpoint sort moved into worker 0, scratch arrays released without `munmap`, output page-table entries dropped in parallel before the exit. The second run was in a dispatch our log calls "a bit slow" | run-7833033c46e9, run-4e7cded1b902 | 0.019312, 0.021607 | 18.52, 18.09 | 360 | 85.34, 84.22 |

The current code is step 34: the Track A source of step 33 (bf2aca8, restored by 51d3674 after
step 35) with the Dockerfile of step 34. Its best run is step 33's run-6cca232c0f6a, 85.60 points.

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
  faulting back in), and no gain on the box (0.0367 s).
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
- **Step 19.** With hours taken in order and written in order, a thread that finished a small hour
  waited for the large hour before it. Publishing the length first and writing later took pass 2
  from 6.9 to 5.9 ms on the server, and gave the first board run under 20 ms.
- **Step 20** is worth almost a point with no change in speed: 659 → 503 lines took the board from
  82.76 to 83.62 points at the same T and M. The first run of it, 0.026108 s, was one of the box's
  outliers.
- **Steps 21–23** went out within half an hour, one board run each. Steps 21 and 22 ran slower
  than step 20 (0.0224 and 0.0237 s), and step 23, which contains both, set the best time then
  (0.019691 s). One run each cannot prove that steps 21 and 22 were neutral on their own.
- **Steps 24–25 (reverted).** Dynamic parse blocks and 4% fewer parse instructions both came out at
  the same board time as step 23 (0.019777 and 0.019916 s against 0.019691). Neither the balance
  between the fixed chunks nor the instruction count limits the 12-thread parse on the box. Each
  was reverted for its extra lines (+13 and +10). Step 32 later showed why: the parse sits close
  to the memory floor.
- **Steps 26–27** cut 504 → 327 lines (+1.07 points at the same T and M: 83.73 → 84.80, then 84.68
  in a slower run). Speed was checked against step 23 at every stage, over three builds of each
  version, because the multi-threaded PGO training gave every build a different profile: three
  builds of the unchanged step-23 source spread 0.5 ms on the server.
- **Step 28** fixed that build-to-build spread at its cause. The instrumented training run has 12
  threads, and plain profile counters lose updates when threads increment them at once. Atomic
  counters give the same profile on every build. A first attempt, training on one CPU with
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
  during pass 2. Server: −0.13 ms. Box: 0.018935 s, the best Track A run, 85.60 points.
- **Step 34.** Loop alignment is a Dockerfile flag, meant to make the layout of the hot loops
  depend less on where unrelated code lands. The one normal-box run, 0.019217 s, is within the
  spread of step 33; its M, 18.06 MB, is the lowest A run. The flag is kept for Tracks A and C (C
  set its best run with it). The two runs in disturbed dispatches, 0.026 s each, came with the
  other tracks' unchanged code running slow in the same dispatches.
- **Step 35 (reverted).** −0.70 ms on the test server, not faster on the box in two runs: see the
  next section and the experiments table.

## The box versus the test server

Several steps did well on the test server and did nothing, or worse, on the box:

| Change | Test server | Box |
|---|---|---|
| Names copied before release (step 11) | 36.2 → 29.9 ms | 0.0367 s, no gain |
| pread through 128 KB buffers (step 16) | −0.9 ms | 0.0236 and 0.0244 s against 0.02188 |
| apost-1: sort in worker 0, no `munmap` of the scratch, parallel output teardown (step 35) | −0.70 ms (−3.8%) | 0.019312 and 0.021607 s against 0.018935 and 0.019217 |

All three are kernel-side: page faults, the `mmap` lock, TLB shootdowns and the exit's unmap. The
test server is a VM, where faults and cross-core interrupts cost much more than on the bare-metal
box. Changes that remove plain CPU work from the critical path (steps 9, 13, 14, 19, 33) moved
both machines the same way. A kernel-side or teardown gain is only real once a board run shows it.

## Box probes (failed runs on the board, by design)

The board reports only T, M and S of a correct run, and the exit code of a failed one. So a
temporary build can report a phase time by exiting with it. Each probe was pushed alone, removed
in the next commit, and shows on the board as "✗ correctness". They are measurements, not
failures of the solution:

| Run ID | Probe | Exit code | Reading |
|---|---|---|---|
| run-7d4218a4cdba | d8f31b6: exit when every part is in (0.1 ms units) | 110 | parse done at 11.0 ms |
| run-be516aa463a5 | 1261c8d: the parse loop replaced by one byte touched per 64 | 78 | memory floor 7.8 ms |

The earlier probes behind step 12 used 0.25 ms units; their run IDs are not in our log.

## Experiments that did not make it (measured, reverted)

| Experiment | Result |
|---|---|
| apost-1 (f8206db, reverted in 51d3674): the endpoint sort moved from the merge into worker 0 (serial merge 250 → 115 µs on the server); runs gathered as each part comes in; the scratch arrays released with `MADV_DONTNEED` and leaked instead of unmapped (their `munmap` took the mmap write lock during the final writes); after a barrier, every thread drops its share of the output mapping's page-table entries (exit 1.07 → 0.57 ms) | Server: −0.70 ms median (−3.8%), 41 interleaved runs, two PGO builds each; memory unchanged. Box: run-7833033c46e9 0.019312 s and run-4e7cded1b902 0.021607 s (a slightly slow dispatch), against run-6cca232c0f6a 0.018935 s and run-df8ecf640b7e 0.019217 s for the current code. Not faster on the box, +8 lines: reverted |
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
| Parse blocks from a shared counter, four per thread (step 24) | run-dee36d077800, 0.019777 s against 0.019691 s, +13 lines: reverted in e6e4d9d |
| Fewer instructions per parsed line (step 25) | run-b68029ac7816, 0.019916 s against 0.019691 s, +10 lines: reverted in 1c8d136 |
| Pass 2 in two phases: every thread computes its hours, waits at a barrier, then writes them | +2 ms on the server. The write phase alone took 2.7 ms: twelve threads faulting in the shared output at once most likely serialise in the kernel |
| Endpoint table entries as one `OnceLock<(key, name)>` per id | Parse +0.4 ms at 12 threads: the lock's state word sits on a second cache line, read on every lookup |
| Each line written straight into the output from its known length, without a stack buffer | +14M instructions on one thread (552M → 566M): the length is then computed twice per line |
| The first digit of every number written as a pair from the table, and the hour label copied as a slice of unknown length | +15M instructions on one thread (540M → 555M) |
| Only long runs released, not a slice's remaining records once no hour reads them | Memory peak +0.7 MB (20.04 → 20.76 MB, 15 runs each) |

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
- **Lines are points.** Steps 20 and 26–27 gained 0.9 and 1.1 points with no change in speed. A
  change that saves 0.1 ms on the box has to cost fewer than about 10 lines.

## How correctness was verified

Before every push, on the binary built by this directory's Dockerfile (so the PGO build is the
one tested):

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
  `vpmovmskb` avoids is not in the binary.
