# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

**The lookup table.** regions.csv lists its ids in ascending order, so its last line gives the
table's size: a byte per region id. Threads claim 64 line-aligned chunks of it as they start and
store each region's group; a country gets the next group number the first time any thread sees it,
and the groups are sorted by code when the output is written, with `_UNMATCHED` last.

**Cutting records.csv without reading it twice.** Parallel parsing needs to start each thread at a
record boundary, which in general means knowing whether a byte is inside quotes. Spec 2.2 says that
inside quotes an LF is always followed by a letter or a quote, and a record terminator by the next
record's BOM or id digit, so each cut is placed after the first LF followed by a digit or a BOM byte.
Every cut is then checked: the run before it must end exactly there. If one does not (impossible for
generated data), the file is parsed again as one run, which relies only on spec 2.1.

**Parsing.** Each record is taken from the 64 bytes at its start, as simdjson does for JSON: AVX2
compares give a bitmap of `"` and one of `,`; a carry-less multiply of the quote bits by all ones
gives, for every byte, whether it is inside quotes, and what is left of the comma bitmap are exactly
the field separators. Counting quote bits between separators gives each text field's unescaped
length (`""` is one byte) without looking at its bytes. One 32-byte compare finds the region's comma
and the amount's dot, and one AVX2 pass converts the three numbers. Any record that does not fit
this shape goes to a byte-by-byte parser of spec 2.1, which also rejects invalid input.

**Distinct names per country.** A name is identified by a hash of its raw bytes between the quotes:
inside quotes `""` maps one to one to the unescaped value, and an unquoted value cannot contain a
quote, so equal values have equal raw bytes. The hash has 56 bits: the chance that two of the
generator's 100,000 names share one is about 7e-8. Parsing files a (hash, country) pair under one of 4
partitions per thread, chosen by the hash. After each run it parses, a thread hands its buffers of
pairs to their owners' inboxes, moving them rather than copying. It then lets its own 4 tables
take what waits in its inbox. A table holds a bit per country for each name, and a thread's
tables stay in its cache. No two threads ever write the same table, so the hot path has no
atomics.

**No waiting for a slow thread.** The file is cut into 32 runs per thread, which threads claim
one at a time. A thread that loses its core to another process parses fewer runs, and its pairs
wait in its inbox. No thread waits for another one until the very end. The earlier version had a
barrier after each of 8 rounds, and one CPU shared with a busy loop made it take twice as long (42
ms instead of 22 on the test server); now it takes 25. If an inbox grows past a bound because its
thread fell behind, whoever files into it takes over its tables, so that memory stays bounded too.
Claims go round the file's stripes, one per thread, so that runs parsed at the same time never share a page
table, whose lock their page faults would otherwise contend on.

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise`, `mallopt` and `prctl` declared by hand.

## With another 24 hours

- Find where the box spends the remaining time. The test server's profile (about 60% parsing, 15%
  deduplication, 15% kernel page faults and releases) does not transfer reliably to the box.
- The fast path compiles to about 250 instructions per record, with 20 to 25 values spilled to the
  stack. Small edits elsewhere in the file move the spill count (60 against 68 stack references in
  `parse_run`) and the single-thread instruction count by up to 5%. A kernel that keeps fewer
  values live at once would make the build both faster and stable.
- The region-table lookup is the largest single stall: with a 16 KB table, the run would be about
  10% faster. A compact rank/select table needs two dependent loads and was slower. A structure with
  one load into less than 512 KB is the open question.
