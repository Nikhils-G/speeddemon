# Track C: Join

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md).
The step-by-step history, with a board run for every step, is in [PERF.md](PERF.md).

## What the program does

It reads `records.csv` (202 MB for seed 1, records of `id,name,note,region_id,amount`) and
`regions.csv` (`region_id,country`). It joins each record to its country through its region; a
record whose region is not listed goes to `_UNMATCHED`. It writes `aggregates.txt`: one line per
country, in byte order of the code, with `_UNMATCHED` last. Each line has:
- the count and the sum of the ids;
- the sum, min and max of the amounts;
- the number of distinct names;
- the unescaped bytes of the names and of the notes.

The CSV is hostile: names and notes are quoted or empty, with `""` escapes and commas inside the
quotes, LFs inside notes, BOMs at some record starts, and CRLF and LF terminators mixed. Distinct
names must be counted per country over all records.

## Where the time goes on the box

The reference runs 14.44 s at 367 MB and S 108. The submitted code (PERF.md step 60, 779a8bf,
S 593) is step 59 with every page its workers write faulted in by four threads only, so that only
four CPUs keep the kernel's per-CPU charge stock, which counts in the memory peak ("Memory" below);
the parse is unchanged. Its first three board runs took 0.017192 to 0.01737 s at 8.73 to 9.16 MB,
83.48 to 83.74 points (run-a3586e5340b9, run-04d0edce5bc1, run-6fb2a1aac4ae), where step 59's eight
runs peaked at 9.76 to 10.69 MB. Its hot loops are those
of steps 57 to 59: step 58 ran 0.0171 to 0.0174 s in 15 of its 19 board runs (fastest
run-0be3ea3875d4, 0.017071 s, 10.58 MB, 83.69 points; best points run-00bcea1e0afe, 0.017356 s,
9.66 MB, 83.90), and step 57 ran the fastest Track C time, 0.016978 s (run-c56b067ab64d). Details:
PERF.md, steps 49-60.

Per record, step 46 executed about 193 instructions in all, parse and deduplication together
(640.9M instructions for 3.32M records at 12 threads). The single largest stall is the join
itself: one random byte load into a 1 MB region table, which misses L2 on the box's CPU. With the
region index masked to 256 KB (wrong output, timing only), the replica ran 6.7% faster. The
submitted code hides most of that load instead: it prefetches the entry as soon as a record's
region id is known, and counts the record only after the next record is parsed (4.7% faster on
the replica than counting at once: step 48's hot path against step 46's, 198 of 201 pairs).

## The design

| Part | Bottleneck it addresses |
|---|---|
| Cuts from spec 2.2, found by each thread | A serial quote-parity pass over 202 MB |
| simdjson-style bitmaps per record, numbers in one AVX2 pass | Instructions and branches per record on hostile CSV |
| Two records in flight per loop; non-temporal prefetch | The random load into the 1 MB region table |
| Region table: one byte per region id | The join itself |
| Distinct names in one table per thread, buffers handed over | Atomics and cache-line traffic of one shared name table |
| Runs claimed one at a time, no barriers | One slow CPU holding up every thread |
| Input released in two calls per thread, small pooled buffers, tables dropped in parallel, exit without frees | Memory peak and the exit's serial unmap |
| Memory faulted in by four threads only, on one heap and pre-faulted stacks | The per-CPU charge stock that every faulting CPU adds to the memory peak |

**What the parser relies on.** Spec section 2.2 lists what the generator writes, for every seed
and scale, the holdout included, and says that a solution may rely on it. This one does:
- records have five fields;
- the id, region id and amount are never quoted;
- a name or note is either empty or quoted;
- there is no CR except in a CRLF terminator;
- a BOM only starts a record;
- a record's only `.` is its amount's.

The general grammar of 2.1 allows more: unquoted text, quoted numbers, a missing final terminator,
a BOM inside quotes after an LF. Input like that is not always caught. A moved record boundary
stops the program; other breaks of 2.2 are answered as parsed (see "Error handling" below, measured
against the reference).

**The lookup table.**
- *Size.* The join table has one byte per region id, for 2^20 ids: listed ids are below 1,000,000
  (spec 2.3), and every id above the table reads its last entry, which no listed id can set.
- *Filling.* Threads claim 64 line-aligned chunks of regions.csv as they start and store each
  region's group. A country's three letters, read as a base-26 number (`(a * 26 + b) * 26 + c`),
  index the table of codes already seen.
- *Groups.* `_UNMATCHED` is group 0, the first key of the group list. A country gets the next
  group, 1 to 240, the first time any thread sees it.
- *Output order.* The groups are sorted by code when the output is written, with `_UNMATCHED`
  last.

**Cutting records.csv without reading it twice.** Under 2.2, inside quotes an LF is always
followed by a letter or a quote, and a record terminator is followed by the next record's BOM or
id digit. So each cut goes after the first LF followed by a digit or a BOM byte, and every cut is
a record start.

**Parsing.** Each record is taken from the 64 bytes at its start, as simdjson does for JSON.
- *Bitmaps.* AVX2 compares give a bitmap of `"` and one of `,`. A carry-less multiply of the quote
  bits by all ones gives, for every byte, whether it is inside quotes (`masks()` returns all three
  bitmaps). What is left of the comma bitmap is exactly the field separators.
- *Unescaped lengths.* A quoted field's inside bits are its opening quote, its bytes and one quote
  of each `""` pair. So counting the inside bits up to a field's end gives its unescaped length
  without looking at its bytes. The quote bitmap is needed only for the carry-less multiply.
- *The tail.* One 32-byte load of the tail (`region_id,amount`) gives the region's comma and the
  amount's dot by two compares. The minus sign is one byte load, right after that comma.
- *The numbers.* One AVX2 pass converts all three: each number is the run of digits that ends its
  8-byte word.
- *Long fields.* About 10% of records need a second 64-byte window for the note, still on the fast
  path.
- *The walker.* A record whose name or note runs past both windows, and the last records of the
  file, go through a short walker: one `field()` scan reads each of the five fields, and a digit
  fold gives the numbers (an amount's digits without its sign and dot are its cents).
- *Two halves.* Each run is parsed as two halves at once, so that two records' work is in the
  core together.
- *Deferred counts.* As soon as a record's region id is known, it is clamped to the table and its
  join-table entry is prefetched, and the record is counted only after the next record is parsed:
  the table load has a whole parse to arrive.
- *The next record.* Every record ends with an LF, so the next record's start is the LF bit of
  the tail's 32 bytes, already loaded: the chain from one record to the next waits on neither the
  amount's dot nor a byte load.

**Distinct names per country.**
- *Identity.* A name is identified by its raw bytes between the quotes. Inside quotes, `""` maps
  one to one to the unescaped value, and the empty name is empty whether it is quoted or not, so
  equal values have equal raw bytes.
- *Hash.* `hash_words` hashes them 32 bytes at a time, with no branch on the length. The block is
  loaded whole and ANDed with 32 bytes taken from a sliding window into `KEEP` (32 bytes of ones,
  then 32 of zeros), so bytes past the name count as zeros. The state starts at zero; the block's
  first 16 bytes are XORed into it, its next 16 are the first AES round's key, and two more rounds
  with a fixed key follow, so every output byte depends on every input byte. The length needs no
  part of its own: a name has no zero byte (spec 2.2), so the zeros past it mark where it ends.
- *Long names.* Names over 32 bytes, none in the generator's pool, go to the walker, whose hash
  loops over further blocks. The walker hashes a copy of the name followed by 32 zero bytes, so
  that its loads stay in bounds. Since the mask zeroes everything past the name anyway, both paths
  give the same name the same hash.
- *Build check.* A `compile_error!` stops any build without AES, as for AVX2 and PCLMULQDQ.

Parsing files a pair (the hash, with the country's group in its low byte) under one partition per
thread, chosen from the hash's top 32 bits by a multiply-shift. Each partition is that thread's
table:
- *Keys.* A table holds 32-bit keys in buckets of 8 slots, at most 5/8 full. At 12 threads a table
  has 16,384 slots. A key's low 14 bits hold the name's row + 1 (never 0, which marks an empty
  slot); its high 18 bits are hash bits.
- *Probe.* A lookup starts at the bucket picked by 11 hash bits and compares all 8 keys of the
  bucket at once. Free slots are looked at only when no key of the bucket matches: slots fill in
  order and are never emptied, so a name is in the first bucket of its probe that has a free slot,
  or in one before it.
- *Check bits.* A key that matches still has to pass a check: 15 more hash bits are stored in the
  name's row of group bits, in word 3's bits 49..64. Those are the bits of groups 241..255, which
  never exist, since the spec has at most 240 countries (more aborts the program). The row is the
  one the lookup is about to set a bit in, so the check reads data it needs anyway.

Two names are confused only if they share every compared bit:
- *At 12 threads:* 11 bucket bits, 18 key bits, 15 check bits and the partition (log2 12 = 3.6
  bits), 47.6 bits.
- *On one thread:* a table has 262,144 slots. The bucket is pair bits 8..23 and the check bits are
  21..36, which overlap by 2 bits, so a thread compares 15 + 13 + 14 = 42 bits (at 2 threads the
  overlap is 1 bit; from 3 threads on, none).
- *Odds.* With the generator's 100,000 names, the chance that any two are confused is about
  C(100000, 2) / 2^47.6 = 2.4e-5 per dataset at 12 threads, and C(100000, 2) / 2^42 = 1.1e-3 on
  one thread. A name that spilled into the next bucket (about 1% at 12 threads) is also compared
  with that bucket's names, which raises the odds by under 2%.
- *Checked on our datasets.* A check program, kept outside the repo so that it does not count as
  code, uses this file's own `add`, hash and field reader on full seed 1, sample seeds 1 to 4 and
  holdout seed 99, at every thread count from 1 to 20. All the names' 64-bit hashes are distinct,
  `add` counts every distinct name exactly once in all 120 configurations, and every pair of names
  that agrees on the partition, key and check bits has home buckets at least 26 buckets farther
  apart than the longest run of full buckets. So on these datasets no two names can be taken for
  one, whatever the threads' timing. For other seeds, the odds above stand.
- *The trade.* Four partitions per thread (PERF.md, step 46) compared 49.6 bits (5.9e-6). One
  table per thread was 2.13% faster on the replica.

**Handing pairs over.**
- *Filing.* After each run it parses, a thread hands its buffer of pairs for each partition to that
  partition's owner, moving it into the owner's inbox rather than copying it.
- *Taking.* Its own table then takes what waits in its inbox. A table's row holds a bit per country
  for each name, and a thread's table stays in its cache. No two threads ever write the same table,
  so the hot path has no atomics.
- *Buffers.* Emptied buffers go back to one shared pool, from which a filer takes the empties for
  its next run (up to 12 at 12 threads) under one lock. A buffer holds a run's mean share of pairs
  plus 2 standard deviations plus 4. One that overflowed grew, and is dropped once emptied, so the
  pool's buffers stay small. The empty name (1% of records) always hashes to the same partition,
  which is the one that overflows.

**No waiting for a slow thread.**
- *Claims.* The file is cut into 64 runs per thread, which threads claim one at a time. A thread
  that loses its core to another process parses fewer runs, and its pairs wait in its inbox. Past
  the waits at the start (for regions.csv, and for the faulters' shares of the memory, technique
  8), no thread waits for another one until the very end.
- *Why.* An earlier version had a barrier after each of 8 rounds, and one CPU shared with a busy
  loop made it take twice as long (42 ms instead of 22 on the test server); now it takes 25.
- *Two threads on one CPU.* About one C run in four on the board was 20% slow: two of our threads
  shared a CPU while another idled, and the scheduler left them together. After the wait for
  regions.csv each thread now calls `move_off_shared_cpu`: it sets its CPU's bit (`sched_getcpu`) in one shared word; a thread that
  finds its bit already set moves to the lowest CPU of its affinity mask that no thread has
  claimed, and stays there for the rest of the run (`sched_setaffinity`). Only movers are pinned:
  pinning every thread at its start was worse (18 slow runs in 300 against 4), and a move happens
  in about 3% of runs. The mask is read once (`sched_getaffinity`), so a move never
  leaves the CPUs the program was given, and a failed call leaves the thread where it is. A normal
  run moves nothing. On the replica, runs over 19.5 ms went from 9 to 2 and from 12 to 6 in two
  sets of 300, and induced stacking from 43.96 to 20.57 ms (PERF.md, step 56).
- *Takeover.* If an inbox grows past 4 × threads buffers because its thread fell behind, whoever
  files into it takes over its table. If the owner holds its table just then, it has most likely
  lost its core to a filer, so the filer yields its core. Together these keep memory bounded.
  Before the yield (step 54), about 2% of runs grew inboxes of 113 to 178 buffers and peaked at up
  to 13.2 MB; with it, 200 runs peaked at 10.82 MB at most (PERF.md, step 54).
- *Stripes.* Claims go round the file's stripes (one per thread, about 17 MB each at 12 threads),
  so runs claimed at the same time lie a stripe apart and rarely share a page-table page, whose lock
  their page faults would otherwise contend on.
- *The end.* Once no thread files pairs any more, each thread drops the whole pages inside its
  share of the input's first two thirds (`drop_pages`, `madvise(MADV_DONTNEED)`: one call each).
  Then each thread drains the inboxes, its own table's first, and adds its totals to one shared
  sum (`merge`). Once all have, thread 0 writes the output and exits. The others meanwhile drop
  their own table's pages and a share each of the join table, of regions.csv and of the input's
  last third, which the exit would otherwise free one by one on one core.

**The code's layout.** `run` maps the files, derives the sizes, allocates the tables, faults in the
main thread's share of the memory and starts the workers with `pthread_create`; its doc comment
says who faults in what. Each thread runs `worker` (for workers 0 to 3, first their share of the
faulting, `fault_in`; then its share of regions.csv, its pair buffers, `move_off_shared_cpu`, and
the run loop with the hand-off and the takeover), which ends in `finish` (the flush barrier, the
input drop, the takes through `take`, the merge, then the write or the teardown). What the threads
share is one `Shared` struct, each field with its comment, which `run` sets once in `SHARED`
before any worker starts.

**Memory.** At 12 threads the program's own data is:
- the region table (1 MB);
- the name tables' keys (0.79 MB: 12 tables of 16,384 4-byte slots);
- the seen rows (3.26 MB: 100,000 names of 4 words each, a bit per possible group).

The pair buffers wait for about one run, so their size follows the run size. At step 59's peak,
the end of the parse, the run held 9.56 to 10.10 MB (the replica, `memory.current` against
`memory.stat` and smaps, 4 runs):
- 6.80 to 6.96 MB of anonymous memory: the rows 3.22, the join table 0.98, the keys 0.77, the pair
  buffers 0.89 and the stacks 0.30;
- 0.72 to 0.83 MB of kernel memory: page tables, kernel stacks and slab;
- 1.9 to 2.6 MB of the memory cgroup's per-CPU charge stock. A page fault is charged through its
  CPU's stock. A CPU whose stock cannot cover a fault charges a batch of 64 pages and keeps the
  unused rest, up to 63 pages (252 KB), for its next faults, and `memory.peak` counts those
  pages. All 12 threads faulted, so all 12 CPUs kept one.

Step 60 has only four threads fault in memory (technique 8), so only four CPUs keep a stock. On
the replica (mem.sh, four sets of 20 runs) the max was 8.54 to 8.69 MB and the median about 8.27
MB, against a max of 10.61 and 10.63 MB for step 59 in the same session.

## Techniques

### Index

One line per technique: what, where in `src/main.rs`, and why the output stays exact. All totals
are exact; the distinct-name count rests on a hash (technique 6 below).

1. Both files memory-mapped (`map`): pre-warmed pages are charged to the host. Exact: the dataset is read-only.
2. Transparent huge pages off (`prctl`): a stable memory peak. Exact: no effect on the bytes.
3. Every page the workers write faulted in by four threads only, the main thread and workers 1 to 3 (`FAULTERS`, `fault_in`; `populate`, which is `MADV_POPULATE_WRITE`, and `touch`, a byte read per 2 MB), on one malloc heap for all threads (`mallopt(M_ARENA_MAX, 1)`, grown 32 MB at a time by `M_TOP_PAD`) and on stacks the main thread has faulted in (`pthread_create` with `pthread_attr_setstack`): only four CPUs keep a cgroup charge stock, which the memory peak counts (technique 8). Exact: `MADV_POPULATE_WRITE` writes no byte, `touch` only reads, and the waits for the faulters (`faulted`, `made`) only order the work.
4. Cuts after the first LF followed by a digit or a BOM byte (`run_start`). Exact: by spec 2.2 such an LF ends a record.
5. Each thread finds its own cuts: no serial pass. Exact: the cut search is monotonic, so the runs cover the file once.
6. Tripwires: every half-run must end exactly at its cut (`assert_eq!` in `parse_run`). Exact on 2.2 input; input that moves a record boundary almost always stops here, and other breaks of 2.2 are answered as parsed ("Error handling").
7. Dense join table of 2^20 bytes, allocated zeroed (`group_of`, `REGION_IDS`). Exact: listed ids are below 1,000,000 (spec 2.3; a listed id of 2^20 − 1 or more panics in `fill`), and ids past the table are clamped to its last entry, which no listed id sets.
8. regions.csv filled in parallel from 64 claimed chunks (`fill`). Exact: chunks are cut at line starts, and each region id is stored once.
9. Country codes indexed by 26^3, numbered under a lock on first sight (`codes`, double-checked load). Exact: one number per code.
10. Groups numbered as first seen, sorted by code at output (`write_output`). Exact: byte order, `_UNMATCHED` last.
11. Byte-sized group indices into 256-entry tables: no bounds checks. Exact: a `u8` cannot index past 256 entries.
12. Region id clamped to the table's last entry (`parse_run`): no branch. Exact: that entry is 0, `_UNMATCHED`.
13. simdjson-style `"` and `,` bitmaps per 64-byte window (`masks`). Exact: one bit per byte, by compare.
14. Inside-quotes mask by a carry-less multiply with all ones (`pclmulqdq`). Exact: a `""` pair flips the state twice.
15. Separators by `tzcnt` and clear-lowest-bit on commas outside quotes (`fast_record`). Exact: a comma inside quotes is masked out (14).
16. Unescaped text length from the popcount of the inside-quotes bits, without reading the bytes (`fast_record`). Exact: inside a quoted field the set bits are its opening quote, its bytes and one quote of each `""` pair (spec 2.2: every inner quote is half of a pair), so the count less one is the unescaped length; an empty field has none.
17. `name_quoted` from the inside-quotes bit of the name's first byte. Exact: a quoted name's first byte is its opening quote, which counts as inside.
18. A second 64-byte window with the quote state carried over, for long notes. Exact: the carried state is the parity at the window's end.
19. One 32-byte tail load giving the region comma and the dot by two compares; the minus sign as the byte after the comma. Exact: the numbers are never quoted and a record's only `.` is its amount's (spec 2.2).
20. Three numbers converted in one AVX2 pass (`numbers`): each word byte-reversed by `pshufb` (the amount's dot dropped), each number kept as the bytes below the word's lowest non-digit byte by `(x − 1) & !x`, then `maddubs`/`madd`/`packus`/`madd`. Exact: each number has at most 8 digits, and the byte before it is not a digit (a BOM byte, an LF, a comma or a minus sign) unless it fills the word.
21. The next record's start from the LF bit of the tail's 32 bytes, already loaded (`fast_record`): no dot and no CR byte load on the chain from one record to the next. Exact: every record ends with an LF, and CR appears only in a CRLF terminator (spec 2.2); a record without its LF stops at the run's tripwire.
22. `vpmovmskb` as inline asm (`bytes_eq`). Exact: the same instruction, without LLVM's bit-by-bit rebuild.
23. Rare records (long fields, the last 256 bytes) to an out-of-line `#[cold]` walker (`walk_record`, `field`) that returns a boxed `Record`. Exact: it reads the same five fields byte by byte. Boxed, because a `Record` returned by value kept the fast path's `Record` in memory, and every field of every record was stored into it.
24. The two-cursor loop bounded by `fast_end`, so `fast_record`'s own bound test is known true and dropped. Exact: records within 256 bytes of the end take the tail loops, which keep the test, and then the walker.
25. Two halves of a run parsed in one loop (`parse_run`): two records' work in the core at once. The join-table entry is prefetched as soon as the region id is known (`record`), and each record is counted after the next is parsed (`last`), so the load has a parse to arrive. Exact: every record is counted once, the last one after the loop, and totals do not depend on record order.
26. Non-temporal prefetch of the record stream 1 KB ahead: more of the region table stays in L2. Exact: a prefetch reads nothing the program uses.
27. Branchy min and max: a new min or max is rare, so the branch predicts. Exact: min and max.
28. Name and note bytes packed in one column (`<< 32`). Exact: each is below 2^31 (spec 3).
29. Name identity from its raw bytes inside the quotes. Exact: `""` maps one to one to `"`; the empty name is empty quoted or not.
30. Branch-free AES hash: 32-byte blocks masked by a sliding `KEEP` window, three AES rounds, from a zero state (`hash_words`). Exact: equal names hash equal; names have no zero byte, so the zeros past a name mark its end; the odds that different names are taken as one are in technique 6.
31. The walker hashes a padded copy, so both paths give a name the same hash and every load is in bounds.
32. Partition by multiply-shift of the hash's top 32 bits: any thread count. Exact: equal hashes go to the same partition.
33. One name table per thread, written only by its owner: no atomics on the hot path. Exact: one writer per table.
34. Pair buffers handed to the owner's inbox, moved and not copied. Exact: every pair reaches its owner's table once (technique 7).
35. One shared pool of empty buffers; a buffer that grew is dropped once emptied. Exact: memory policy only.
36. Buffers sized at a run's mean share plus 2 standard deviations plus 4. Exact: a full buffer grows, so the size never drops a pair.
37. An inbox past 4 × threads buffers is taken over by its filer, which yields its core if the owner holds the table (`yield_now`): memory stays bounded. Exact: the takeover is under the table's lock, so a table still has one writer at a time.
38. Runs claimed one at a time, 64 per thread: no barriers; a slow thread parses fewer. Exact: each run is claimed once from a counter.
39. Claims round the file's stripes: concurrent runs rarely share a page-table page. Exact: the order of claims only.
40. Spin, then yield, in the five waits (`wait`). Exact: the wait ends only when the counter is reached.
41. 32-bit keys in 8-slot buckets, compared by one AVX2 compare; free slots tested only after no key matched (`add`). Exact: slots fill in order and are never emptied, so a name is in the first bucket of its probe with a free slot or before it, and a matching slot whose row bits are 0 is empty and ends the bucket's names.
42. The name's row + 1 inside its key: no second load after a hit. Exact: row + 1 is never 0, the empty-slot mark.
43. 15 check bits of the hash in the row's unused group bits (groups 241..255), compared in place (`(word ^ check) >> 49`). Exact: those groups never exist (more than 240 countries aborts).
44. A bit per group in each name's row: distinct names counted when a bit flips. Exact: a (name, country) pair flips its bit once.
45. Tables sized from the generator's 100,000-name pool with 8 standard deviations of room, at most 5/8 full; more names stop the program. Exact: it stops; it never drops a name.
46. Input pages dropped after the parse in two parts, one call per thread for each (`drop_pages`): the first two thirds before the last takes, the last third by the non-writers during the write; 23 TLB-shootdown rounds instead of one per block. Exact: `MADV_DONTNEED` on a private file mapping that is never written, so a later read faults the same bytes back in.
47. Totals summed under one lock as each thread finishes, counted by `merged`; once all are in, thread 0 writes and exits without frees, while the others drop their tables' pages and a share of the join table, of regions.csv and of the input's last third (`drop_pages`). Exact: sums are added, min and max taken; the output is complete before the exit.
48. The main thread is worker 0, and starts the others with `pthread_create` on 256 KB stacks it allocated (`STACK`, technique 8); a thread that finds another of ours on its CPU after the region wait moves to a free CPU of its affinity mask (`move_off_shared_cpu`, `sched_setaffinity`), and a failed call leaves it where it is. Exact: thread placement only.
49. `#[inline(never)]` on `parse_run` and `add`: edits around them cannot change their register allocation. Exact: code placement only.
50. Profile-guided build trained on generator seed 7, with atomic profile counters (Dockerfile). Items 50 and 51 change speed only: every check runs the Dockerfile's binary.
51. Hot loops aligned to 64 bytes; fat LTO, one codegen unit, `panic = "abort"`, `crt-static`, `target-cpu=native`; `compile_error!` without AVX2, PCLMULQDQ or AES.
52. Exit-code timing probes and in-process A/B probes on the box (PERF.md, "Box probes"). Measurement only, never a scored version.
53. No dependencies: libc calls declared by hand.

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
- **Why exact.**
  - *Escapes.* A `""` pair flips the quote state twice, so it leaves the mask unchanged, and an
    escaped quote is never taken as a field end.
  - *Lengths.* Inside a quoted field the inside bits are its opening quote, its bytes and the
    second quote of each `""` pair (the first flips the state off, the second back on). Every inner
    quote is half of such a pair (spec 2.2), so the field's unescaped length is its count of inside
    bits less one. An empty field has no inside bits, and the id and the commas are outside.
  - *The rest.* Records the windows cannot hold go to the walker, which reads the same five fields
    byte by byte.

### 3. The numbers in one AVX2 pass

- **Bottleneck.** Three digit loops per record, and a length per number to find its digits.
- **Design.**
  - *Tail.* One 32-byte load of the tail gives the region comma and the dot by two compares, and
    the minus sign is the byte after the comma.
  - *Words.* The 8 bytes ending at each number go into one vector.
  - *Shuffle.* A shuffle reverses each word's bytes, so that byte k weighs 10^k, and drops the
    amount's dot.
  - *Digits.* After subtracting `'0'`, digits are 0 to 9 and other bytes 10 or more. A number is
    the bytes below its word's lowest other byte: `(x − 1) & !x` on the non-digit mask keeps them,
    with no table and no length.
  - *Conversion.* `maddubs`/`madd`/`packus`/`madd` convert all three numbers at once.
- **Why exact.**
  - *Digit counts.* Ids are below 4 × 10^7 (spec section 3, "Magnitudes"), region ids are at most
    9,999,999, and amounts lie between −999.99 and 99,999.99 (spec 2.2). So each number has at
    most 8 digits, and the 8 bytes ending at it hold all of them.
  - *Word starts.* The byte before a number is a BOM byte, an LF, a comma or a minus sign, never
    a digit, unless the number fills its word.
  - *Cents.* An amount has exactly two decimals (spec 2.1), so its digits without the dot are its
    cents.
  - *Range.* The multiply-adds are exact for 8 digits (99,999,999 fits a 32-bit lane).

### 4. Two records in flight, deferred counts and prefetches

- **Bottleneck.** The region-table load is random over 1 MB and depends on the parse: 25-30% of a
  single thread's cycles sat on it, and on the box's CPU it misses L2. Counted right after its
  parse, the instruction after that load took 19% of `parse_run`'s cycles (perf record, replica).
- **Design.**
  - *Two halves.* Each run is parsed as two halves in one loop, so two records' work is in the
    core at once. The loop is bounded by `fast_end` as well, so the fast path's own bound test is
    known true and LLVM drops it.
  - *Deferred counts.* As soon as a record's region id is known, it is clamped to the table and its
    entry is prefetched; the count then reads the entry without a check. The record is counted
    after the next record is parsed: the first half's after the second half's parse, the second
    half's after the next pass's first record. On the replica this was 4.7% faster than counting at
    once (198 of 201 pairs against step 46's hot path). The prefetch alone gave 0.26%, and the
    deferral without the prefetch was 1.22% slower (PERF.md, steps 47-48).
  - *The next record's start* comes from the LF bit of the tail's 32 bytes, not from the amount's
    end and a CR byte load: −0.57% on the replica (PERF.md, step 49).
  - *Stream prefetch.* The record stream is prefetched non-temporally 1 KB ahead, so it takes less
    of L2 and more of the table stays there. Without it the replica ran 0.75% slower (PERF.md,
    experiments).
- **Why exact.** The totals, the min and max, and the set of (name, country) pairs do not depend on
  the order in which records are counted, and every record is counted once: the last deferred one
  after the loop. The second half starts at a cut found like the run cuts, and the first half must
  end exactly there (`assert_eq!`).

### 5. The join is one byte load

- **Bottleneck.** A hash map from region id to country would be a probe and a compare per record.
- **Design.** A dense table by region id, one byte per id: the group number, or 0, `_UNMATCHED`,
  for an id that is not listed. It is filled in parallel from 64 claimed chunks of regions.csv.
  The record's region id is clamped to the table's last entry.
- **Why exact.** Listed ids are below 1,000,000 (spec 2.3), so they fit the table's 2^20 entries;
  a listed id of 2^20 − 1 or more panics in `fill`. The last entry is never set, so every id
  above the table reads 0, `_UNMATCHED`. Each country's group number is handed out once under a
  lock, and the output sorts the groups by code.

### 6. Distinct names without atomics

- **Bottleneck.** One shared table needed a `fetch_or` per record and moved cache lines between
  cores (71 against 41 ms on the test server when it was tried again, PERF.md).
- **Design.**
  - *Hash.* A branch-free AES hash of the name's raw bytes.
  - *Partitions.* (hash, country) pairs are filed under one partition per thread and handed to the
    owner in buffers, moved and not copied.
  - *Tables.* Each owner's table compares the 8 keys of a bucket at once. The key holds the row,
    and 15 check bits sit in the row's unused group bits.
- **Why exact.**
  - *Equal names.* Equal unescaped names have equal raw bytes (inside quotes `""` maps one to one to
    `"`, and the empty name is empty quoted or not), so they get equal hashes.
  - *Different names.* They are counted as one only if their hashes agree on all 47.6 compared bits
    at 12 threads (42 on one thread): about 2.4e-5 per dataset at 12 threads and 1.1e-3 on one, for
    the generator's 100,000 names. These are upper bounds: two names taken for one change the
    output only if both occur in the same country, and then only that country's names_distinct, by
    1. It is not a proof; on the six datasets we have, a check with the program's own `add` shows
    that no two names can be taken for one at any thread count from 1 to 20 (design, "Checked on
    our datasets").
  - *History.* Step 33 moved from 64-bit to 32-bit keys for 0.56 MB (server median), and step 46
    from four tables per thread to one for 2.13% of time: together, 50 → 47.6 bits at 12 threads.
    The output matched the reference on every seed tried (PERF.md, "How the final number was
    verified").
  - *Limits.* More names than the tables hold, or more than 240 countries, stops the program.

### 7. No barriers

- **Bottleneck.** With a barrier after each round, one CPU shared with a busy loop made the run
  take twice as long: the board showed four runs of unchanged code at about 2x.
- **Design.** Runs are claimed one at a time from a counter, in stripe order. A thread that falls
  behind only lets its own inbox grow, and a filer that finds it past 4 × threads buffers takes its
  table over. Once every thread has added its totals, thread 0 writes the output.
- **Why exact.** Every run is claimed exactly once, every buffer is taken by exactly one table
  (under its lock), and every thread takes all inboxes once no thread files any more. The totals
  are summed under one lock, with min and max as min and max.

### 8. Memory and exit

- **Bottleneck.** The memory peak: the pair buffers, the name tables, and the charge stock that
  every CPU that faults keeps (1.9 to 2.6 MB of step 59's peak, "Memory" above); and the exit's
  unmap of the 202 MB input (about 6 ms on the box, measured by a probe).
- **Design.**
  - *Who faults.* Only four threads fault in the memory the workers write: the main thread and
    workers 1 to 3 (`FAULTERS`), never the other eight. Fewer threads would keep fewer stocks but
    cost far more time (6 threads: +26% for −1.5 MB; PERF.md, experiments).
  - *The main thread*, before it starts the workers: the join table (`populate`, which is
    `madvise(MADV_POPULATE_WRITE)`), the page tables of regions.csv (`touch`, a byte read per 2
    MB), the first 80 KB of the heap past `run`'s own allocations, and the top 24 KB of each
    worker's stack (`STACK_USED`).
  - *The faulters*, a share each (`fault_in`; a quarter from 4 threads on): the page tables of the
    input and the 1.25 MB heap margin (`MARGIN`, nearly all of it for pair buffers), which every
    thread's parse waits for (`faulted`); then their share of the name tables (three each at 12
    threads), rows populated and keys zeroed, which every thread's first take waits for (`made`).
  - *One heap.* Every thread allocates from the main thread's heap (`mallopt(M_ARENA_MAX, 1)`), so
    every allocation lands in faulted memory; `M_TOP_PAD` (32 MB) grows that heap 32 MB at a time,
    so the margin past its top is already mapped.
  - *Stacks.* The workers run on stacks the main thread allocated and faulted in: `pthread_create`
    with `pthread_attr_setstack`, since `std::thread` cannot take a stack. A stack is 256 KB
    (`STACK`), and a worker uses about 20 KB of it, with no recursion. Unlike the stacks pthread
    makes itself, these have no guard page.
  - *Rows.* Each table reserves rows for its share of the names plus 4 standard deviations (8,780
    at 12 threads; the fullest we measured held 8,650), all faulted in by its faulter; a table
    that needs more grows, on its owner's CPU.
  - *Checked.* A perf trace of each thread's user page faults, over 40 runs, found no write fault
    in any of the other eight workers. Replica, max of 20 runs: 8.54 to 8.69 MB against 10.61 and
    10.63 for step 59, for about 0.7% of time (PERF.md, step 60).
  - *Release.* Once no thread files pairs any more, each thread drops its share of the input's
    first two thirds in one call, and the threads other than the writer drop the last third while
    it writes: 23 TLB-shootdown rounds in all. Dropping each 8 runs' pages as they were
    parsed took 96, and the box's probe read the single release 0.1 ms faster (PERF.md, step 44).
  - *Buffers.* Runs are small (64 per thread), so pairs wait less. Buffers are sized from the run
    statistics and pooled, and 32-bit keys halve the tables.
  - *Exit.* Thread 0 writes the output and exits without freeing anything. Meanwhile the others
    drop their own name table's pages (about 83 each at 12 threads), a share each of the 1 MB join
    table and of regions.csv's 532 pages, and the input's last third, which the exit would
    otherwise free one by one on one core: about 1% faster on the replica over steps 51 and 52
    (PERF.md).
  - *Huge pages* are off (`prctl`), so that touching a byte cannot fault in a whole 2 MB page.
- **Why exact.** Faulting changes no data: `MADV_POPULATE_WRITE` writes no byte (a new page reads
  as zeros, as it would at its first touch), `touch` only reads, and the waits for the faulters
  only order the work; the same tables take the same pairs. Dropped input pages are never written,
  so a later read faults them back in from the page cache. That is why it is `MADV_DONTNEED` and
  not `munmap`. The tables and the join table would read back as zeros, so they are dropped only
  after every thread has added its totals: from then on only the sums and the country names
  (copies) are read.
- **If it fails.** On a kernel without `MADV_POPULATE_WRITE` (before Linux 5.14) the call fails
  and the pages fault in where they are first touched: about step 59's memory, the same output.
  Under contention a worker can still fault: with a busy loop on one CPU, the max of 15 runs was
  10.75 to 10.80 MB against step 59's 10.79 to 11.30 (PERF.md, step 60).

### 9. Build

- **Design.**
  - *PGO.* Trained on data from a byte-identical copy of the organisers' generator, with atomic
    profile counters, so that no count is lost while 12 threads train at once. The layout is still
    not bit-reproducible: six rebuilds of one source placed 0-14 instructions of `parse_run`
    differently, because the training threads' order varies.
  - *Flags.* Loops aligned to 64 bytes in the final build. Fat LTO, one codegen unit,
    `panic = "abort"`, a static binary, `target-cpu=native`.
  - *Code placement.* `#[inline(never)]` on `parse_run` and `add`, so that edits to the worker
    cannot change how their loops are compiled.
- **Why exact.** The build changes speed only; each check runs the Dockerfile's binary.

## Why the output is exact

Every total (count, id and amount sums, min, max, name and note bytes) is exact for every input
the generator writes (spec 2.2), at any thread count. `names_distinct` is exact unless two of the
dataset's names agree on all 47.6 compared hash bits at 12 threads (42 on one thread), with odds
of about 2.4e-5 per dataset (1.1e-3 on one thread); a check rules that out on all six datasets we
have. The arguments, gathered from the techniques above:

- **Cuts.** Under 2.2 an LF inside quotes is followed by a letter or a quote, and a record
  terminator by the next record's id digit or BOM, so every cut is a record start and the runs
  cover the file once (technique 1). Each half-run must end exactly at its cut, or an `assert_eq!`
  stops the program: a wrong cut can stop the run, never change its answer.
- **Fields and numbers.** A `""` pair leaves the quote mask unchanged, so an escaped quote never
  ends a field (technique 2). Every number has at most 8 digits under the spec's magnitudes, and an
  amount's digits without its dot are its cents, so sums are exact integers (technique 3).
- **The join.** Listed region ids are below 1,000,000 (spec 2.3) and fit the table's 2^20
  entries; every other id reads 0, `_UNMATCHED` (technique 5).
- **Distinct names.** Equal names have equal raw bytes and so equal hashes. Two different names
  are counted as one only if they agree on all 47.6 compared bits at 12 threads (42 on one
  thread): about 2.4e-5 per dataset (1.1e-3 on one thread). On full seed 1, sample seeds 1 to 4
  and holdout 99, at every thread count from 1 to 20, a check with this file's own `add` showed that
  no two names can be taken for one ("Distinct names per country").
- **Room for the names.** The generator writes at most 100,529 distinct names, and the tables are
  sized from its 100,000-name pool with 8 standard deviations of room, at most 5/8 full. The
  fullest partition we measured held 84.5% of its rows ("Robust on unseen seeds"); a table that
  filled up would stop the program, never drop a name.
- **Order and totals.** Totals, min and max and the set of (name, country) pairs do not depend on
  the order in which records are counted, and every record is counted once (technique 4). Every run
  is claimed once and every buffer taken by one table under its lock; thread 0 writes the output
  only after every thread has added its totals (technique 7).
- **Faulted memory.** Faulting pages in ahead writes no byte, and the waits for the faulters only
  order the work (technique 8).
- **Released memory.** Dropped input pages fault back in with the same bytes; the tables are
  dropped only after every total is in (technique 8).
- **Evidence.** The seed-1 hash at 1, 3, 7 and 12 CPUs, sample seeds 2-4 and holdout seed 99 at
  every gate, the 32 mutants, and the robustness runs on 30 fresh full-scale seeds and in 1,505
  more runs ("Robust on unseen seeds").

**The spec's edge cases (section 2.5), one by one.**

| Edge case | Where it is handled |
|---|---|
| 1-3. A BOM at the file's start, after a CRLF or an LF mid-file, and the record after one | The first is part of the header line, which is skipped. A cut may land after the LF before a BOM byte; the fast path's number words see a BOM byte, never a digit, before the id; the walker skips the 3 bytes |
| 4. CRLF and LF mixed | The next record starts after the first LF of the tail's 32 bytes, either way |
| 5. A comma inside quotes | The inside-quotes mask takes it out of the separators |
| 6. `""` escapes | A pair flips the quote state twice, and counts one byte |
| 7-9. One to three LFs in a note, with a comma and an escape | An LF inside quotes is followed by a letter or a quote, so no cut lands there; a second 64-byte window carries the quote state, and longer notes go to the walker |
| 10-11. Empty fields, unquoted or `""` | Length 0, and for a name the hash of the empty string, either way |
| 12. Trailing spaces in a name | Part of its raw bytes: of its hash and its length |
| 13. The last record, ending in CRLF | The last 256 bytes go to the walker, which reads CRLF or LF |
| 14-15. Negative amounts, -0.99 to -0.01, and 0.00 | The sign is the byte after the region's comma; the digits without the dot are the cents ("0.05" is 5) |
| 16. A region id in a hole, or above the range | Its entry is 0, `_UNMATCHED`; ids past the table are clamped to its last entry, also 0 |
| 17. `Betts` and `Betts ` in one country | Different raw bytes: two names, which the hash keeps apart but for the odds of technique 6 |
| 18. An unquoted and a quoted empty name in one country | The same empty bytes: one name |
| 19. A country with no records | Its count is 0, and the output skips it |
| 20. A country whose amounts all lie in -0.99..-0.01 | The min and max start at the far ends (`EMPTY`), so a negative max is kept, and the money format prints "-0.05" |

## Robust on unseen seeds

The scored data comes from the organisers' generator, and spec 2.2 states what it writes for every
seed and scale; the parser relies on that and nothing else. Nothing is fitted to seed 1's data.

| What could change with the seed | What the program does |
|---|---|
| Region ids and their count | A table of 2^20 entries for ids below 1,000,000 (spec 2.3); an id above it is `_UNMATCHED`, a listed one of 2^20 − 1 or more stops the program |
| Countries | Numbered as first seen, sorted at output; more than 240 stops the program |
| Names | Tables sized from the generator's 100,000-name pool plus 8 standard deviations; more stops the program. The odds of two names taken as one are per dataset, for any seed: about 2.4e-5 at 12 threads (technique 6) |
| Long names and notes, the file's end | The walker, which reads the same five fields byte by byte |
| Where the cuts fall | Each half-run must end exactly at its cut (`assert_eq!`) |

Evidence, on the binary the Dockerfile builds:

- **Holdout seed 99**, full scale, byte-equal to the reference at every gate since step 25,
  including the submitted code's.
- **Sample seeds 2-4** byte-equal; tiny seeds 11-14 and sample seeds 21-23 at four CPU sets.
- **Fresh seeds**, generated for those gates and checked with `reference/c_join.py`, at 1, 3, 12
  and 16 threads:
  - tiny 81 and 82 and sample 84 and 85 (steps 39 to 46);
  - earlier, tiny 61-63 and sample 64-66 (step 33), and 57, 58 and 71-73.
- **Hostile input**: 32 mutants (truncations, stray quotes, no terminator, bad regions, more than
  240 countries) exit 0 or stop with a message, never a segfault. Their exit codes were the same at
  every gate from step 33 to step 48. Since step 49, two mutants whose terminator LF is replaced by
  `x` stop the program where earlier steps wrote an answer; the reference rejects both inputs too.
  Since step 55, two more exit 0: one without a final terminator, with the reference's output, and
  one cut after a one-decimal amount, answered like any one-decimal amount, which the reference
  rejects.
- **Robustness runs on the replica** (Sun 1 Nov, about 17:00 UTC), on the exact Dockerfile build
  of step 52 (steps 53, 54 and 56 to 59 change no output path, step 55 only a last record without
  a terminator, which the generator never writes; all passed the gate; step 60's own runs are
  below), with data generated on the replica by the organisers' generator (Python 3.11, which
  reproduces the official sample seed 1 and its reference output byte for byte). Every run was
  byte-exact, with no crash, no nonzero exit and no output on stderr:
  - 30 fresh full-scale seeds (101-130) at 12 threads, seeds 101-102 also at 1 and 3 CPUs and
    seeds 103-104 at 2, 7 and 11: 40 of 40 runs;
  - 20 sha256-checked repeats per seed at 12 threads, and runs with a busy loop on 1, 2 or 4 of
    the twelve CPUs (seeds 109, 110 and 113): 660 of 660;
  - tiny and sample seeds 201-210 at 1, 2, 3, 7, 12 and 20 CPUs, each run as root and as uid
    65534 (the board's privilege drop) with read-only data, 3 repeats, and full scale at 12 and
    20 CPUs for both users: 840 of 840;
  - copies of seed 101 whose every file is an exact multiple of 4096 bytes, so that a read past
    the end of a file would fault, at 1, 3, 7, 12 and 20 CPUs: 5 of 5.
- **The submitted code, step 60** (Mon 2 Nov, the replica's build of the Dockerfile), which
  changes how the threads start, wait and fault: fresh full seeds 101-130 at 1, 3 and 8 CPUs (one,
  three and four faulters), as root and as uid 65534: 180 of 180 runs exit 0 with the reference's
  output and nothing on stderr; and with a busy loop on CPU 1, 2 or 5, 15 runs each, every output
  exact.
- **Room for the names, measured.** The generator writes at most 100,529 distinct names (100,515
  to 100,524 measured). The fullest partition held at most 8,650 of its 10,240 rows (84.5%) at 12
  threads, 61.4% at one thread and 51.4% at 20. A table that filled up would stop the program,
  never drop a name. Step 60 faults in 8,780 rows a table at 12 threads; a table that needs more
  grows, with the same output and a little more memory.
- **Time and memory on other seeds.** On the 30 fresh seeds step 52 ran 17.61 to 18.09 ms (medians
  of 7 runs). Seed 1, at 17.19 ms, is about 4% faster than a typical fresh seed. The memory peak
  on seeds 101-110 was 10.23 to 11.19 MB (max of 5 runs; seed 1: 10.51 MB). About one run in 50
  to 100 peaked at 11.1 to 12.1 MB, against a median of about 10.0 MB; step 54 fixed the cause
  (200 runs: max 10.82 MB).
- **Not covered:** more than 20 threads (the replica has 20 CPUs).

## Error handling

**Outside spec 2.2.** The parser assumes 2.2, which the generator writes for every
seed and scale. It checks what keeps every access in bounds and what moves a record boundary, not
every rule of 2.2. Measured on 21 mutants of sample seed 1, each changing one record, and on the
edge cases of the robustness runs, each run through this program and through the organisers'
`reference/c_join.py` (tests kept outside the repo):

| One record changed | Reference | This program |
|---|---|---|
| A lone quote in a note; the terminator LF replaced by `x` (two mutants of the gate) | rejects | stops (exit 134) |
| An LF inside quotes followed by a digit (2.1 allows it) | answers | stops (exit 134) |
| A sixth field | rejects | ignores it: the same output as the valid file |
| One field missing (the id, name, note, region or amount) | rejects | counts the record under `_UNMATCHED` |
| A BOM before the region id; two BOMs at a record start | rejects | answers as parsed |
| A dot in the region id; an amount with one decimal ("6.3") | rejects | answers as parsed |
| A stray CR before the CRLF; a CR as an unquoted note | rejects | answers as parsed |
| A quoted id or a quoted region id | answers | answers, with a different output |
| An unquoted non-empty name or note (2.1 allows them) | answers | wrong `name_bytes` and `note_bytes` (the latter wraps to about 2^32); counts, sums, amounts and `names_distinct` right |
| A last record without a terminator (2.1 allows it) | answers | the same output (steps 52 to 54 stopped: an index out of range) |
| A quoted amount; a CR or LF inside a quoted name; "06.35"; "-0.00"; a header with LF and no BOM | answers | the same output |

- *Cuts.* Each half-run must end exactly at the next cut (`assert_eq!`). A record boundary that
  moves (a lone quote, a lost LF) almost always ends a half-run somewhere else, and the assert
  stops the program. It is a tripwire, not a proof: for general 2.1 input, the step-24 version in
  PERF.md is the one to use.
- *Other panics.* A file that cannot be mapped, whose message names the OS error; indexing out of
  bounds, an unexpected header, a listed region id of 2^20 − 1 or more, more than 240 countries,
  and more distinct names than the tables hold. The build aborts on
  panic (`panic = "abort"`), so each one prints its message and the exit status is nonzero.
- *Bounds.* Every `unsafe` access is in bounds for any input, not only for 2.2 input: the fast path
  never reads outside the 256-byte margin it checks, and the walker hashes a padded copy of the
  name.

**The spec's RULES fixture exits nonzero, by design.** `tests/test_c_join.py` has a hand-written
`RULES_RECORDS` case for rules "the generator never exercises":
- a header with LF and no BOM;
- a BOM inside quotes after an LF;
- an unquoted empty note;
- a last record without a terminator.

It is valid 2.1 input but not 2.2 input. The program stops on it with exit 134, at the cut
tripwire ("records.csv is outside spec 2.2"): the first record's note holds an LF followed by a
BOM, which 2.2 rules out, and the cut rule takes an LF followed by a BOM as a record start, so a cut
lands inside that note. The reference answers it. We ran eight variants of the fixture (four forms,
each also repeated 300 times), with the same result. The `WORKED_RECORDS` examples are 2.2 input,
and their output matches the reference byte for byte, once and repeated 300 times. An earlier version also
handled general 2.1 input, with a byte-by-byte 2.1 parser and a second, single-run pass for any
wrong cut. That was about 115 lines. Since the scored data comes only from the generator, it was
removed (PERF.md, step 25).

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise`, `mallopt`, `prctl`, `sched_getcpu`,
`sched_getaffinity`, `sched_setaffinity`, `pthread_attr_init`, `pthread_attr_setstack` and
`pthread_create` declared by hand. The build needs AVX2, PCLMULQDQ and AES (the Dockerfile builds with `target-cpu=native`);
without them a `compile_error!` stops it.

## With another 24 hours

- **A smaller region table with one load.** Masking the region index to 256 KB (wrong output,
  timing only) was 6.7% faster on the replica, before the deferred counts hid part of that load.
  The compact exact forms we built (a bitmap with ranks, 32-id blocks) need a second dependent load
  and were slower. A one-load structure under 256 KB is the open question.
- **`add` in batches.** Prefetching the rows of 32 pairs at a time was 1.20% faster on the replica,
  for 18 more lines, about break-even in points (PERF.md, experiments). A shorter form of it would
  pay.
- **A dot-anchored kernel** (find every record's `.` per 64-byte block first, then parse each
  record from its start and its dot, with no chain from one record to the next) was exact on every
  dataset but came out 13% slower and used 21% more instructions in two attempts (PERF.md). A
  version whose second stage compiles to fewer instructions than today's fast path might still win.
