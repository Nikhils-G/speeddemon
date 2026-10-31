# Track D: performance log

Every step from the reference to the current version, each with the live-board run that measured it.
Points are the board's 50·log10(K), with K = (T_ref/T)^0.5 · (M_ref/M)^0.2 · (S_ref/S)^0.15 · (D_ref/D)^0.15.

The Commit column is the commit that introduced the Track D change. The board builds every track on
every push, so a run is sometimes from a later push that changed only other tracks; the Track D code
is the same. Board runs of one commit spread by about 2–5% in T in a quiet hour and by up to 25%
when the box is busy, and M moves by a few MB from run to run (122.9 and 128.3 MB for the current
code), so a step smaller than that is not a measured gain.

| Step | Commit | Change | Run ID | T (s) | M (MB) | S | Points |
|---|---|---|---|---|---|---|---|
| 0 | | The reference, unchanged (organisers' calibration) | | 72.69 | 893 | 43 | 0 |
| 1 | 45e6c6d | Parallel CSR build with atomics; one Dijkstra per query (binary heap), queries in parallel | run-0b9e52ff7751 | 0.928 | 230 | 282 | 47.1 |
| 2 | 8b37779 | Nodes renumbered in BFS order; radix heap; lazily zeroed distance array | run-7fc476f546f4 | 0.416 | 247 | 408 | 54.3 |
| 3 | e6798d9 | BFS regions grown in parallel; 32-bit distances with an overflow map | run-392c7bf5a451 | 0.391 | 164 | 459 | 56.4 |
| 4 | e6910af | Paged distance table per search thread | run-f638c2064ec2 | 0.400 | 143 | 477 | 56.6 |
| 5 | 42acaa7 | Profile-guided build | run-773037920897 | 0.364 | 132 | 477 | 57.9 |
| 6 | 9a6a935, 198043b | CSR built by radix partitioning the edges; renumbering in parallel | run-47f934eb34b8 | 0.329 | 129 | 515 | 58.9 |
| 7 | cf56f1b | Distance get/set always inlined, the over-2^32 path kept cold (kept for the test-server gain; flat on the board) | run-3cbaaea35cc0 | 0.326 | 133 | 527 | 58.8 |
| 8 | a04fdb5 | Bidirectional Dijkstra over a reverse CSR, strict alternation; 256-node distance pages; THP off for the query phase | run-714cdd5314e8 | 0.200 | 129 | 577 | 63.9 |
| 9 | ba09515 | Each parsing thread drops its part of the mapping | run-d10160e8a98d | 0.198 | 127 | 588 | 64.1 |
| 10 | 0030dfe | Exit right after writing the output (current code) | run-15c21ec71a29 | 0.199 | 123 | 589 | 64.2 |

Other board runs of the same code, for the noise:

- Step 3: run-1f98089ca892 T 0.381 M 160.9 (56.7); run-92bba37ecb17 T 0.381 M 162.9.
- Step 4: run-242203a2b329 (pushed with d7860cf) T 0.388 M 134.8 (57.2).
- Step 5: run-432067883f9e, the first run with profile-guided builds, T 0.392 M 133.6 (57.1).
- Step 10: run-937b6a6ab04c, the first run, T 0.2005; run-d42af3966e98 (pushed with 1c8d136, the
  latest) T 0.2009 M 128.3 (63.9). run-15c21ec71a29 was pushed with 2bf221c. During busy hours the
  same code also measured 0.22–0.25 s; those runs were taken as box noise, since other tracks'
  unchanged code slowed down in the same runs.

## Where the time went

Phase times on the test server (16 vCPU AMD EPYC VM, not the scoring box) after step 5: building the
CSR 78 ms, relabelling 52 ms, queries 177 ms. Step 6 took the build to 34 ms and the relabel to
40 ms, which left the queries as most of the run; step 8 then took the whole run from 240 to
157 ms on that server. A profile at step 4: search 41%, distance-table reads 18%, parsing 14%,
kernel inter-processor interrupts 9% (a VM cost), relabelling 8%.

## What each step taught

- **Step 2.** Input ids are a random relabelling, so a search ball of 10,000 nodes touched 10,000
  random cache lines in each of the offsets, edges and distances. Renumbering in BFS order makes the
  same ball a few hundred compact runs: T 0.928 → 0.416 s. The radix heap fits Dijkstra's monotone
  keys and replaces a 15-level binary heap with appends to short vectors.
- **Step 3.** Growing the regions in parallel took the serial BFS out of the relabel, and 32-bit
  distances halved the table: M 247 → 164 MB. Distances above 2^32 (the spec has them) go to a side
  map, so nothing is truncated.
- **Step 4** cut memory (each search thread's table is a few MB of pages instead of 7 MB) but not
  time: on the test server it was 0.35 → 0.30 s, on the board 0.391 → 0.400 s. It was kept for the
  21 MB. Lesson: the test server is a VM on another CPU and over-weights kernel costs; only the
  board decides speed.
- **Step 5.** The profile-guided build was 0.31 → 0.29 s on the test server; on the board the first
  run (0.392) was inside the noise and the second 0.364.
- **Step 6.** Scattering 5M edges with an atomic cursor each is a random write per edge. Bucketing
  them by source node first (sequential appends), then laying out one bucket at a time in cache,
  took the build from 78 to 34 ms on the test server. Renumbering in parallel took the relabel from
  48 to 40 ms there. Board: 0.364 → 0.329 s.
- **Step 7.** Distance reads were 25% of the profile as an out-of-line call that carried the rare
  overflow-map lookup. Inlining the common path and marking the map path cold gave 0.25 → 0.23 s with
  12 threads and 2.09 → 1.89 s with one thread on the test server, but 0.329 → 0.326 s on the board,
  which is noise, for 12 more lines. It stayed in because it is a CPU-side change (those did carry
  over to the box elsewhere) and the next step's search runs through the same two functions.
- **Step 8** is the algorithmic step: T 0.326 → 0.200 s, +5.1 points. The forward search from s and
  a backward one from t over the reversed graph settle about 3× fewer nodes. The two sides take
  strict turns: choosing the side with the smaller frontier key instead floods the whole graph from
  t when nothing reaches back to s (the spec's sources without in-edges start every path with a
  weight near 2^32, so every forward key is near 2^32). The search stops when the two last-popped
  keys, each a lower bound for its side, add up to the best meeting path found (the proof is at
  `distance` in `src/main.rs`). The reverse graph also replaces the "has an in-edge" flags: a target
  with no in-edges is still answered -1 at once. The distance pages went from 4,096 to 256 nodes,
  so a search zeroes and touches less memory around the nodes it reaches; each pool is reserved once
  so it never moves; and transparent huge pages are turned off before the queries, so a lazily
  filled pool is not backed by 2 MB pages (memory would then depend on the host).
- **Step 9.** Each parsing thread drops the page-table entries of its slice of the 92 MB input
  (`madvise(MADV_DONTNEED)`), so the kernel does not unmap 22,450 pages one by one on one core at
  exit: 0.2003 → 0.1975 s, within noise.
- **Step 10.** Calling `exit` right after the write skips freeing about 95 MB of graphs one
  allocation at a time. In Tracks A and C the same change saved 3.5–4.6 ms; here 0.1975 → 0.1988 s is
  inside the noise, but it is one line.

## Experiments that did not make it (measured, reverted)

| Experiment | Result |
|---|---|
| Hash-map distance table instead of a dense one | 2× slower on the test server; not pushed |
| Populating (pre-faulting) the edge array before scattering | slower on the test server; not pushed |
| Prefetching the next node's edges and neighbours in the search | test server 0.37 s, slower than without; not pushed |

Two kept steps were also flat on the board and are in the table above with their numbers: the paged
table (step 4, kept for memory) and the inlined get/set (step 7).

## Correctness checks run before every push

- sha256 of the seed-1 full output (`a23dbdf0…`).
- Byte comparison with the reference output for sample seeds 2, 3 and 4.
- The same output at 1, 3, 7, 12 and 16 threads: the order of a node's in-edges in the reverse graph
  depends on thread timing, the answers must not.
- Once, a full-scale seed 99 dataset: output identical to the official reference's.
- On the board, every run passed the correctness gate on seed 1.

The output is exact by construction rather than by testing alone: integer distances only, an exact
side map for the values above 2^32, and the standard bidirectional stopping rule; the argument is
written out at `distance` in `src/main.rs`.
