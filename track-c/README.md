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
length (`""` is one byte) without looking at its bytes. One 32-byte load of the tail
(`region_id,amount`) gives, by three compares, the region's comma, the amount's dot and its minus
sign (a bit of the `-` bitmap, so no byte load waits on the comma), and one AVX2 pass converts the
three numbers. About 10% of records need a second 64-byte window for the note, still on the fast
path. A record whose name or note runs past
both windows, and the last records of the file, go through a short walker: one `field()` scan
reads each of the five fields (quoted or not), and a digit fold gives the numbers, an amount's
digits without its sign and dot being its cents. Each run is parsed as two halves at
once, so that two records' region-table loads are in flight together.

**Distinct names per country.** A name is identified by its raw bytes between the quotes: inside
quotes `""` maps one to one to the unescaped value, and the empty name is empty whether it is quoted
or not, so equal values have equal raw bytes. `hash_words` hashes them 32 bytes at a time without a
branch on the length. Each block is loaded whole and ANDed with 32 bytes taken from a sliding
window into `KEEP` (32 bytes of ones, then 32 of zeros), so that bytes past the name count as
zeros. The block then goes through three AES rounds: the state starts as the name's length, the
first 16 bytes are XORed into it and the next 16 are the first round's key, so every output byte
depends on every input byte. Names over 32 bytes, which are rare, loop over further blocks. The
walker hashes a copy of the name followed by 32 zero bytes, so that its loads stay in bounds;
since the mask zeroes everything past the name anyway, both paths give the same name the same
hash. A `compile_error!` stops any build without AES, as for AVX2 and PCLMULQDQ.

Parsing files a pair (the hash's top 56 bits, and the country's group in the low byte) under one
of 4 partitions per thread, chosen by the hash's top 32 bits. A partition's table key holds the
hash's top 40 bits, the name's row of group bits in bits 8..24 and `0xFF` in the low byte (so a key
is never 0, the empty slot). A lookup starts at the bucket picked by hash bits 8 and up, compares
the 40 bits of 4 slots at once and reads the row from the key it hit, with no second load. Two
names are confused only if they share those 40 bits and their bucket bits: 50 bits at 12 threads
(4096-slot tables), 54 on one thread (65,536 slots). The partition adds nothing, since it is picked
from bits that are compared anyway. With the generator's 100,000 names, the chance that any two of
them are confused is about C(100000, 2) / 2^50 = 4.4e-6 per dataset at 12 threads, and
C(100000, 2) / 2^54 = 2.8e-7 on one thread. (The 7e-8 of earlier versions held only until step 29,
when the tables still compared all 56 bits and kept the rows in a separate array.)

After each run it parses, a thread hands its buffers of pairs to their owners' inboxes, moving
them rather than copying. Its own 4 tables then take what waits in its inbox. A table's row holds
a bit per country for each name, and a thread's tables stay in its cache. No two threads ever write the same table, so the hot path has no atomics.
Emptied buffers go back to one shared pool, from which a filer takes the empties for its next run
(up to 48 at 12 threads) under one lock. A buffer holds a run's mean share of pairs plus 2 standard
deviations plus 4 (113 pairs at 12 threads on the full data); one that overflowed grew, and is dropped once
emptied, so the pool's buffers stay small. The empty name (1% of records) always hashes to the same
partition, which is the one that overflows.

**No waiting for a slow thread.** The file is cut into 64 runs per thread, which threads claim
one at a time. A thread that loses its core to another process parses fewer runs, and its pairs
wait in its inbox. No thread waits for another one until the very end. The earlier version had a
barrier after each of 8 rounds, and one CPU shared with a busy loop made it take twice as long (42
ms instead of 22 on the test server); now it takes 25. If an inbox grows past a bound because its
thread fell behind, whoever files into it takes over its tables, so that memory stays bounded too.
Claims go round the file's stripes, one per thread, so that runs parsed at the same time never share a page
table, whose lock their page faults would otherwise contend on. The last thread to parse a run of a
block of 8 drops the block's input pages (`madvise(MADV_DONTNEED)`, about 2 MB per call), in
parallel instead of in one unmap at exit. Each thread's totals are added to one shared sum as it
finishes, and the last thread to finish writes the output and exits.

**Memory.** At the end of a 12-thread run (smaps, test server) the program's own data is the region
table (0.98 MB), the name tables' keys (1.57 MB: 48 tables of 4096 8-byte slots) and the seen rows
(3.26 MB: 100,000 names of 4 words each, a bit per possible group). The pair buffers wait for about
one run, so their size follows the run size. They were about 2.9 MB before step 31, whose 64 runs
per thread (instead of 32) and shared pool cut the peak by about 1.5 MB (PERF.md). About 3.3 MB is
outside the program's control: about 1 MB already charged when the program starts, about 0.7 MB of
kernel page tables and stacks, and 1-2 MB of the kernel's per-CPU charge batching.

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
The build needs AVX2, PCLMULQDQ and AES (the Dockerfile builds with `target-cpu=native`); without
them a `compile_error!` stops it.

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
