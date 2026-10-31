# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).

## Approach

**Building the graph.** The edge list is mapped and parsed by one thread per hardware thread. Each
thread appends its edges to one of 128 buckets by source node, a sequential write; then threads take
buckets and lay each out with a counting sort that stays in cache, into its own part of a CSR array.

**Renumbering for locality.** Input node ids are random, so a shortest-path search would touch a
random cache line per node. Each thread grows a breadth-first region from its own seed, claiming
nodes with a compare-and-swap; new ids go region by region in BFS order, so the nodes one search
visits are mostly neighbours in memory. The CSR is then copied in the new order, in parallel, and a
reverse CSR (every edge stored at its head) is built with an atomic cursor per node.

**Queries.** Threads take queries one at a time. Each answer is a bidirectional Dijkstra: a forward
search from s over the graph and a backward search from t over the reverse graph take strict turns,
one settled node each. Every label either side improves offers a path through that node using the
other side's label; the search stops once the two sides' last-popped keys add up to at least the
best such path, or a side runs out. Each side has a radix heap and a distance table in 256-node pages
handed out on first touch, so a search's working set stays in L2. Distances are 32-bit with a
side map for the rare path through an edge near 2^32. A source without out-edges or a target without
in-edges is answered -1 at once.

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise` and `prctl` declared by hand.

## With another 24 hours

- Pack each edge into 32 bits (head plus weight, with an escape for large weights) in every graph
  array: memory about 127 → 80 MB, and less traffic in every phase.
- Parse the edge list with SWAR digit conversion instead of a byte loop.
- Remove the remaining serial passes over all nodes (the root scan after the BFS regions and the
  prefix sums).
