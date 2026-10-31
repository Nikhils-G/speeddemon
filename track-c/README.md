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
quote, so equal values have equal raw bytes. Parsing files a (hash, country) pair under one of 4
partitions per thread chosen by the hash. The file is parsed in 8 rounds; between rounds each thread
marks the pairs of the partitions it owns (the same ones every round) in their small tables, which
hold a bit per country for each name and stay in that thread's cache. No two threads ever write the
same memory, so there are no atomics or locks on the hot path, and the rounds keep the pair buffers
to about 3.5 MB.

## Dependencies

None: the Rust standard library only, with `mmap` and `prctl` declared by hand.

## With another 24 hours

- Find where the box spends the remaining time. The test server's profile (half parsing, a quarter
  deduplication) does not transfer reliably: a shared atomic table is 70% slower there but only 20%
  slower on the box.
- The fast path compiles to about 250 instructions per record, with some 25 register spills; a
  version with fewer values live at once could approach 170.
- The region-table lookup is the largest single stall (cache footprint: a 16 KB table would save
  about 10%). A compact rank/select table needs two dependent loads and was slower; a structure with
  one load into less than 512 KB is the open question.
