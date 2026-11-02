# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).
The step-by-step history, with a board run for every step, is in [PERF.md](PERF.md).

## What the program does

It reads `requests.log`: 3.1 million lines (239 MB) of
`timestamp method endpoint status latency`. It writes `summary.txt`: one line per (UTC hour,
endpoint) group, 126,908 lines (10.1 MB) for seed 1. Each line has:
- the count;
- the nearest-rank p50, p95 and p99 latency;
- the 5xx count, and the 5xx rate in parts per million.

The output is sorted by hour, then by endpoint in byte order.

Two things make the problem harder than it looks. The lines are not in time order: eight shards
are interleaved in arrival order, and the spec promises no bound on the disorder. And exact
percentiles need every latency of a group, not a running summary.

## Where the time goes on the box

The reference runs 32.05 s at 228 MB and S 56. The submitted code (PERF.md step 74, ar-5, 0098691,
S 441) is step 71 with the first slices sized by start order and one release per pair of slices
(step 72), std's start-up skipped (step 73) and the Dockerfile's `RUN` on fewer lines (step 74):
about −1.7% against step 71 on the replica. Its first board run, run-977a4610bb91 (0.015606 s,
18.23 MB, 87.06 points), is the best Track A run and the fastest Track A time. Step 71 (al-6,
ccc90a3, S 438) is step 69's engine in 18 fewer lines, cut in steps 70 and 71 at the same speed; its
second board run, run-89081c753251 (0.015752 s, 18.32 MB, 86.96 points), was the best Track A run
until step 74. Step 69 (am5-2, abc7b44, S 456) is step 68 with the endpoints' name order sorted
after the merge's hand-off, while the other threads start pass 2. Its first board run,
run-a533307c210b (0.018707 s, 18.62 MB, 84.89 points), was a slow run: Track A alone was slow in
that dispatch, and B set its best time then in the same dispatch (run-567ede8911b8, 0.074086 s). Its
second, run-b3a4984e4320, took 0.015715 s (18.22 MB, 86.88 points), and its third, run-dcb2c65578ff
(0.015702 s, 18.24 MB, 86.88 points), was the best Track A run until step 71 and the fastest Track A
time until step 74. Step 68 (am5-1, 9f2993f, S 452), step 67's engine with the endpoint table in
static memory, a cheaper cold lookup and the space window 3 bytes later, had them before it:
run-be74feabcc76, 0.015722 s, 18.44 MB, 86.85 points. Until then, step 64 had the best run
(run-1bda764d4b1e, 0.016043 s,
18.67 MB, 86.65 points) and step 63 the fastest time (run-219d1a5f89d0, 0.016017 s, 86.64 points)
(details: PERF.md, steps 51-74). On the replica of the scoring box (the same CPU, timed by the
board's own timer; PERF.md, "How we measured"), step 53 ran at a median of 16.856 ms over 201
pairs and steps 54 to 56, 59 and 60 within 0.3% of it; step 61 measured −0.69% against step 60,
step 62 the same as step 61, step 63 −1.07% against step 62, steps 64 to 67 the same speed as
step 63, step 68 −1.18% and −1.19% against step 67, step 69 −0.92% and −0.86% against step 68,
steps 70 and 71 +0.075% and −0.23% against step 69 (means of four and eight pairings), step 72
about −0.62% against step 71, step 73 −1.08% against step 72 and step 74 the same as step 73.

Probes are temporary builds that exit with a measured time as their exit code, because the board
reports only T and the exit code of a failed run (PERF.md, "Box probes"):

- The parse ends at 10.1 ms (run-500e61325035, exit code 101, 0.1 ms units).
- A build that only touches one byte per 64 of each slice ends at 7.8 ms (run-be516aa463a5, exit
  code 78). That is the floor of reading 239 MB from DRAM and taking its page faults.
- With 6 threads instead of 12, the same touch takes only 5% longer (run-c08713cbca4a, exit code
  105). So memory sets that floor, not the cores.
- The parse itself is core-bound: with one thread per core (6 threads) it ends 31% later on the
  replica (12.62 against 9.63 ms, medians of 15 runs of a build that prints its phase ends), while
  the touch above took only 5% longer. The rest of the run took 19% longer.

So the parse runs about 2.3 ms above the memory floor, and about 6 to 7 ms of T comes after it:
the merge, pass 2, the write and the exit. The probes measured step 43's parse; steps 44 to 60
changed pass 2, the writer and cold code, and steps 61 to 63 only shortened the parse loop's
source, at the same speed on the replica. Step 68 started the space window 3 bytes later and made
the parse's cold lookup cheaper. Steps 70 and 71 shortened the source of the parse and the lookup
again, at the same speed. Step 72 made the first pair of slices 5% smaller and gives each pair's
input back in one call.

Pass 2's parts, on the replica (builds that skip one part, wrong output, timing only; 201 pairs
each, PERF.md):
- the selections of the groups of more than 64 values: 1.15 ms, of which p50's 0.70;
- writing the lines: 1.10 ms, of which output page faults and stores 0.76 and formatting 0.35;
- rank counting for the groups of up to 64 values: 0.75 ms;
- the scatter of each hour's values: 0.47 ms.

Memory is the other axis. About 4.7 MB is the container's floor (an empty input). The output
counts, because `/out` is a tmpfs. The mapped input does not: the harness has it in the page cache
already, charged to the host. The peak came 2 to 3 ms into pass 2, while records of 32 of the 48
parts were still alive. Since step 61 the output waits until the records given back make room for
it (*Gate*, below), and in 7 of 10 replica runs of step 68's code the peak is the start of pass 2,
when each thread's first scatter faults in its values buffer. The expected highest of five peaks
is 18.42 and 18.46 MB on the replica for two builds of the submitted source (40 rounds), against
19.49 MB for step 60. Each run's peak also includes 0 to 3 MB of the kernel's per-CPU charge
batches, 2.3 to 3.1 MB at the peak in those 10 runs (PERF.md, "What we learned about the box").

## The design

The program has five parts. Each one exists because of one bottleneck.

| Part | Bottleneck it addresses |
|---|---|
| Mapped, padded input; slices taken in pairs from a shared counter | 239 MB to read once, with no copy, no memory charge and no end-of-file special case; a thread slowed by a shared CPU |
| Two slices parsed side by side; a branch-free AVX2 line parse; endpoints found where they lie | Each line's start waits on the previous line's end; branch mispredictions on the method and endpoint length; instructions per line |
| 4-byte records, hours as runs | Memory per record; a calendar per line; unbounded disorder |
| Pass 2 by hour: counting sort, rank counting, selection | Cache misses of a global sort; full sorts for three ranks |
| Output written in place, out of order; no teardown | Serial writes; page faults on one file lock; freeing and unmapping at exit |

**Parse (pass 1).**
- *Input.* The log is mapped, followed by 256 zero bytes, so that fixed-size vector loads near its
  end stay inside the mapping.
- *Slices.* The log is cut at line ends into four slices per hardware thread. Threads take them in
  file order, two at a time, from a shared counter, so a thread slowed by a shared CPU parses
  fewer of them. Each thread finds the cuts of the slices it takes. The first round's pairs shrink
  in start order, and the first pair is 5% smaller again: its thread adds most of the endpoints to
  the table while the others are still being spawned.
- *Two slices side by side.* A thread parses its two slices one line of each in turn. A line's end,
  and so the next line's start, waits on that line's loads; the other slice's line fills the wait.
- *Field ends.* The timestamp is a fixed 27 bytes and every method has 3 to 7 letters, so one
  64-byte AVX2 window from byte 31, three letters into the method, holds the spaces that end the
  method and the endpoint. Their positions come from `tzcnt`, with no branch on the method or on
  the endpoint's length.
- *Endpoint lookup.* The endpoint is looked up where it lies. A CRC of its length and its first and
  last 8 bytes picks its slot in one table that all threads share, a zeroed static whose pages
  fault in only as the parse touches them. One vector compare against the
  stored key (the endpoint, a space, then zeros to 128 bytes) confirms it. Endpoints that end past
  the window, misses and new endpoints go to a cold path that builds the key.
- *Inserts.* Lookups are plain loads; the rare insert claims its slot by compare-and-swap, so no
  tables are merged after the parse.
- *Latency.* A latency of up to 6 digits (98.8% of lines) is converted where it lies, by two
  multiply-adds whose weights depend on its length. Longer ones take a cold, out-of-line parser.
- *Records.* A line becomes one `u32`, written into one records array per thread. The array is
  allocated zeroed for the worst case, so only written pages get memory.
- *Release.* Each thread gives back the pages of every pair of slices it has parsed, in one call
  per pair.

**Merge.** One pool of threads runs both passes, and the main thread is one of them: as many
threads as CPUs. Each thread counts itself done when it runs out of slices (`parsed`), and the
last one runs the short serial merge (`merge`):
- it sorts all runs by hour;
- it sets each slice's reader count to its number of runs;
- it maps the output, then publishes all of this as one hand-off (`merged`), on which the other
  threads sleep.

While the others start pass 2, it unmaps the log (which frees its page tables too), sorts the
endpoints (`names`) and publishes their order (`order`), gives back the endpoint keys, then starts
pass 2 too. An hour waits for the order only after its count and scatter, when it builds its
groups in name order. The sort and its collect took 0.16 ms of the 0.275 ms serial merge when the
merge did them, and every other thread waited for them; with the sort after the hand-off, the run
measured 0.9% faster on the replica (PERF.md, step 69). Until step 62 the merge ran on a 13th
thread that waited for the 12 workers. On the replica, 21 to 43 of 300 runs were then more than 8% slower
than the median; with the main thread as a worker, 6 to 10, at a median 0.9% lower (PERF.md,
step 63).

**Pass 2.**
- *Work order.* Threads take whole hours from a counter, in blocks of one hour per thread, the
  largest hour of each block first.
- *Grouping.* A thread gathers an hour's runs from every slice and groups the latencies by
  endpoint id with a counting sort whose arrays fit in L1/L2 (an hour is 14K to 73K records).
- *Release.* A slice's records are given back by the hour that is the last to read them.
- *Percentiles.* Groups of up to 64 values (95% of the groups) get their p50, p95 and p99 by
  counting ranks with vector compares; larger groups by three nested selections.
- *Hand-off.* The thread keeps only the statistics (24 bytes per endpoint) and the hour's exact
  output length. It publishes the length and takes the next hour.

**Write.**
- *Placement.* The output file is sized to a bound and mapped shared. Once the lengths of all
  earlier hours are published, an hour's start is their sum, and the thread holding the hour
  writes it straight into the mapping.
- *Gate.* While hours are still being computed, a thread writes an hour only once the records
  given back so far cover the output already written, the hour's own bytes and three quarters of
  what the threads' values buffers can hold (`flush`). When each hour was written as soon as its
  start was known, the output piled up on the records and the values buffers 2 to 3 ms into pass
  2, where the memory peak was. On the replica the expected highest of five peaks fell from 19.49
  to 18.50 MB (40 rounds, with step 61's line cuts) at the same speed; gating on all of the
  buffers' room took 0.4 MB more off but cost 0.9%. A thread with no more hours to take writes
  its pending ones ungated once their starts are known, so no hour waits for ever. The gate
  changes only when an hour is written, never its bytes.
- *Lines.* Each line is written from its end, backwards, two digits at a time from a table.
- *Waiting.* A thread whose hours are all written sleeps on the lengths it still needs.
- *Exit.* Once every thread has written (a barrier), each drops the page-table entries of its share
  of the output mapping, so the exit does not tear them all down on one core. The program then cuts
  the file to its length and exits without freeing anything.

## Techniques

### Index

One line per technique: what it is, where it is in `src/main.rs`, and why the output stays exact.

1. Memory-mapped input (`main`, mmap): pre-warmed pages are charged to the host. Exact: the dataset is read-only.
2. Zero pad by an anonymous reservation with the file mapped over it (`MAP_FIXED`, `PAD`): full-width loads at any line. Exact: the pad is zeros and every load is bounded (SAFETY in `parse`).
3. Transparent huge pages off (`prctl`): a stable memory peak. Exact: no effect on the bytes.
4. Line-aligned slices, four per thread, taken two at a time from a shared counter (`cut`, `claim`), the first round's pairs sized by start order and the first pair 5% smaller (`w`, `at`): no serial cut pass, a slowed thread parses fewer slices, and the first thread, which adds most of the endpoints to the table while the spawns go on, gets less to parse (PERF.md, step 72). Exact: cuts follow an LF and never decrease; the slice count is even, so a pair's second slice always exists.
5. One thread pool for both passes, the main thread included (as many threads as CPUs), handed over through `OnceLock` (`parts`, `merged`, then `order`); the last thread done with pass 1 merges (`parsed`): no join, spawn or 13th thread between the passes. Exact: the counter's acquire-release and `OnceLock::wait` order every parse before the merge, and the name sort before any hour reads the order.
6. Two slices parsed side by side, one line of each in turn (`parse`, `step`): two independent line-to-line dependency chains. Exact: each slice keeps its own cursor, records, escapes and runs.
7. Hour change from one 8-byte compare of `MM-DDTHH` (`step`, `last`). Exact: all timestamps lie in one 72-hour span (spec).
8. Hour key = the timestamp's first 13 bytes as a big-endian number. Exact: fixed width, so byte order is time order, and the bytes are the output label.
9. Runs of lines instead of an hour per record (`Part`, `Start`, `Run`). Exact: every run of an hour is gathered from every slice, so no disorder bound is assumed.
10. One 64-byte AVX2 space mask from byte 31, three letters into the method that follows the fixed 27-byte timestamp (`window`): 6.6% of seed 1's lines end past it, against 7.6% from byte 28 (PERF.md, step 68). Exact: fields are separated by one space, and every method has 3 to 7 letters, so bytes 28 to 30 are letters and the method's space is at byte 31 to 35.
11. Field ends by `tzcnt` and clear-lowest-bit, no branch on the method or the length. Exact: the first two set bits from byte 31 are the method's and the endpoint's spaces (10).
12. `vpcmpeqb` and `vpmovmskb` by the plain intrinsics, in one helper since step 70 (`eq_mask`). At step 9 LLVM rebuilt the 64-bit mask bit by bit from the intrinsic, without AVX-512, and inline asm avoided it; in the code of step 53 the intrinsic compiles to the same hot loop as the asm, and the Dockerfile's build has no `vpinsrb` (PERF.md, step 53). Tracks B and C keep the inline asm. Exact: the same instructions.
13. Endpoint lookup where it lies (`Interner::find`): the home slot from two CRC steps over the length and the first and last 8 bytes (`home`), then the stored key compared over bytes 0..=len (`eq_mask`, `bzhi`). Exact: the stored key is the endpoint, a space, then zeros, so a match of bytes 0..=len means the same bytes and the same length; the hash only picks the slot.
14. Cold path for endpoints past the window, misses and inserts (`Interner::id`, `#[cold]`, `#[inline(never)]`; the insert itself, `Interner::add`, is not marked cold, as `id` is its only caller): 128 bytes loaded, the space mask built from two 64-bit halves (`half`), the key built with a sliding `KEEP` mask, the hash's two 8-byte words read from the line and cut by `bzhi` to a short endpoint and its space (not read back from the key just stored, which waited on store forwarding), linear probing from the home slot, and per slot four equality masks ANDed (`eq_mask`). Exact: the hash's words are the key's bytes at the same offsets, for every length, and the whole 128-byte key is compared.
15. 128-byte keys: an endpoint of up to 125 bytes, its space and zeros (`Key`). Exact: no endpoint byte is a space or zero, so equal keys mean equal endpoints, at any length.
16. Tripwire `assert!` on the endpoint length (`MAX_NAME`, 125): malformed input stops instead of reading past the pad. Exact: it stops; it never guesses.
17. One lock-free open-addressing table shared by all threads, ids published by compare-and-swap (`Interner::add`); a thread keeps an id that lost its slot for its next insert (`SPARE`, a thread-local `Cell<Option<usize>>`). Exact: a losing id is never in a record, and at most one id per thread is unpublished, so the 2,047 ids hold the spec's 1,842 endpoints for up to 205 threads.
18. 8x slots per possible id (11% load): almost every lookup is one probe. Exact: the load changes the probe count, not the answer.
19. A slot holds the id as a record holds it (`id << 21`), so a lookup needs no shift. Exact: the same bits.
20. Endpoint bytes copied at insert: the input's pages can be released early. Exact: names are compared and printed from the copies.
21. The table a zeroed static (`TABLE`, in .bss): nothing clears it, and its pages fault in only as touched (`Box::new_zeroed` of this 32-byte-aligned type was an allocation and a 0.13 ms memset on the main thread before the first spawn; PERF.md, step 68). Exact: a zero slot means empty; ids start at 1, and id 0's key stays zero, which no endpoint matches.
22. Latencies of up to 6 digits converted where they lie by `maddubs` and `madd` with per-length weights (`DIGITS`, a literal table), the length from the LF's place in one 16-byte load: no shuffle, no multiply chain. Exact: weights are 0 past the digits, and the table holds exact place values.
23. Longer latencies (7 to 10 digits, 1.2% of lines) by `parse_digits`, digit by digit over the count that the LF mask gave, `#[cold]` and out of line, which also pushes the escape. Exact: at most 16 bytes are read, and the value is accumulated in a `u64`, so 10 digits cannot overflow. The SIMD version it replaced was no faster on the replica (PERF.md, step 53).
24. 4-byte records `id << 21 | 5xx << 20 | latency`. Exact: 11 bits hold the ids; every latency of 7 or more digits escapes (25); up to 6 digits fit 20 bits.
25. Escape list for every latency of 7 or more digits, pushed by `parse_digits`, which returns the marker 2^20 − 1. Exact: up to 6 digits stay below the marker; escapes are read back in order, run by run, from each run's first escape index.
26. Worst-case pre-sized records, one array per thread for all its slices (`free`), written through raw pointers: no capacity checks, and untouched pages cost nothing. Exact: a line moves the parse on at least 38 bytes whatever its bytes (`MIN_LINE`: the endpoint ends at byte 32 or later, then its space, the status, a space and the LF), so a slice of n bytes holds at most n / 38 + 1 records, and an assert checks the room.
27. 5xx from the status's first digit. Exact: a status is 5xx exactly when it starts with 5.
28. Input pages dropped by each parse thread after each pair, in one call for the pair's two adjacent slices (`release`, `MADV_DONTNEED`; every call is a round of TLB shootdowns): no single unmap (the exit's, or now the merge's `munmap`) drops 58,000 pages on one core. Exact: the log is a read-only file mapping, so a dropped page that is read again (by a later `cut`, for one) faults back in unchanged.
29. Log unmapped by the merging thread right after the hand-off and before the name sort, while the others start pass 2: its page tables are freed off their path, before their values buffers fault in. Exact: nothing reads the log after the parse; the names were copied (20).
30. Endpoint keys released after the sort. Exact: no key is read after it.
31. Endpoint sort by the first 32 key bytes as two big-endian `u128`, then the name, done by the merging thread after the hand-off (`names`) and published in `order`. Exact: byte order; the space after an endpoint is below every endpoint byte, so a proper prefix sorts first; the order is set once, and every hour waits for it before it builds its groups.
32. All runs sorted by hour, in a vector presized to the parts' starts, and cut into hours with `chunk_by` (`merge`, `same_hour`). Exact: every run of an hour, from any slice, lands in that hour, whatever the disorder, and the order of an hour's runs changes no output byte: each run reads its own escapes, and a group's values are only counted or selected.
33. Hours queued in blocks of one per thread, largest first: small hours do not pile up behind a big one. Exact: it orders the work; each hour's place in the file comes from the published lengths (42).
34. Per-hour counting sort with count and 5xx count packed in one `u64`: one add per record. Exact: each group's count is zeroed as it is taken (`std::mem::take`), and every id a record can hold is in the endpoint list, so each hour starts from zeros.
35. Scatter without bounds checks (SAFETY: the counts are this hour's). Exact: each id's cursor starts at its group's start and ends at the next id's start, at most the hour's record count.
36. Record pages released per slice once all its runs are placed (`readers`: the merge sets each part's count to its number of runs, and pass 2 counts it down once per placed run): one `madvise` per slice, because each call is a round of TLB shootdowns. Exact: the count reaches zero only when the part's last run has been placed (`AcqRel`), so nothing reads the records after their release.
37. Percentiles of groups of up to 64 values by counting ranks (`small_ranks`): masked loads into 1, 4 or 8 vectors, one compare per value and vector, and the value at rank r taken as the largest value with at most r values below it. Exact: that is the definition of the value at rank r; the zeros past the end are never counted.
38. Larger groups: p50, p95 and p99 by nested `select_nth_unstable`, each read right after its own selection (`stats`). Exact: ranks `ceil(p * n / 100)`; the next selection reorders the slice.
39. Scratch sized once for the largest hour and allocated zeroed. Exact: every hour fits in it.
40. Pending statistics as six `u32` per endpoint (24 bytes), the rate computed once in pass 2 (`stats`). Exact: the rate is the integer `errors * 1000000 / count`.
41. Each hour's exact output length computed first (`line_len`: the hour label and its space, 21 bytes, `LABEL`; the name; each number after a space; LF), and the writer asserts it. Exact: the length comes from the same six numbers the writer prints, and a mismatch stops the program (`assert_eq!`).
42. Out-of-order compute, in-order placement: an hour's start is the sum of the published lengths before it. Exact: each hour's length is published once, so every hour's byte range is fixed and disjoint from the others, in whatever order the hours finish.
43. Output mapped shared at a bound, truncated to the total at the end: no write calls on the tmpfs inode lock. Exact: no output exceeds the bound (hours × endpoints × 213 bytes, the longest possible line), and the file is cut to the sum of the lengths.
44. Lines written backwards, straight into the mapping (`write`, `PAIRS`): digit pairs while the value is 100 or more, then the last one or two digits as one pair, with a lone digit's leading 0 overwritten by the space before the number. No stack buffer and no copy, so no store-forwarding stall. Exact: the writer asserts that each hour's lines end exactly at the hour's start.
45. Exit without teardown (`process::exit`). Before it, the threads drop the output mapping's page-table entries in parallel after a barrier (`written`, `share`); the pages stay in the file. Exact: the output is complete in the page cache before the exit, and `MADV_DONTNEED` on a shared file mapping drops only page-table entries.
46. Profile-guided build trained on generator seed 7, not the scored seed (Dockerfile). Items 46 to 51 change speed only: every correctness check runs the binary this Dockerfile builds.
47. Atomic profile counters: the training run's threads no longer lose counter updates. Builds still differ a little in block order, because some counts depend on timing, so every A/B uses two builds a side.
48. Hot loops aligned to 64 bytes (`-align-loops=64`). Kept because A is not slower with it and C's best run uses it (PERF.md, steps 34 and 36).
49. Fat LTO, one codegen unit, `panic = "abort"` (Cargo.toml).
50. Static binary (`crt-static`).
51. `target-cpu=native`, with a `compile_error!` without AVX2.
52. Exit-code timing probes on the box (PERF.md, "Box probes"). Measurement only, never a scored version.
53. No dependencies: libc calls declared by hand.
54. Output writes gated on the records given back (`flush`, `given`, `wrote`, `held`): the output takes the records' place instead of stacking on them at the peak. Exact: the gate only delays a write; the last flush is ungated, and each hour's place comes from the published lengths (42).
55. std's start-up skipped (`#![no_main]`, our own C `main`): no stack-guard lookup through /proc/self/maps, no signal handlers or alternate signal stacks (in the spawned threads either), no SIGPIPE setting and no check of fds 0 to 2: about 175 µs, 1.08% of T on the replica (PERF.md, step 73). Exact: nothing here uses what the start-up sets up; the arguments arrive as before (glibc passes them to std), and a panic still prints its message, then aborts.

Each technique below names the bottleneck, what the design does about it, and why the output
stays exact for every seed of the generator.

### 1. Mapped input with a zero pad

- **Bottleneck.** Reading 239 MB into a buffer would cost 239 MB of memory and a copy. Vector
  loads near the end of the file would need a slow tail path.
- **Design.** `mmap` of the pre-warmed file: no copy, and its pages are charged to the host.
  - First the range `len + 256` is reserved as anonymous zero pages.
  - Then the file is mapped over its start with `MAP_FIXED`.

  Every line can then be read with full-width loads.
- **Why exact.** The pad is zeros, and every load is bounded. The SAFETY comment in `parse` adds
  up the offsets: the furthest load ends 242 bytes after a line's start, within the 256-byte pad.
  The dataset is mounted read-only, so the bytes cannot change under the mapping.

### 2. Line-aligned slices, taken in pairs from a shared counter

- **Bottleneck.** A serial scan for line ends would be a pass over 239 MB on one core. And with
  one fixed slice per thread, the scheduler sometimes put two of the 12 threads on one CPU for the
  whole parse, so the merge waited for that pair. On the test server (dev build), 4 of 30 runs
  finished the parse 4.9 to 8.0 ms apart and took 18.1-20.8 ms, against 14.6-15.6 ms for the rest
  (PERF.md, step 38).
- **Design.** There are `slices = 4 * threads` slices, taken in file order, two at a time, from a
  shared counter.
  - Cut `p` is just after the last LF before byte `at(p)`: `len` times the share of slices 0 to
    p − 1 in the weights `w(q) = 200 + threads.saturating_sub(q / 2) - 10 * (q < 2)`. Each pair of
    the first round (one a thread, claimed in start order) is half a percent smaller than the one
    before it, the first pair is 5% smaller again, and the second round is even. The first thread
    adds most of the endpoints to the table while the others are still being spawned, and its
    first pair took 4.65 ms against 4.23 to 4.43 ms for the others. The 5% cut is the gain (the
    sizing measured −0.275% on the replica); the slope adds nothing measurable (PERF.md, step 72).
  - A thread computes the cuts of the slices it takes, so a slowed thread just parses fewer of
    them.
  - Each thread keeps one records array for all its slices.

  Four pairs per thread were 0.3 ms slower than two on the test server (61 interleaved runs),
  because of the per-slice work, mostly each slice's release. On the replica of the box, two pairs
  were 1.2 to 1.4% faster than four (PERF.md, step 51).
- **Why exact.** Cuts never decrease and always follow an LF, so every line is in exactly one
  slice. Two cuts in one line give an empty slice, which is harmless. Parts are indexed by slice,
  not by thread, so which thread parsed a slice does not matter. The thread count changes the
  slices, the runs and the order of endpoint ids, but none of them reaches the output: the output
  was checked byte for byte at 1 to 16 threads.

### 3. Two slices side by side

- **Bottleneck.** In one slice, a line's start is the previous line's end, which waits on that
  line's loads: the parse of one slice is a chain of dependent steps, and the core idles while
  each step's loads arrive.
- **Design.** `parse` takes two slices and runs one line of each in turn, in one loop until both
  are done (`while step(0) | step(1) {}`; a finished slice's step returns false). The two chains
  are independent, so the out-of-order core
  overlaps them. Two cursors gain only with a lean loop. On the old loop they were slower at 12
  threads (9.53 against 9.15 ms for the parse on the test server); with the new loop, the cursors
  together with the cheaper lookup cut the parse by 0.89 to 1.07 ms there. Three cursors were
  slower than two (register spills).
- **Why exact.** Each slice keeps its own cursor, its own records (the second slice's start after
  room for the first's), its own escapes and its own runs. The two parts come out exactly as two
  separate parses would.

### 4. Branch-free AVX2 field ends

- **Bottleneck.** There were 2.2 branch mispredictions per line, from the method (GET 46%, POST
  37%) and from the endpoint length (49% shorter than 32 bytes).
- **Design.** One 64-byte window of space bits starts at byte 31: the method starts at byte 28,
  right after the timestamp's space, and has at least 3 letters. Its first set bit ends the method,
  the next one ends the endpoint (`tzcnt`, then clear the lowest bit). An endpoint that does not
  end inside the window goes to the cold path, which loads the 128 bytes from the endpoint's start
  and finds its space there. Starting the window at byte 31 instead of 28 (step 68) took those
  lines from 7.60% to 6.64% of seed 1's (7.40% to 6.44% on holdout seed 99). Before
  that change, 10.2% of seed 1's lines took the cold path: 7.6% past the window, 2.6% missing their
  home slot (technique 5).
  `vpmovmskb` was inline asm from step 9 on, because LLVM had turned the intrinsic into a vector
  of booleans and, on CPUs without AVX-512 (the box), built the 64-bit mask one bit at a time. In
  the code of step 53 the plain intrinsic compiles to the same hot loop (PERF.md, step 53).
- **Why exact.** The timestamp is fixed at 27 bytes, the fields are separated by one space, and
  every method has 3 to 7 letters (spec). So bytes 28 to 30 are letters, the method's space is at
  byte 31 to 35, and the window's first two spaces are the method's and the endpoint's. The
  endpoint then ends at byte 32 or later, so a line moves the parse on at least 38 bytes
  (`MIN_LINE`, index 26), and the furthest load ends 242 bytes after a line's start, within the
  pad (technique 1). An
  endpoint over 125 bytes fails an `assert!` and stops the program.

### 5. Endpoints found where they lie, in one shared table

- **Bottleneck.** The old parse built a 64-byte key for every line (a masked load and a fold into
  a hash), then made two dependent table loads: about 34 uops a line, the largest cost of the
  parse. On the test server, removing the lookup outright (wrong output) took the 12-thread parse
  end from about 9.2 to 7.65 ms (PERF.md, steps 42-43).
- **Design.**
  - *Home slot.* Two CRC32 steps over the endpoint's length, its first 8 bytes (already in a
    register) and its last 8 bytes pick the slot (`home`).
  - *Compare.* The slot holds the id as a record holds it (`id << 21`), and the id selects the
    stored 128-byte key. Bytes 0..=len of the line are compared with the key by `eq_mask`
    (`vpcmpeqb`, then `vpmovmskb`) and `bzhi`. A match is the id.
  - *Cold path.* Misses and endpoints that end past the window go to the cold path. So do
    endpoints under 8 bytes, almost always: their last 8 bytes reach into the method, so the inline
    probe does not look in their home slot. It builds the key, probes linearly from the home slot, compares whole keys, and inserts a
    new endpoint: it copies the key and the name's bytes, then publishes the id by compare-and-swap.
  - *The cold path's own cost.* A profile of step 67 put it at 9.6 to 11.2% of all cycles, for
    10.2% of the lines: about 120 cycles a call. Its hash now reads the endpoint's first and last
    8 bytes from the line, with `bzhi` keeping 8 × min(len, 7) + 8 bits, instead of reading them
    back from the key it has just stored as vectors, which waited on store forwarding. Its space
    mask is built from two 64-bit halves, since one `u128` of four masks compiled to `vpinsrd`
    inserts. The two measured −0.45% and −0.62% on the replica (PERF.md, step 68).
  - *Size.* There are 8 times as many slots as possible ids (11% load), so almost every lookup is
    one probe.
  - *Memory.* The table is one zeroed static (`TABLE`), so nothing allocates or clears its 352 KB:
    its pages are mapped as the parse first writes them.
- **Why exact.**
  - *Keys.* A stored key is the endpoint, a space, then zeros, and no endpoint byte is a space or
    zero. So a match of bytes 0..=len (the endpoint and its space) means the same bytes and the same
    length, for any length up to 125.
  - *Hash.* The hash only picks where to look. In the cold path, for an endpoint under 7 bytes,
    `bzhi` keeps its bytes and its space, as the key holds them; from 7 bytes up it keeps all 64
    bits, which are then bytes of the endpoint and its space. So the cold path hashes the key's
    bytes at the same offsets, for every length, as it did when it read them from the key.
  - *Table.* All-zero bytes are an empty table: every field is an atomic, a SIMD vector or a null
    slice pointer of length 0.
  - *Races.* If two threads insert the same endpoint at once, the slot's compare-and-swap picks one
    id, and every record gets that id. The losing id is published nowhere: no slot or record holds
    it.
  - *Id space, a bound.* The thread that lost keeps its id for its next insert, which overwrites
    the id's key and name. So at most one id per thread is ever unpublished: at 12 threads the
    spec's 1,842 endpoints need at most 1,854 of the 2,047 ids, and any thread count up to 205
    fits. More ids than that (more endpoints than the spec allows) stops the program.
  - *Names.* The names are copies, so they stay valid after the log's pages are given back.

### 6. Latency digits where they lie; 4-byte records with an escape list

- **Bottleneck.** Memory, and instructions per line. The records are the largest thing the program
  allocates. With 8-byte records alive while the output was written, the box measured 44.66 MB
  (run-73fe01d551bc); 4-byte records, freed before the output, took it to 24.3 MB
  (run-6154ff9e18eb, PERF.md step 8).
- **Design.**
  - *Short latencies.* The latency starts 5 bytes after the endpoint (its space, three status
    digits, a space). One 16-byte load holds it and its LF, and the LF's place in that load's mask
    is the digit count, 1 to 10. Up to 6 digits (98.8% of lines) are converted in place: minus `'0'`, then `maddubs` and
    `madd` with weights for that digit count (`DIGITS`). The weights are 0 past the last digit, so
    nothing is shifted.
  - *Long latencies.* 7 to 10 digits (1.2% of lines) go to `parse_digits`, out of line, which
    converts that many digits one at a time and pushes the escape. Inlined into the loop's cold
    branch it cost 0.29% on the replica (PERF.md, experiments).
  - *Records.* A record is `endpoint id << 21 | 5xx << 20 | latency`. Every latency of 7 or more
    digits (1.2% of lines) is stored as the marker `2^20 − 1`, and `parse_digits` pushes its value
    to the part's escape list. Up to 6 digits are at most 999,999, below the marker, so the marker
    in a record always means "the next escape", with no compare on the value.
  - *Room.* The records array is sized for the worst case, so the loop has no capacity checks;
    only touched pages get memory. Without the 6-digit path the parse ran 3.76% slower on the
    replica (PERF.md, step 49).
- **Why exact.** Latencies are 32-bit (spec), and 10 digits fit the `u64` that `parse_digits` sums them in.
  Escaped values are read back in the order they were written, run by run, from each run's first
  escape index. A status is 5xx exactly when its first digit is `5`.

### 7. Hours as runs, with no calendar

- **Bottleneck.** Parsing a timestamp per line was about 80% of the reference's time. A bound on
  disorder would make the program wrong on a seed that breaks it.
- **Design.** Records carry no hour. Shards write in time order, so the file is a sequence of runs
  of lines from one hour.
  - A slice starts a new run when bytes 5..13 of the timestamp (`MM-DDTHH`, one 8-byte load)
    change.
  - The run's key is the first 13 bytes (`YYYY-MM-DDTHH`) read as a big-endian number.
  - The merge sorts all runs by hour and cuts them into hours. The order of an hour's runs changes
    no output byte: each run reads its own escapes, and a group's values are only counted or
    selected.
- **Why exact.** The timestamp has a fixed width, so byte order is time order, and the 13 bytes
  are the start of the output's hour label. All timestamps lie in one 72-hour span (spec), so lines
  that share `MM-DDTHH` share the year. A line out of order only starts another run: pass 2 gathers
  every run of an hour from every slice, so no bound on disorder is assumed.

### 8. Byte-order endpoint sort on 32-byte keys

- **Bottleneck.** The sort was on the serial path between the passes, which every other thread
  waits for. With 16-byte keys, 600 of the 1,842 endpoints tied and needed a name compare; the
  sort was 0.20 ms of a 0.27 ms merge on the test server. With 32-byte keys it still took 0.14 ms
  of the replica's 0.275 ms merge (phase marks of step 67's code).
- **Design.** Endpoints are sorted by the first 32 bytes of their keys, as two big-endian `u128`
  numbers, then by the full name on ties. Since step 69 the merging thread sorts them after the
  hand-off (`names`), while the other threads start pass 2, and publishes the order in a second
  `OnceLock` (`order`). An hour needs the order only when it builds its groups, after its count
  and scatter; its write reads it too, set by then because the hour's groups used it. Replica
  −0.92% and −0.86% against step 68 (PERF.md, step 69).
- **Why exact.** Big-endian numbers compare like their bytes. In a key the endpoint is followed by
  a space and zeros, both below every endpoint byte, so a proper prefix sorts first: byte order.
  Ties fall back to the full bytes. The order is set once, and every hour waits for it
  (`order.wait()`) before it reads it; the parse is over before the sort starts, and the keys are
  released only after it.

### 9. Hour-local counting sort, rank counting and selection

- **Bottleneck.** Global histograms and a scatter over a 12 MB array did not scale with threads
  (1 thread 195 ms, 12 threads 60 ms on the test server). A full sort per group is more work than
  three ranks need, and 95% of the groups have 64 values or fewer.
- **Design.**
  - *Grouping.* A thread takes one hour and counts per endpoint id, with the 5xx count in the high
    half of the same `u64` (one add per record). It turns the counts into starts and places each
    latency. These arrays fit in L1/L2. The scratch array is sized once, for the largest hour.
  - *Up to 64 values* (`small_ranks`). The values are loaded into 1, 4 or 8 vectors, zeros past the
    end. For every value, one compare per vector adds 1 to each lane holding a larger value: after
    the loop, each lane knows how many values lie below it. The value at rank r is the largest lane
    with at most r values below it. There is no branch on the values. Sorting groups of up to 32
    values took twice as long in a microbenchmark on the groups' real sizes.
  - *Larger groups.* p50 comes from `select_nth_unstable`, p95 from a selection within the part
    above p50, and p99 within the part above p95.
- **Why exact.**
  - *Ranks.* They are `ceil(p * n / 100)`, 1-indexed.
  - *Rank counting.* The r + 1 smallest values are not above the value at rank r, so any larger
    value has at least r + 1 values below it, and the rule picks exactly the value at rank r, ties
    included. Lanes past the end hold 0 and are never compared against, so they never change a
    count; 0 is also never larger than a real value.
  - *Selections.* Each value is read right after its own selection, before the next one reorders
    the slice (reading all three at the end gave wrong output once, PERF.md). The selections are
    exact; only the order of the work changes.

### 10. Out-of-order compute, in-order placement, lines written in place

- **Bottleneck.** Writing hours in order made a thread that had finished a small hour wait for a
  large one. A write phase after a barrier was 2 ms slower: twelve threads faulting in the shared
  output at once. And building each line in a stack buffer, then copying it out, should stall once
  per line: the copy's wide loads read bytes that had just been stored one or two at a time. (The
  replica measured the same speed both ways; the in-place writer was kept for its 11 fewer lines,
  PERF.md step 50.)
- **Design.**
  - *Lengths first.* Each hour's exact byte length is computed from its statistics and published.
    The hour stays pending with its statistics. Its groups are built in name order, so the hour
    waits there for the order, after its count and scatter (technique 8).
  - *Placement.* Once every earlier hour's length is known, the hour's start is their sum, and its
    thread writes it into the shared mapping while the other threads keep computing.
  - *Largest hour first in each block.* In each block of one hour per thread, the largest hour goes
    first, so it ends with its block instead of holding up the small hours after it (up to 30 hours
    were pending at once).
  - *Lines backwards.* Each line is written from its end, straight into place: the LF, then each
    number in digit pairs from the `PAIRS` table while it is 100 or more, then its last one or two
    digits as one pair (a lone digit's leading 0 is overwritten by the space before the number),
    then the endpoint and the hour label. There is no buffer and no copy.
- **Why exact.** The length comes from the same numbers the writer prints, and the writer asserts
  that an hour's lines end exactly at the hour's start. The rate is `errors * 1000000 / count` in
  integers. Every hour's bytes are its own range of the file, and the file is cut to the sum of
  the lengths.

### 11. Memory follows the work

- **Bottleneck.** The peak is in pass 2: records, the growing output, the scratch arrays and the
  pending statistics, all at once.
- **Design.**
  - *Input pages.* Each parse thread drops each pair's pages in one call (`madvise(MADV_DONTNEED)`,
    as the pair's slices are adjacent), in parallel, instead of leaving 58,000 pages to one unmap on
    one core (the exit's, or now the merge's `munmap`). One call per pair instead of one per slice
    measured about 0.3% faster on the replica: each call is a round of TLB shootdowns (PERF.md,
    step 72).
  - *Page tables.* The merging thread unmaps the log right after the hand-off to free its page
    tables (about 0.5 MB), before it sorts the names: freed 0.16 ms later, after the sort, they
    landed in the start of pass 2, when the values buffers fault in, and the expected highest of
    five peaks was 0.08 and 0.17 MB above step 68's (two builds), against +0.03 and −0.01 MB with
    the unmap first (PERF.md, step 69).
  - *Records.* A slice's records go when the last hour that reads them has placed them.
  - *Statistics.* Pending statistics are 24 bytes per endpoint.
  - *THP off*, so that one touched byte cannot fault in 2 MB.
- **Why exact.** Nothing released is read again.
  - The log's bytes are only read during the parse, and every endpoint name was copied.
  - A slice's records go when the count of its runs still to be placed reaches zero, after the
    last of them has been placed.
  - Released input pages would fault back in from the page cache if they were read; anonymous
    pages would read as zeros, but nothing reads them.

### 12. No teardown

- **Bottleneck.** Dropping the tables one free at a time took 4 to 5 ms on the box. And the exit
  tears down the output mapping's 2,500 page-table entries on one core (about 0.6 ms on the test
  server, PERF.md step 40).
- **Design.** After its last write, each thread waits at a barrier, then drops the page-table
  entries of a page-aligned share of the output mapping (`MADV_DONTNEED`). Dropped while other
  threads still wrote, they slowed those writes by as much (PERF.md, experiments). Once the output
  is cut to its length, the program calls `exit(0)`.
- **Why exact.** The output is complete in the page cache before the exit; the kernel frees the
  rest. `MADV_DONTNEED` on a shared file mapping drops only the page-table entries: the written
  pages stay in the file, and nothing touches the mapping afterwards.

### 13. Build

- **Bottleneck.** Branch weights and code layout: without a profile the compiler guesses which side
  of each branch is hot, and two builds of one source should give the board close to the same
  binary. The
  first profile-guided build took 0.0510 → 0.0498 s on the board (PERF.md, step 5; step 4 has no
  run in our log).
- **Design.**
  - *PGO.* The Dockerfile generates training data with a byte-identical copy of the organisers'
    generator (seed 7, sample scale), runs an instrumented build on it, and builds the final binary
    with the profile.
  - *Atomic counters.* The training run uses `-instrprof-atomic-counter-update-all`. With plain
    counters its threads lost updates, so every build got a different profile and a different
    binary. Atomic counters stop that, but counts that depend on timing (the endpoint table's
    insert race, the endpoint sort, the flush) still vary from run to run, so two builds of one
    source can lay out blocks differently (17 to 120 worker instructions in step 65's builds). Every
    replica A/B therefore uses two builds a side.
  - *Loop alignment* (`-align-loops=64`) is on for A. The one board build without it (PERF.md, step
    36) ran 0.027124 s, in A's slow mode, so it proves nothing either way; the flag stays because A
    is not slower with it and C's best run uses it.
  - *The rest.* Fat LTO, one codegen unit, `panic = "abort"`, a static binary and
    `target-cpu=native` (the board builds this track on the box). A `compile_error!` stops a build
    without AVX2.
- **Why exact.** The build changes speed only. Each correctness check runs the binary built by
  this directory's Dockerfile.

## Why the output is exact

No step approximates: each one keeps every byte of the reference output. The arguments, gathered
from the techniques above:

- **Every line once, whole.** Slices are cut at line ends and never overlap (technique 2). The 256
  zero bytes after the log keep every fixed-size load inside the mapping, and a malformed line
  stops the program at a tripwire instead of reading past them (index 16).
- **Endpoints.** A lookup compares the stored key over the endpoint's bytes and the space after it,
  so two endpoints are never taken for one, at any length up to 125 bytes (technique 5).
- **The id bound.** A thread keeps an id that lost its slot for its next insert, so at most one id
  per thread is unpublished: the spec's 1,842 endpoints need at most 1,854 of the 2,047 ids at 12
  threads, and any thread count up to 205 fits. More endpoints than that stop the program
  (technique 5, index 17). The name order lists every id handed out, an unpublished one too, under a
  copy of a name that a published id holds; no record holds it, so its group is empty in every
  hour and prints no line.
- **Latencies.** One of up to 6 digits is converted by exact place values, a longer one digit by
  digit in a `u64`. Every value of 7 or more digits goes to the escape list (a record's 20 bits
  hold every value of up to 6), read back in order, so every `u32` latency is kept (technique 6).
- **Hours.** An hour is the first 13 bytes of the fixed-width timestamp, so byte order is time
  order, and every run of an hour is gathered from every slice: no bound on disorder is assumed
  (technique 7).
- **Order of the endpoints.** They are sorted by their first 32 bytes as big-endian numbers, then by
  the full name; the space after an endpoint is below every endpoint byte, so a proper prefix sorts
  first. The order is set once, and every hour waits for it before it builds its groups
  (technique 8).
- **Percentiles.** The ranks are the spec's nearest ranks, `ceil(p * n / 100)`. Groups of up to 64
  values count ranks exactly, ties included; larger groups take three nested
  `select_nth_unstable` calls, each rank read right after its own selection (technique 9).
- **Counts.** Each hour's per-endpoint counts start at zero: the array starts zeroed, and each hour
  zeroes every count as it takes the group (`std::mem::take`); every id a record can hold is in the
  endpoint list, so no count carries over to the next hour (technique 9).
- **The output.** Each hour's byte length comes from the same numbers the writer prints, and the
  writer asserts that the hour's lines end exactly at its start. The rate is the integer
  `errors * 1000000 / count`, and the file is cut to the sum of the lengths (technique 10).
- **Evidence.** The seed-1 hash at 1, 3, 7 and 12 CPUs, sample seeds 2-4 and holdout seed 99 at
  every gate, and the robustness runs on 30 fresh full-scale seeds and in 1,485 more runs ("Robust
  on unseen seeds").

## Spec edge cases

The spec injects fifteen edge cases into every seed. Each one, and the code that handles it:

| Case | Handled by |
|---|---|
| E1: a group of one request | Rank counting with one vector: every rank is `(p * 1).div_ceil(100) - 1 = 0`, and the one value has 0 values below it |
| E2, E7: latency 4294967295 | 10 digits go to `parse_digits`, digit by digit into a `u64`; every latency of 7 or more digits goes to the escape list as a full `u32` |
| E2, E6: latency 0 | A value like any other: the escape marker is `2^20 - 1`, not 0, and rank counting never compares against the zero padding |
| E3, E8: statuses 499, 500, 599 | 5xx is "first status digit is `5`", so 499 is not an error and 500 and 599 are |
| E4, E12: an hour boundary, an earlier hour written later | Each line's own `MM-DDTHH` starts a run when it changes; runs are merged by their hour key, with no bound on disorder |
| E5: the first and last instants of the span | The hour key is the timestamp's first 13 bytes; `MM-DDTHH` is enough to see a change because the span is 72 hours |
| E9, E10, E11: equal timestamps, duplicate lines | Nothing is keyed by line or by time: every line is one record |
| E13: all seven methods | Skipped by the first space bit of the mask: each has 3 to 7 letters, so the window from byte 31 starts at or before its space (technique 4) |
| E14: `/` and the 125-byte template | `/` is under 8 bytes, so it almost always takes the cold path (technique 5). An endpoint that ends past byte 94 does too; it loads 128 bytes from the endpoint's start, so a 125-byte endpoint and its space fit. Keys are 128 bytes, so every endpoint is compared in full |
| E15: 1,842 distinct endpoints | Ids 1 to 2,047 (11 bits): at most 1,842 + 12 used at 12 threads, since each thread keeps an id that lost its slot (technique 5) |
| Empty `requests.log` | `len @ 1..` fails, and the summary created at the start stays empty |

Trusted from the spec and not checked, since a check would be a branch per line: the 27-byte
timestamp, the 72-hour span, one space between fields, a method of 3 to 7 letters, the 3-digit
status, and 1 to 10 latency digits then LF. A line that breaks them gives wrong output or stops at an assert, never a read
outside the mapping (SAFETY in `parse`). These are checked by an `assert!`, which stops the
program:
- the endpoint length;
- the 2,047 ids;
- the room for the records;
- each hour's written length.

## Robust on unseen seeds

The board scores seed 1; nothing in the program is fitted to seed 1's data.

| What could change with the seed | What the program does |
|---|---|
| Disorder between the eight shards | No bound is assumed: a line out of order starts another run, and pass 2 gathers every run of an hour from every slice (technique 7) |
| Number and size of the hours | Hours come from the data; the scratch array is sized from the largest hour |
| Latencies up to 4294967295 | The escape list keeps any `u32` (technique 6) |
| Distinct endpoints | 2,047 ids for the spec's 1,842 (E15); at most one id per thread is unpublished, whatever the order of the lines (technique 5); more endpoints than the spec allows stop the program |
| Endpoint lengths | Any length up to 125 bytes is looked up exactly (keys of 128 bytes); short ones only take the cold path more often |
| The build | PGO is trained on generator seed 7 at sample scale, not on the scored seed |

Evidence, all on the binary built by this directory's Dockerfile:

- **Holdout seed 99**, full scale, byte-equal to the reference output in the gate since step 13
  (the hidden-seed check in our run log), including every gate of the new parse (PERF.md, "How the
  final number was verified").
- **Sample seeds 2-4**, byte-equal to the reference at every gate.
- **Fresh seeds** the earlier gates had not seen:
  - tiny and sample seeds 21-24 (step 30) and 21-23 (step 31);
  - 41-43 (steps 38-40, and again for the new parse, step 43), each at 1 and 8 CPUs, against
    `reference/a_ingest.py`.
- **Malformed input**: 300 mutated logs abort with a message or exit 0, never a segfault (step
  29, and again for the new parse: 111 exit 0, 189 stop at a tripwire).
- **Robustness runs on the replica** (Sun 1 Nov, about 17:00 UTC), on the exact Dockerfile build
  of step 59 (step 60 changes names and formatting only), with data generated on the replica by the organisers' generator (Python 3.11, which
  reproduces the official sample seed 1 and its reference output byte for byte). Every run was
  byte-exact, with no crash, no nonzero exit and no output on stderr:
  - 30 fresh full-scale seeds (101-130) at 12 threads, seeds 101-102 also at 1 and 3 CPUs and
    seeds 103-104 at 2, 7 and 11: 40 of 40 runs;
  - 20 sha256-checked repeats per seed at 12 threads, and runs with a busy loop on 1, 2 or 4 of
    the twelve CPUs (seeds 109, 110 and 113): 640 of 640;
  - tiny and sample seeds 201-210 at 1, 2, 3, 7, 12 and 20 CPUs, each run as root and as uid
    65534 (the board's privilege drop) with read-only data, 3 repeats, and full scale at 12 and
    20 CPUs for both users: 840 of 840;
  - copies of seed 101 whose every file is an exact multiple of 4096 bytes, so that a read past
    the end of a file would fault, at 1, 3, 7, 12 and 20 CPUs: 5 of 5.
- **Step 63**, the exact Dockerfile build: fresh full seeds 101-110 at 12 threads and 101-102 at
  1 and 3 CPUs, 14 of 14; 20 repeats of each of 10 seeds, 200 of 200; tiny and sample seeds
  201-205 at 1, 3, 7, 12 and 20 CPUs, as root and as uid 65534, plus full seeds 111-112, 208 of
  208; the 13 in-spec edge inputs at 1 and 12 CPUs as both users, 52 of 52.
- **Steps 64 and 65**, the exact Dockerfile builds of their code (step 65 adds only comments to the
  code that was gated): the same runs as step 63 (14 of 14, 200 of 200, 208 of 208), the 13 in-spec
  edge inputs (52 of 52) and 250 malformed logs, none of which crashes. Step 66 passed the gate
  script on its exact Dockerfile build, and its machine code differs from step 65's only outside
  the hot loops (PERF.md, step 66).
- **Steps 68 and 69**, each on two exact Dockerfile builds and on a
  build of the file as committed (comments only after the gate): the gate (the seed-1 hash at 1,
  3, 7 and 12 CPUs, sample seeds 1-4, holdout seed 99, the empty input, and 250 malformed logs,
  none of which crashes); the 13 in-spec edge inputs at 1 and 12 CPUs as both users, 52 of 52;
  fresh full seeds 101-130 at 12 threads and 101-102 also at 1 and 3 CPUs, 34 of 34, with 20
  repeats of each of the 30 seeds, 600 of 600; and tiny and sample seeds 201-210 at 1, 3, 7, 12
  and 20 CPUs, as root and as uid 65534, twice each, 400 of 400.
- **Steps 70 and 71**, each on two exact Dockerfile builds: the same gate (250 malformed logs: 9
  stop at a tripwire, none crashes), and on the laptop's build the seed-1 hash, sample seeds 1-4,
  holdout seed 99 and the empty input. Step 71's code also ran on 136 seeds never used before
  (tiny 301-400, sample 401-430, full 501-506), each against the organisers' reference at 1 to 8
  CPUs, with repeats and a harness-like container run: 2,224 runs, all byte-identical, no stderr.
- **Steps 72 to 74** (step 74 is the submitted code): the same gate on every kept build (250
  malformed logs: 9 stop at a tripwire, none crashes).
- **The id bound, measured.** Every seed has 1,842 endpoints, against 2,047 ids. With step 54's
  code up to 13 ids were lost in 20 runs of a seed; with steps 57 to 59, at most 1 id in 150 runs,
  and the bound is one per thread: at most 1,854 ids at 12 threads.
- **Time and memory on other seeds.** On the 30 fresh seeds step 59 ran 16.71 to 18.22 ms (medians
  of 7 runs; seed 1: 17.36 ms), with a memory peak of 18.97 to 20.19 MB on seeds 101-110 (max of 5
  runs; seed 1: 19.44 MB).
- **Not covered:** more than 20 threads (the replica has 20 CPUs).

## Error handling

Errors are panics with a message, and the build aborts on panic, so the exit status is nonzero:
- a missing log, a summary that cannot be created or sized, or a failed `mmap`;
- an endpoint over 125 bytes, or more than 2,047 endpoint ids;
- records past their room;
- an hour whose written length differs from its computed one.

Ignored on purpose, as each only costs memory or time and never changes a byte: a failed `prctl`
(THP stays on), `madvise` (the pages are kept) or `munmap` (the log stays mapped). The output is
written through a shared mapping, so a full `/out` would show as a SIGBUS on a store, not as an
error message; the output is 10 MB.

The spec's empty input gives an empty summary.

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise`, `munmap` and `prctl` declared by
hand. All the code is one file, `src/main.rs`.

## With another 24 hours

- **The selections of the large groups.** The 5.9K groups of more than 64 values hold 63% of the
  values, and their `select_nth_unstable` calls cost about 1.15 ms on the replica (a build that
  skips them, wrong output, ran 6.9% faster). Counting ranks up to 96 or 128 values did not help,
  nor did sorting groups of up to 128 or 256 values, a bracketed sample selection, an AVX2
  quickselect or a radix select (PERF.md, experiments). A cheaper exact selection for groups of 65 to a few thousand
  values is still the next thing to try.
- **The memory peak.** The board read 18.79 to 19.8 MB in six runs of step 56's engine and 18.15
  to 19.18 MB in the sixteen runs of steps 61 to 66 that report it, and the final scoring takes the largest of five
  peaks. Step 61's gate moved the output off the peak (about −1 MB). Deferring every write, alone
  or with long runs given back right after their scatter, took 0.2 to 0.5 MB more off but cost
  1.9 to 2.8% (PERF.md, experiments). Up to 3 MB of each peak is the kernel's per-CPU charge
  batching, 2.3 to 3.1 MB at the peak in 10 replica runs. Only a program that made every charge on
  one CPU could avoid it: about 6,200 page faults, 3 to 5 ms of single-CPU work, which costs more
  T than the memory is worth. Removing the burst at the start of pass 2, the peak in 7 of those 10
  runs, would lower the mean peak by only 0.15 MB (PERF.md, "What we learned about the box").
- **Pass 2 during the parse** is ruled out in spec: the disorder is unbounded, so a part not yet
  parsed may hold lines of any hour, and no hour is complete before the last part is in.
