# Track D: performance log

Every step from the reference to the current version, each with the live-board run that measured it.
Points are the board's 50·log10(K). Reruns of one commit spread about 2–5%.

| Step | Commit | Change | Run ID | T (s) | M (MB) | S | Points |
|---|---|---|---|---|---|---|---|
| 0 | | The reference, unchanged | | 72.69 | 893 | 43 | 0 |
| 1 | 45e6c6d | Parallel CSR build with atomics; one Dijkstra per query (binary heap), queries in parallel | run-0b9e52ff7751 | 0.928 | 230 | 282 | 47.1 |
| 2 | 8b37779 | Nodes renumbered in BFS order; radix heap; lazily zeroed distance array | run-7fc476f546f4 | 0.416 | 247 | 408 | 54.3 |
| 3 | e6798d9 | BFS regions grown in parallel; 32-bit distances with an overflow map | run-392c7bf5a451 | 0.391 | 164 | 459 | 56.4 |
| 4 | e6910af | Paged distance table per search thread | run-f638c2064ec2 | 0.400 | 143 | 477 | 56.6 |
| 5 | 42acaa7 | Profile-guided build | run-773037920897 | 0.364 | 132 | 477 | 57.9 |
| 6 | 9a6a935, 198043b | CSR built by radix partitioning the edges; renumbering in parallel | run-47f934eb34b8 | 0.329 | 129 | 515 | 58.9 |
| 7 | a04fdb5 | Bidirectional Dijkstra over a reverse CSR, strict alternation; 256-node distance pages | run-714cdd5314e8 | 0.200 | 129 | 577 | 63.9 |
| 8 | ba09515 | Each thread drops its parsed part of the mapping | run-d10160e8a98d | 0.198 | 127 | 588 | 64.1 |

## What each step taught

- **Step 2.** Input ids are a random relabelling, so a search ball of 10,000 nodes touched 10,000
  random cache lines in each of the offsets, edges and distances. Renumbering in BFS order makes the
  same ball a few hundred compact runs. The radix heap fits Dijkstra's monotone keys.
- **Step 4** cut memory (each search thread's table is a few MB of pages, not 7 MB per thread) but
  not time.
- **Step 6.** Scattering 5M edges with an atomic cursor each is a random write per edge. Bucketing
  them by source node first (sequential appends), then laying out one bucket at a time in cache, took
  the build from 78 to 34 ms on the test server.
- **Step 7** is the algorithmic step: the forward search from s and a backward one from t over the
  reversed graph settle about 3× fewer nodes. The two sides take strict turns: choosing the side with
  the smaller frontier key instead floods the whole graph from t when nothing reaches back to s,
  because every forward key is then near 2^32. The search stops when the two last-popped keys, each a
  lower bound for its side, add up to the best meeting path found. The reverse graph also replaces the
  "has an in-edge" flags: a target with no in-edges is still answered -1 at once.

## Experiments that did not make it (measured, reverted)

| Experiment | Result |
|---|---|
| Hash-map distance table instead of a dense one | 2× slower (server) |
| Populating the edge array before scattering | slower |
| Prefetching the next node's edges in the search | slower on the server |
| Inlined distance get/set (cf56f1b) | server 0.25 → 0.23 s; on the board run-3cbaaea35cc0, T 0.326 = noise |

## Correctness checks run before every push

sha256 of the seed-1 output; byte comparison with the reference output for sample seeds 2–4; the
same output at 1, 3, 7, 12 and 16 threads (in-edge order in the reverse graph depends on timing,
answers must not).
