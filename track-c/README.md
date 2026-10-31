# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

**The lookup table.** regions.csv is parsed in two parallel passes over line-aligned slices. The
first marks which of the 26³ possible country codes occur; numbering those in sorted order makes the
group number the output order, with `_UNMATCHED` last. The second fills a byte per region id with
its group.

**Cutting records.csv without reading it twice.** Parallel parsing needs to start each thread at a
record boundary, which in general means knowing whether a byte is inside quotes. Spec 2.2 says that
inside quotes an LF is always followed by a letter or a quote, and a record terminator by the next
record's BOM or id digit, so each cut is placed after the first LF followed by a digit or a BOM byte.
Every cut is then checked: the run before it must end exactly there. If one does not (impossible for
generated data), all cuts are redone from quote parity, which relies only on spec 2.1.

**Parsing.** Each thread reads its runs through a 128 KB buffer and scans 64 bytes at a time, as
simdjson does for JSON: AVX2 compares give a bitmap of `"` and one of `,` and LF; a carry-less
multiply of the quote bits by all ones gives, for every byte, whether it is inside quotes. What is
left of the second bitmap are exactly the field separators. Counting quote bits between separators
gives each text field's unescaped length (`""` is one byte) without looking at its bytes. Numbers are
converted eight digits at a time with SWAR arithmetic.

**Distinct names per country.** A name is identified by a hash of its raw bytes between the quotes:
inside quotes `""` maps one to one to the unescaped value, and an unquoted value cannot contain a
quote, so equal values have equal raw bytes. Parsing files a (hash, country) pair under one of 64
partitions chosen by the hash. The file is parsed in 8 rounds; between rounds each thread takes whole
partitions and marks the pairs in that partition's small table, which holds a bit per country for
each name. No two threads ever write the same memory, so there are no atomics, and the rounds keep
the pair buffers to about 4 MB.

## Dependencies

None: the Rust standard library only, with `mmap` and `prctl` declared by hand.

## With another 24 hours

- Find where the box spends the remaining time. The test server's profile (half parsing, a quarter
  deduplication) does not transfer reliably: a shared atomic table is 70% slower there but only 20%
  slower on the box.
- A branch-free stage 2 that takes all five fields of a record from one separator bitmap word when
  they fit, instead of one scanner call per field.
- Fewer lines: the scalar fallbacks and the error plumbing are about a fifth of the code.
