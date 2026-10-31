# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

The log is mapped into memory rather than read, because pages the harness has already cached are
not charged to the process. It is cut at line ends into one slice per hardware thread.

**Parse (pass 1).** Each thread walks its slice line by line. The timestamp is a fixed 27 bytes,
so one 64-byte AVX2 window starting right after it holds the spaces that end the method and the
endpoint; their positions come from `tzcnt` on the mask, with no branch on the method's letters or
the endpoint's length. The endpoint's identity is its bytes zero-padded to 64 (endpoints never
contain a zero byte), loaded and masked straight from the line; only the 5% of lines with an
endpoint of 64 bytes or more also compare bytes. Each thread interns endpoints in its own table, so
nothing is shared while parsing. A line becomes one u32: local endpoint id, a 5xx flag, and the
latency, or an escape marker when the latency needs more than 20 bits (about 1% of lines), with the
value kept in a side list.

Records carry no hour. Shards write in time order, so the file is a sequence of runs of lines from
one hour, and each thread records where its runs start, keyed by the timestamp's first 13 bytes
`YYYY-MM-DDTHH`. Those bytes sort the hours and are the start of the output's hour label, so there is
no date arithmetic, and no assumption about how far a line can be out of order.

**Aggregate (pass 2).** The per-thread endpoint tables are merged and ranked in byte order. Threads
then take whole hours: a thread gathers one hour's runs from every slice, groups the latencies by
endpoint with a counting sort that fits in L1/L2, and finds the nearest-rank p50, p95 and p99 by
selection rather than sorting. It keeps only those statistics and the exact byte length of the hour's
output, then the records are freed, before any output exists.

**Write (pass 3).** The output file is sized, mapped shared, and split into each hour's slice; threads
format their hours straight into place.

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise` and `prctl` declared by hand.

## With another 24 hours

- Read the log through small buffers per thread instead of mapping it, and measure it on the box: on
  the test server page faults are about a third of the parse, but that VM overstates kernel costs.
- Interleave two or more line cursors per thread to overlap the latency of each line's dependent
  loads, as the fastest One Billion Row Challenge entries do.
- Fold the merge of the per-thread endpoint tables into pass 2.
