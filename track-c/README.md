# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## Approach

**What the parser relies on.** Spec section 2.2 lists what the generator writes, for every seed
and scale, the holdout included, and says that a solution may rely on it. This one does. Records
have five fields; the id, region id and amount are never quoted; a name or note is either empty or
quoted; there is no CR except in a CRLF terminator; a BOM only starts a record; and a record's
only `.` is its amount's. The general grammar of 2.1 allows more (unquoted text, quoted numbers, a
missing final terminator, a BOM inside quotes after an LF). Input like that stops the program
instead of producing an answer: see "Tripwires" below.

**The lookup table.** regions.csv lists its ids in ascending order, so its last line gives the
table's size: a byte per region id. Threads claim 64 line-aligned chunks of it as they start and
store each region's group; a country gets the next group number the first time any thread sees it,
and the groups are sorted by code when the output is written, with `_UNMATCHED` last.

**Cutting records.csv without reading it twice.** Under 2.2, inside quotes an LF is always
followed by a letter or a quote, and a record terminator is followed by the next record's BOM or
id digit. So each cut goes after the first LF followed by a digit or a BOM byte, and every cut is
a record start.

**Parsing.** Each record is taken from the 64 bytes at its start, as simdjson does for JSON: AVX2
compares give a bitmap of `"` and one of `,`; a carry-less multiply of the quote bits by all ones
gives, for every byte, whether it is inside quotes (`masks()` returns all three bitmaps), and what is left of the comma bitmap are exactly
the field separators. Counting quote bits between separators gives each text field's unescaped
length (`""` is one byte) without looking at its bytes. One 32-byte compare finds the region's comma
and the amount's dot, and one AVX2 pass converts the three numbers. About 10% of records need a
second 64-byte window for the note, still on the fast path. A record whose name or note runs past
both windows, and the last records of the file, go through a short walker: one `field()` scan
reads each of the five fields (quoted or not), and a digit fold gives the numbers, an amount's
digits without its sign and dot being its cents. Each run is parsed as two halves at
once, so that two records' region-table loads are in flight together.

**Distinct names per country.** A name is identified by a hash of its raw bytes between the quotes:
inside quotes `""` maps one to one to the unescaped value, and the empty name is empty whether it
is quoted or not, so equal values have equal raw bytes. The hash has 56 bits: the chance that two of
the generator's 100,000 names share one is about 7e-8. Parsing files a (hash, country) pair under
one of 4 partitions per thread, chosen by the hash. After each run it parses, a thread hands its
buffers of pairs to their owners' inboxes, moving them rather than copying. Its own 4 tables then
take what waits in its inbox. A table holds a bit per country for each name, and a thread's tables
stay in its cache. No two threads ever write the same table, so the hot path has no atomics.

**No waiting for a slow thread.** The file is cut into 32 runs per thread, which threads claim
one at a time. A thread that loses its core to another process parses fewer runs, and its pairs
wait in its inbox. No thread waits for another one until the very end. The earlier version had a
barrier after each of 8 rounds, and one CPU shared with a busy loop made it take twice as long (42
ms instead of 22 on the test server); now it takes 25. If an inbox grows past a bound because its
thread fell behind, whoever files into it takes over its tables, so that memory stays bounded too.
Claims go round the file's stripes, one per thread, so that runs parsed at the same time never share a page
table, whose lock their page faults would otherwise contend on.

**Tripwires.** Wherever 2.2 is relied on, input outside it fails loudly instead of giving a wrong
answer. Each half-run must end exactly at the next cut (`assert_eq!`). If the input breaks 2.2 in a way
that moves a cut, the records parsed from there almost always end somewhere other than the next
cut, and the assert stops the program. It is a tripwire, not a proof: for general 2.1 input, the
step-24 version in PERF.md is the one to use. Indexing out of bounds,
an unexpected header, more than 255 countries and more distinct names than the tables hold are
panics too. The build aborts on panic (`panic = "abort"`), so each one prints its message and the
exit status is nonzero. Every `unsafe` access is in bounds for any input, not only for 2.2 input:
the fast path never reads outside the 256-byte margin it checks, and the walker hashes a padded copy
of the name.

**The spec's RULES fixture exits nonzero, by design.** `tests/test_c_join.py` has a hand-written
`RULES_RECORDS` case for rules "the generator never exercises": a header with LF and no BOM, a BOM
inside quotes after an LF, an unquoted empty note, and a last record without a terminator. It is
valid 2.1 input but not 2.2 input, and this program stops on it with an error message. The
`WORKED_RECORDS` example is 2.2 input and gives the expected output. An earlier version also
handled general 2.1 input: a byte-by-byte 2.1 parser and a second, single-run pass for any wrong
cut. That was about 115 lines. Since the scored data comes only from the generator, it was
removed (PERF.md, step 25).

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise`, `mallopt` and `prctl` declared by hand.

## With another 24 hours

- Find where the box spends the remaining time. The test server's profile (about 60% parsing, 15%
  deduplication, 15% kernel page faults and releases) does not transfer reliably to the box.
- The region-table lookup is the largest single stall: with a 16 KB table, the run would be about
  10% faster. A compact rank/select table needs two dependent loads and was slower. A structure with
  one load into less than 512 KB is the open question.
- A dot-anchored kernel (find every record's `.` per 64-byte block first, then parse each record
  from its start and its dot, with no chain from one record to the next) was exact on every dataset
  but came out 13% slower and used 21% more instructions in two attempts (PERF.md). A version whose
  stage B compiles to fewer instructions than today's fast path might still win.
