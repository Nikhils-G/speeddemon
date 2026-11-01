# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).

## What the program does

It reads `records.csv` (202 MB for seed 1, records of `id,name,note,region_id,amount`) and
`regions.csv` (`region_id,country`). It joins each record to its country through its region; a
record whose region is not listed goes to `_UNMATCHED`. It writes `aggregates.txt`: one line per
country, in byte order of the code, with `_UNMATCHED` last. Each line has the count, the sum of
the ids, the sum, min and max of the amounts, the number of distinct names, and the unescaped
bytes of the names and of the notes.

The CSV is hostile: names and notes are quoted, with `""` escapes, LFs and BOMs inside quotes,
and CRLF and LF terminators mixed. Distinct names must be counted per country over all records.

## The design

| Part | Bottleneck it addresses |
|---|---|
| Cuts from spec 2.2, found by each thread | A serial quote-parity pass over 202 MB |
| simdjson-style bitmaps per record, numbers in one AVX2 pass | Instructions and branches per record on hostile CSV |
| Two records in flight per loop; non-temporal prefetch | The random load into the 1 MB region table |
| Region table: one byte per region id | The join itself |
| Distinct names in per-thread partition tables, buffers handed over | Atomics and cache-line traffic of one shared name table |
| Runs claimed one at a time, no barriers | One slow CPU holding up every thread |
| Block releases, small pooled buffers, exit without frees | Memory peak and the exit's serial unmap |

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
gives, for every byte, whether it is inside quotes (`masks()` returns all three bitmaps), and what
is left of the comma bitmap are exactly the field separators. Counting quote bits between
separators gives each text field's unescaped length (`""` is one byte) without looking at its
bytes. One 32-byte load of the tail (`region_id,amount`) gives, by three compares, the region's
comma, the amount's dot and its minus sign (a bit of the `-` bitmap, so no byte load waits on the
comma), and one AVX2 pass converts the three numbers. About 10% of records need a second 64-byte
window for the note, still on the fast path. A record whose name or note runs past both windows,
and the last records of the file, go through a short walker: one `field()` scan reads each of the
five fields, and a digit fold gives the numbers, an amount's digits without its sign and dot being
its cents. Each run is parsed as two halves at once, so that two records' region-table loads are
in flight together.

**Distinct names per country.** A name is identified by its raw bytes between the quotes: inside
quotes `""` maps one to one to the unescaped value, and the empty name is empty whether it is
quoted or not, so equal values have equal raw bytes. `hash_words` hashes them 32 bytes at a time
without a branch on the length. Each block is loaded whole and ANDed with 32 bytes taken from a
sliding window into `KEEP` (32 bytes of ones, then 32 of zeros), so that bytes past the name count
as zeros. The block then goes through three AES rounds: the state starts as the name's length, the
first 16 bytes are XORed into it and the next 16 are the first round's key, so every output byte
depends on every input byte. Names over 32 bytes, which are rare, loop over further blocks. The
walker hashes a copy of the name followed by 32 zero bytes, so that its loads stay in bounds;
since the mask zeroes everything past the name anyway, both paths give the same name the same
hash. A `compile_error!` stops any build without AES, as for AVX2 and PCLMULQDQ.

Parsing files a pair (the hash's top 56 bits, and the country's group in the low byte) under one
of 4 partitions per thread, chosen from the hash's top 32 bits by a multiply-shift. A partition's
table holds 32-bit keys in buckets of 8 slots, at most 5/8 full. At 12 threads a table has 4096
slots: a key's low 12 bits hold the name's row + 1 (never 0, which marks an empty slot), and its
high 20 bits are hash bits. A lookup starts at the bucket picked by hash bits 8 and up and compares
all 8 keys of the bucket at once. A key that matches still has to pass a check: 15 more hash bits
are stored in the name's row of group bits, in word 3's bits 48..62. Those are the bits of groups
240..254, which never exist, since the spec has at most 240 countries (more aborts the program).
The row is the one the lookup is about to set a bit in, so the check reads data it needs anyway.

Two names are confused only if they share every compared bit: 9 bucket bits, 20 key bits, 15
check bits and the partition (log2 of 48 partitions), 49.6 bits at 12 threads. On one thread a
table has 65,536 slots: 13 bucket bits, 16 key bits, 15 check bits and log2 of 4, 46 bits. With
the generator's 100,000 names, the chance that any two are confused is about
C(100000, 2) / 2^49.6 = 5.9e-6 per dataset at 12 threads, and C(100000, 2) / 2^46 = 7.1e-5 on one
thread. The 64-bit keys of step 31 had 50 bits (4.4e-6) and 54 bits (2.8e-7); the 32-bit keys cost
a little of that for 0.8 MB of memory.

After each run it parses, a thread hands its buffers of pairs to their owners' inboxes, moving
them rather than copying. Its own 4 tables then take what waits in its inbox. A table's row holds
a bit per country for each name, and a thread's tables stay in its cache. No two threads ever
write the same table, so the hot path has no atomics. Emptied buffers go back to one shared pool,
from which a filer takes the empties for its next run (up to 48 at 12 threads) under one lock. A
buffer holds a run's mean share of pairs plus 2 standard deviations plus 4 (113 pairs at 12
threads on the full data); one that overflowed grew, and is dropped once emptied, so the pool's
buffers stay small. The empty name (1% of records) always hashes to the same partition, which is
the one that overflows.

**No waiting for a slow thread.** The file is cut into 64 runs per thread, which threads claim one
at a time. A thread that loses its core to another process parses fewer runs, and its pairs wait
in its inbox. No thread waits for another one until the very end. The earlier version had a
barrier after each of 8 rounds, and one CPU shared with a busy loop made it take twice as long (42
ms instead of 22 on the test server); now it takes 25. If an inbox grows past a bound because its
thread fell behind, whoever files into it takes over its tables, so that memory stays bounded too.
Claims go round the file's stripes, one per thread, so that runs parsed at the same time never
share a page table, whose lock their page faults would otherwise contend on. The last thread to
parse a run of a block of 8 drops the block's input pages (`madvise(MADV_DONTNEED)`, about 2 MB per
call), in parallel instead of in one unmap at exit. Each thread's totals are added to one shared
sum as it finishes, and the last thread to finish writes the output and exits.

**Memory.** At the end of a 12-thread run (smaps, test server) the program's own data is the region
table (0.98 MB), the name tables' keys (0.79 MB: 48 tables of 4096 4-byte slots; 1.57 MB with the
8-byte keys before step 33) and the seen rows (3.26 MB: 100,000 names of 4 words each, a bit per
possible group). The pair buffers wait for about one run, so their size follows the run size.
About 3.3 MB is outside the program's control: about 1 MB already charged when the program starts,
about 0.7 MB of kernel page tables and stacks, and 1-2 MB of the kernel's per-CPU charge batching.

**Tripwires.** Wherever 2.2 is relied on, input outside it fails loudly instead of giving a wrong
answer. Each half-run must end exactly at the next cut (`assert_eq!`). If the input breaks 2.2 in a
way that moves a cut, the records parsed from there almost always end somewhere other than the
next cut, and the assert stops the program. It is a tripwire, not a proof: for general 2.1 input,
the step-24 version in PERF.md is the one to use. Indexing out of bounds, an unexpected header,
more than 240 countries and more distinct names than the tables hold are panics too. The build
aborts on panic (`panic = "abort"`), so each one prints its message and the exit status is
nonzero. Every `unsafe` access is in bounds for any input, not only for 2.2 input: the fast path
never reads outside the 256-byte margin it checks, and the walker hashes a padded copy of the name.

**The spec's RULES fixture exits nonzero, by design.** `tests/test_c_join.py` has a hand-written
`RULES_RECORDS` case for rules "the generator never exercises": a header with LF and no BOM, a BOM
inside quotes after an LF, an unquoted empty note, and a last record without a terminator. It is
valid 2.1 input but not 2.2 input, and this program stops on it with an error message. The
`WORKED_RECORDS` example is 2.2 input and gives the expected output. An earlier version also
handled general 2.1 input: a byte-by-byte 2.1 parser and a second, single-run pass for any wrong
cut. That was about 115 lines. Since the scored data comes only from the generator, it was
removed (PERF.md, step 25).

## Techniques

Each technique below names the bottleneck, what the design does about it, and why the output
stays exact for every seed of the generator. Every total (count, sums, min, max, byte counts) is
exact. The distinct-name count rests on a hash; its odds are stated in technique 6.

### 1. Cuts from the generator's guarantees

- **Bottleneck.** A quote-parity pass to find safe cuts reads the 202 MB file twice, and serially.
- **Design.** Each thread finds its own cuts: the first LF at or after the nominal point that is
  followed by a digit or a BOM byte.
- **Why exact.** Under spec 2.2, an LF inside quotes is followed by a letter or `"`, and a record
  terminator by the next record's id digit or BOM. So such an LF is always a record end. The search
  is monotonic, so the runs cover the file exactly once. Each half-run must end exactly at its
  cut, or `assert_eq!` stops the program.

### 2. simdjson-style bitmaps per record

- **Bottleneck.** Byte-by-byte CSV parsing of quoted fields: one branch per byte and a state
  machine.
- **Design.** For the 64 bytes at a record's start: a bitmap of `"`, a bitmap of `,`, and a
  carry-less multiply of the quote bits by all ones, which XORs every lower bit into each bit and
  so gives the inside-quotes mask. Commas outside it are the separators, found by `tzcnt` and
  clear-lowest-bit. A long note takes a second window, with the quote state carried over.
  `vpmovmskb` is inline asm: written as the intrinsic, LLVM rebuilt the 64-bit masks one bit at a
  time on CPUs without AVX-512, a quarter of the run time (PERF.md, step 7).
- **Why exact.** A `""` pair flips the quote state twice, so it leaves the mask unchanged, and an
  escaped quote is never taken as a field end. A text field's unescaped length is its raw length
  minus the two quotes minus half its inner quote bits, since inside a field every inner quote is
  half of a `""` pair (spec 2.2). Records the windows cannot hold go to the walker, which reads the
  same five fields byte by byte.

### 3. The numbers in one AVX2 pass

- **Bottleneck.** Three digit loops per record, and a byte load for the minus sign that waited
  on the comma's position.
- **Design.** One 32-byte load of the tail gives, by three compares, the region comma, the dot and
  the minus sign. The 8 bytes ending at each number go into one vector; a shuffle drops the
  amount's dot, masks keep each field's digits, and `maddubs`/`madd`/`packus`/`madd` convert all
  three at once.
- **Why exact.** Spec 2.2 keeps the id, the region id and the amount's digits within 8 bytes each,
  and an amount has exactly two decimals, so its digits without the dot are its cents. The
  multiply-adds are exact for 8 digits (at most 99,999,999 fits a 32-bit lane).

### 4. Two records in flight, and a non-temporal prefetch

- **Bottleneck.** The region-table load is random over 1 MB and depends on the parse: 25–30% of a
  single thread's cycles sat on it.
- **Design.** Each run is parsed as two halves in one loop, so two records' table loads are in
  the core at once. The record stream is prefetched non-temporally 1 KB ahead, so it takes less of
  L2 and more of the table stays there.
- **Why exact.** The totals, the min and max, and the set of (name, country) pairs do not depend on
  the order in which records are counted. The second half starts at a cut found like the run cuts,
  and the first half must end exactly there (`assert_eq!`).

### 5. The join is one byte load

- **Bottleneck.** A hash map from region id to country would be a probe and a compare per record.
- **Design.** A dense table by region id, one byte per id, group number + 1, 0 for an id that is
  not listed. It is filled in parallel from 64 claimed chunks of regions.csv. The record's id is
  clamped to the table, and 0 − 1 wraps to group 255, `_UNMATCHED`.
- **Why exact.** regions.csv lists its ids in ascending order (spec 2.3), so the last line gives
  the largest id, and the table has one more entry for every id above it. Each country's group
  number is handed out once under a lock; the output sorts the groups by code.

### 6. Distinct names without atomics

- **Bottleneck.** One shared table needed a `fetch_or` per record and moved cache lines between
  cores (71 against 41 ms on the test server when it was tried again, PERF.md).
- **Design.** A branch-free AES hash of the name's raw bytes. Pairs (hash, country) are filed under
  4 partitions per thread and handed to the owner in buffers, moved and not copied. Each owner's
  tables compare 8 keys of a bucket at once; the key holds the row, and 15 check bits sit in the
  row's unused group bits.
- **Why exact.** Equal unescaped names have equal raw bytes (inside quotes `""` maps one to one to
  `"`, and the empty name is empty quoted or not), so they get equal hashes. Different names are
  counted as one only if their hashes agree on all 49.6 compared bits at 12 threads (46 on one
  thread): about 5.9e-6 per dataset at 12 threads and 7.1e-5 on one, for the generator's 100,000
  names. It is not a proof. The output matched the reference on every seed tried (PERF.md,
  "How correctness was verified"). More names than the tables hold, or more than 240 countries,
  stops the program.

### 7. No barriers

- **Bottleneck.** With a barrier after each round, one CPU shared with a busy loop made the run
  take twice as long: the board showed four runs of unchanged code at about 2x.
- **Design.** Runs are claimed one at a time from a counter, in stripe order. A thread that falls
  behind only lets its own inbox grow, and a filer that finds it past 16 × threads buffers takes
  its tables over. The last thread to finish writes the output.
- **Why exact.** Every run is claimed exactly once, every buffer is taken by exactly one table
  (under its inbox's lock), and every thread takes all inboxes once no thread files any more.
  The totals are summed under one lock, with min and max as min and max.

### 8. Memory and exit

- **Bottleneck.** The pair buffers, the name tables and the exit's unmap of the 202 MB input
  (about 6 ms on the box, measured by a probe).
- **Design.** Every block of 8 runs (about 2 MB) is dropped by the last thread to parse a run of
  it: one TLB-shootdown round per 2 MB. Runs are small (64 per thread), so pairs wait less;
  buffers are sized from the run statistics and pooled; 32-bit keys halve the tables. The last
  thread to finish writes the output and exits without freeing anything. Transparent huge pages
  are off, and `M_TOP_PAD` stops each thread's heap from growing a few pages per `mprotect`.
- **Why exact.** Dropped input pages are never written, so a later read (the cut search or the
  walker reading into a neighbour block) faults them back in from the page cache. That is why it
  is `MADV_DONTNEED` and not `munmap`.

### 9. Build

- **Design.** Profile-guided, trained on data from a byte-identical copy of the organisers'
  generator, with atomic profile counters so that the 12-thread training run gives the same
  profile on every build. Loops aligned to 64 bytes in the final build. Fat LTO, one codegen unit,
  `panic = "abort"`, a static binary, `target-cpu=native`. `#[inline(never)]` on `parse_run` and
  `add`, so that edits to the worker cannot change how their loops are compiled.
- **Why exact.** The build changes speed only; each check runs the Dockerfile's binary.

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise`, `mallopt` and `prctl` declared by
hand. The build needs AVX2, PCLMULQDQ and AES (the Dockerfile builds with `target-cpu=native`);
without them a `compile_error!` stops it.

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
