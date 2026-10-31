# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).
Board best: run-15c21ec71a29, T 0.199 s, M 122.9 MB, S 589, 64.2 points (reference: 72.69 s).
The steps that got there, with their runs, are in [PERF.md](PERF.md).

## Approach

**The problem.** 2,000 exact shortest-path queries on a road network of 1.74M nodes and 5M directed
edges, read from a 92 MB text file. The clock covers parsing and building the graph as well as the
queries, so both halves matter: on the test server, building and renumbering the graph took about
75 ms of a 157 ms run.

**The bottleneck.** Dijkstra on a road network is memory-bound: each settled node reads its offsets,
its edges and the distance labels of its neighbours. The input ids are a random permutation, so in
input order every one of those reads is a cache miss in a different place. The build has the same
problem: placing 5M edges into a CSR array by source node is a random write per edge.

**The design.**

- *Building the graph.* Both files are mapped, not read. One thread per hardware thread parses a
  slice of the edge list and appends each edge to one of up to 128 buckets by source node, a
  sequential write. Threads then take buckets and lay each out with a counting sort that stays in
  cache, into its own part of the CSR array. Each parsing thread drops its slice of the mapping when
  it is done, so the kernel does not unmap 22,450 pages on one core at exit.
- *Renumbering for locality.* Each thread grows a breadth-first region from its own seed, claiming
  nodes with a compare-and-swap; new ids go region by region in BFS order, so the nodes one search
  visits are mostly neighbours in memory: a search ball becomes a few hundred compact runs instead
  of thousands of random lines. The CSR is then copied in the new order, in parallel, and a reverse
  CSR (every edge stored at its head) is built with an atomic cursor per node.
- *Queries.* Threads take queries one at a time. Each answer is a bidirectional Dijkstra: a forward
  search from s over the graph and a backward search from t over the reverse graph take strict
  turns, one settled node each, which settles about 3× fewer nodes than one search from s. Every
  label either side improves offers a path through that node using the other side's label; the
  search stops once the two sides' last-popped keys add up to at least the best such path, or a side
  runs out. Strict turns matter here: picking the side with the smaller key would let the backward
  side flood the whole graph whenever the source's only edges weigh nearly 2^32, which the spec
  includes on purpose.
- *Per-search state.* Each side has a radix heap (Dijkstra's keys never decrease, so an entry lives
  in the bucket of the highest bit where it differs from the last key popped) and a distance table
  in 256-node pages handed out on first touch, so a search's working set stays in L2. Distances are
  stored as 32 bits, with a side map for the rare path through an edge near 2^32. Transparent huge
  pages are turned off so these lazily filled tables are not backed by 2 MB pages.
- *Shortcuts and exit.* A source without out-edges or a target without in-edges is answered -1 at
  once (the reference settles the whole graph to find that out). The process exits right after
  writing the answers, without freeing anything.

**Why it is exact.** Every distance is an exact integer: 32 bits when it fits, the exact u64 in a
side map when it does not, and sums are done in u64. The bidirectional stopping rule is the standard
one: a settled node holds its exact distance, an unsettled one is at least its side's last-popped
key away, so once the two last keys add up to the best path found, no shorter path can be left.
The full argument, including the case where one side runs out, is at `distance` in `src/main.rs`.
Nothing depends on thread timing: the regions and the in-edge order change from run to run, the
distances cannot. Checked before every push on seed 1 (sha256), sample seeds 2–4 (byte comparison
with the reference) and 1 to 16 threads, and once on a full-scale seed 99 dataset.

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise` and `prctl` declared by hand
(`src/mmap.rs`). The Dockerfile builds with profile-guided optimisation, trained on a sample dataset
from a byte-identical copy of the organisers' generator (`d_traverse.py`).

## With another 24 hours

- Pack each edge into 32 bits (head plus weight, with an escape for large weights) in every graph
  array: memory about 123 → 80 MB, and less traffic in every phase.
- Parse the edge list with SWAR digit conversion instead of a byte loop.
- Remove the remaining serial passes over all nodes (the root scan after the BFS regions and the
  prefix sums).
- Preprocess for faster queries (landmarks with the triangle inequality, or contraction hierarchies).
  With only 2,000 queries the preprocessing has to cost less than the search time it saves, which
  needs measuring first.
