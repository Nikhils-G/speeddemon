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

The reference runs 32.05 s at 228 MB and S 56. The submitted code (PERF.md step 60, 3aa95c6, S
449) runs 0.0163 to 0.0172 s on the board in its normal runs (run-252c6a84434b, 0.016292 s, 19.61
MB, 86.22 points, the fastest Track A time; run-4f8350fdbe1a, 0.016341 s, 86.32 points) (details:
PERF.md, steps 51-60). On the replica of the scoring box (the same CPU, timed by the board's own timer;
PERF.md, "How we measured"), step 53 ran at a median of 16.856 ms over 201 pairs, and steps 54 to
56, 59 and 60 within 0.3% of it.

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
the merge, pass 2, the write and the exit. The probes measured step 43's parse; steps 44 to 59
changed pass 2, the writer and cold code, not the parse loop.

Pass 2's parts, on the replica (builds that skip one part, wrong output, timing only; 201 pairs
each, PERF.md):
- the selections of the groups of more than 64 values: 1.15 ms, of which p50's 0.70;
- writing the lines: 1.10 ms, of which output page faults and stores 0.76 and formatting 0.35;
- rank counting for the groups of up to 64 values: 0.75 ms;
- the scatter of each hour's values: 0.47 ms.

Memory is the other axis. About 4.7 MB is the container's floor (an empty input). The output
counts, because `/out` is a tmpfs. The mapped input does not: the harness has it in the page cache
already, charged to the host. The peak (18.2 to 19.3 MB on the replica) comes 2 to 3 ms into pass
2, while records of 32 of the 48 parts are still alive and the largest hours are written. Each
run's peak also includes 0 to 3 MB of the kernel's per-CPU charge batches (PERF.md, "What we
learned about the box").

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
  fewer of them. Each thread finds the cuts of the slices it takes.
- *Two slices side by side.* A thread parses its two slices one line of each in turn. A line's end,
  and so the next line's start, waits on that line's loads; the other slice's line fills the wait.
- *Field ends.* The timestamp is a fixed 27 bytes, so one 64-byte AVX2 window right after it holds
  the spaces that end the method and the endpoint. Their positions come from `tzcnt`, with no
  branch on the method or on the endpoint's length.
- *Endpoint lookup.* The endpoint is looked up where it lies. A CRC of its length and its first and
  last 8 bytes picks its slot in one table that all threads share. One vector compare against the
  stored key (the endpoint, a space, then zeros to 128 bytes) confirms it. Endpoints that end past
  the window, misses and new endpoints go to a cold path that builds the key.
- *Inserts.* Lookups are plain loads; the rare insert claims its slot by compare-and-swap, so no
  tables are merged after the parse.
- *Latency.* A latency of up to 6 digits (98.8% of lines) is converted where it lies, by two
  multiply-adds whose weights depend on its length. Longer ones take a cold, out-of-line parser.
- *Records.* A line becomes one `u32`, written into one records array per thread. The array is
  allocated zeroed for the worst case, so only written pages get memory.
- *Release.* Each thread gives back the pages of every pair of slices it has parsed.

**Merge.** One set of threads runs both passes. A worker publishes its parse and waits, and the
main thread does the short serial merge (`merge`):
- it sorts the endpoints;
- it sorts all runs by (hour, slice);
- it counts how many hours read each slice's records;
- it maps the output, then publishes all of this as one hand-off.

While pass 2 runs, the otherwise idle main thread unmaps the log (which frees its page tables too)
and gives back the endpoint keys.

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
4. Line-aligned slices, four per thread, taken two at a time from a shared counter (`cut`, `claim`): no serial cut pass, and a slowed thread parses fewer slices. Exact: cuts follow an LF and never decrease; the slice count is even, so a pair's second slice always exists.
5. One thread pool for both passes, handed over through `OnceLock` (`parts`, `merged`): no join or spawn between the passes. Exact: `OnceLock::wait` orders every parse before the merge.
6. Two slices parsed side by side, one line of each in turn (`parse`, `step`): two independent line-to-line dependency chains. Exact: each slice keeps its own cursor, records, escapes and runs.
7. Hour change from one 8-byte compare of `MM-DDTHH` (`step`, `last`). Exact: all timestamps lie in one 72-hour span (spec).
8. Hour key = the timestamp's first 13 bytes as a big-endian number. Exact: fixed width, so byte order is time order, and the bytes are the output label.
9. Runs of lines instead of an hour per record (`Part`, `Run`). Exact: every run of an hour is gathered from every slice, so no disorder bound is assumed.
10. One 64-byte AVX2 space mask after the fixed 27-byte timestamp (`spaces`). Exact: fields are separated by one space.
11. Field ends by `tzcnt` and clear-lowest-bit, no branch on the method or the length. Exact: the first two set bits after byte 27 are the method's and the endpoint's spaces.
12. `vpmovmskb` by the plain intrinsic (`movemask`). At step 9 LLVM rebuilt the 64-bit mask bit by bit from the intrinsic, without AVX-512, and inline asm avoided it; in the code of step 53 the intrinsic compiles to the same hot loop as the asm, and the Dockerfile's build has no `vpinsrb` (PERF.md, step 53). Tracks B and C keep the inline asm. Exact: the same instruction.
13. Endpoint lookup where it lies (`Interner::find`): the home slot from two CRC steps over the length and the first and last 8 bytes (`home`), then the stored key compared over bytes 0..=len (`vpcmpeqb`, `movemask`, `bzhi`). Exact: the stored key is the endpoint, a space, then zeros, so a match of bytes 0..=len means the same bytes and the same length; the hash only picks the slot.
14. Cold path for endpoints past the window, misses and inserts (`Interner::id`, `#[cold]`, `#[inline(never)]`): 128 bytes loaded, the key built with a sliding `KEEP` mask, linear probing from the home slot, four vector XORs and one `vptest` per slot. Exact: the whole 128-byte key is compared.
15. 128-byte keys: an endpoint of up to 125 bytes, its space and zeros (`Key`). Exact: no endpoint byte is a space or zero, so equal keys mean equal endpoints, at any length.
16. Tripwire `assert!` on the endpoint length (`MAX_NAME`, 125): malformed input stops instead of reading past the pad. Exact: it stops; it never guesses.
17. One lock-free open-addressing table shared by all threads, ids published by compare-and-swap (`Interner::add`); a thread keeps an id that lost its slot for its next insert (`SPARE`). Exact: a losing id is never in a record, and at most one id per thread is unpublished, so the 2,047 ids hold the spec's 1,842 endpoints for up to 205 threads.
18. 8x slots per possible id (11% load): almost every lookup is one probe. Exact: the load changes the probe count, not the answer.
19. A slot holds the id as a record holds it (`id << 21`), so a lookup needs no shift. Exact: the same bits.
20. Endpoint bytes copied at insert: the input's pages can be released early. Exact: names are compared and printed from the copies.
21. Table allocated zeroed (`Box::new_zeroed`): its pages fault in only as touched. Exact: a zero slot means empty; ids start at 1, and id 0's key stays zero, which no endpoint matches.
22. Latencies of up to 6 digits converted where they lie by `maddubs` and `madd` with per-length weights (`DIGITS`, a literal table): no shuffle, no multiply chain. Exact: weights are 0 past the digits, and the table holds exact place values.
23. Longer latencies (7 to 10 digits, 1.2% of lines) by `parse_digits`, digit by digit up to the LF, `#[cold]` and out of line, which also pushes the escape. Exact: at most 16 bytes are read, and the value is accumulated in a `u64`, so 10 digits cannot overflow. The SIMD version it replaced was no faster on the replica (PERF.md, step 53).
24. 4-byte records `id << 21 | 5xx << 20 | latency`. Exact: 11 bits hold the ids; a latency that does not fit 20 bits escapes (25).
25. Escape list for latencies of 2^20 − 1 or more, pushed by `parse_digits`, the only path that can see one: 2^20 − 1 has 7 digits. Exact: read back in order, run by run, from each run's first escape index.
26. Worst-case pre-sized records, one array per thread for all its slices (`free`), written through raw pointers: no capacity checks, and untouched pages cost nothing. Exact: a line moves the parse on at least 35 bytes (`MIN_LINE`), so a slice of n bytes holds at most n / 35 + 1 records, and an assert checks the room.
27. 5xx from the status's first digit. Exact: a status is 5xx exactly when it starts with 5.
28. Input pages dropped by each parse thread after each pair (`release`, `MADV_DONTNEED`): the exit does not unmap 58,000 pages on one core.
29. Log unmapped by the idle main thread during pass 2: its page tables are freed off the serial path. Exact: nothing reads the log after the parse; the names were copied (20).
30. Endpoint keys released after the sort. Exact: no key is read after it.
31. Endpoint sort by the first 32 key bytes as two big-endian `u128`, then the name. Exact: byte order; the space after an endpoint is below every endpoint byte, so a proper prefix sorts first.
32. All runs sorted by (hour, slice) and cut into hours with `chunk_by` (`merge`, `same_hour`). Exact: every run of an hour, from any slice, lands in that hour, whatever the disorder.
33. Hours queued in blocks of one per thread, largest first: small hours do not pile up behind a big one. Exact: it orders the work; each hour's place in the file comes from the published lengths (42).
34. Per-hour counting sort with count and 5xx count packed in one `u64`: one add per record.
35. Scatter without bounds checks (SAFETY: the counts are this hour's).
36. Record pages released per slice once no hour reads them (`readers`): one `madvise` per slice, because each call is a round of TLB shootdowns. Exact: released only after the last reader has placed its values.
37. Percentiles of groups of up to 64 values by counting ranks (`small_ranks`): masked loads into 1, 4 or 8 vectors, one compare per value and vector, and the value at rank r taken as the largest value with at most r values below it. Exact: that is the definition of the value at rank r; the zeros past the end are never counted.
38. Larger groups: p50, p95 and p99 by nested `select_nth_unstable`, each read right after its own selection (`stats`). Exact: ranks `ceil(p * n / 100)`; the next selection reorders the slice.
39. Scratch sized once for the largest hour and allocated zeroed. Exact: every hour fits in it.
40. Pending statistics as six `u32` per endpoint (24 bytes), the rate computed once in pass 2 (`stats`). Exact: the rate is the integer `errors * 1000000 / count`.
41. Each hour's exact output length computed first (`line_len`: the hour label and its space, 21 bytes, `LABEL`; the name; each number after a space; LF), and the writer asserts it.
42. Out-of-order compute, in-order placement: an hour's start is the sum of the published lengths before it.
43. Output mapped shared at a bound, truncated to the total at the end: no write calls on the tmpfs inode lock.
44. Lines written backwards, straight into the mapping (`write`, `PAIRS`): digit pairs while the value is 100 or more, then the last one or two digits as one pair, with a lone digit's leading 0 overwritten by the space before the number. No stack buffer and no copy, so no store-forwarding stall. Exact: the writer asserts that each hour's lines end exactly at the hour's start.
45. Exit without teardown (`process::exit`). Before it, the threads drop the output mapping's page-table entries in parallel after a barrier (`written`, `share`); the pages stay in the file.
46. Profile-guided build trained on generator seed 7, not the scored seed (Dockerfile). Items 46 to 51 change speed only: every correctness check runs the binary this Dockerfile builds.
47. Atomic profile counters: every build gets the same profile.
48. Hot loops aligned to 64 bytes (`-align-loops=64`). Kept because A is not slower with it and C's best run uses it (PERF.md, steps 34 and 36).
49. Fat LTO, one codegen unit, `panic = "abort"` (Cargo.toml).
50. Static binary (`crt-static`).
51. `target-cpu=native`, with a `compile_error!` without AVX2.
52. Exit-code timing probes on the box (PERF.md, "Box probes"). Measurement only, never a scored version.
53. No dependencies: libc calls declared by hand.

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
  up the offsets: the furthest load ends 239 bytes after a line's start, within the 256-byte pad.
  The dataset is mounted read-only, so the bytes cannot change under the mapping.

### 2. Line-aligned slices, taken in pairs from a shared counter

- **Bottleneck.** A serial scan for line ends would be a pass over 239 MB on one core. And with
  one fixed slice per thread, the scheduler sometimes put two of the 12 threads on one CPU for the
  whole parse, so the merge waited for that pair. On the test server (dev build), 4 of 30 runs
  finished the parse 4.9 to 8.0 ms apart and took 18.1-20.8 ms, against 14.6-15.6 ms for the rest
  (PERF.md, step 38).
- **Design.** There are `slices = 4 * threads` slices, taken in file order, two at a time, from a
  shared counter.
  - Cut `p` is just after the last LF before byte `len * p / slices`. A thread computes the cuts of
    the slices it takes, so a slowed thread just parses fewer of them.
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
- **Design.** `parse` takes two slices and runs one line of each in turn (`step(0) & step(1)`),
  then finishes whichever is longer. The two chains are independent, so the out-of-order core
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
- **Design.** One 64-byte window of space bits starts at byte 28, right after the timestamp's
  space. Its first set bit ends the method, the next one ends the endpoint (`tzcnt`, then clear
  the lowest bit). An endpoint that does not end inside the window (7.6% of lines) goes to the
  cold path, which loads the 128 bytes from the endpoint's start and finds its space there. With
  misses and short endpoints, 10.6% of lines take the cold path (test server count).
  `vpmovmskb` was inline asm from step 9 on, because LLVM had turned the intrinsic into a vector
  of booleans and, on CPUs without AVX-512 (the box), built the 64-bit mask one bit at a time. In
  the code of step 53 the plain intrinsic compiles to the same hot loop (PERF.md, step 53).
- **Why exact.** The timestamp is fixed at 27 bytes and the fields are separated by one space
  (spec), so the first two spaces after byte 27 are the method's and the endpoint's. An endpoint
  over 125 bytes fails an `assert!` and stops the program.

### 5. Endpoints found where they lie, in one shared table

- **Bottleneck.** The old parse built a 64-byte key for every line (a masked load and a fold into
  a hash), then made two dependent table loads: about 34 uops a line, the largest cost of the
  parse. On the test server, removing the lookup outright (wrong output) took the 12-thread parse
  end from about 9.2 to 7.65 ms (PERF.md, steps 42-43).
- **Design.**
  - *Home slot.* Two CRC32 steps over the endpoint's length, its first 8 bytes (already in a
    register) and its last 8 bytes pick the slot (`home`).
  - *Compare.* The slot holds the id as a record holds it (`id << 21`), and the id selects the
    stored 128-byte key. Bytes 0..=len of the line are compared with the key by `vpcmpeqb`,
    `movemask` and `bzhi`. A match is the id.
  - *Cold path.* Misses and endpoints that end past the window go to the cold path. So do
    endpoints under 8 bytes, almost always: their last 8 bytes reach into the method, so the inline
    probe does not look in their home slot. It builds the key, probes linearly from the home slot, compares whole keys, and inserts a
    new endpoint: it copies the key and the name's bytes, then publishes the id by compare-and-swap.
  - *Size.* There are 8 times as many slots as possible ids (11% load), so almost every lookup is
    one probe.
- **Why exact.**
  - *Keys.* A stored key is the endpoint, a space, then zeros, and no endpoint byte is a space or
    zero. So a match of bytes 0..=len (the endpoint and its space) means the same bytes and the same
    length, for any length up to 125.
  - *Hash.* The hash only picks where to look.
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
    digits, a space). An 8-byte load holds it. If the LF is within the first 7 bytes (up to 6
    digits, 98.8% of lines), the digits are converted in place: minus `'0'`, then `maddubs` and
    `madd` with weights for that digit count (`DIGITS`). The weights are 0 past the last digit, so
    nothing is shifted.
  - *Long latencies.* 7 to 10 digits (1.2% of lines) go to `parse_digits`, out of line, which
    reads them one at a time up to the LF and pushes the escape. Inlined into the loop's cold
    branch it cost 0.29% on the replica (PERF.md, experiments).
  - *Records.* A record is `endpoint id << 21 | 5xx << 20 | latency`. A latency of `2^20 − 1` or
    more is stored as that marker, and its value is pushed to the part's escape list, on the long
    path only (1.2% of lines).
  - *Room.* The records array is sized for the worst case, so the loop has no capacity checks;
    only touched pages get memory. Without the 6-digit path the parse ran 3.76% slower on the
    replica (PERF.md, step 49).
- **Why exact.** Latencies are 32-bit (spec), and 10 digits fit a `u64` before the escape test.
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
  - The merge sorts all runs by (hour, slice) and cuts them into hours.
- **Why exact.** The timestamp has a fixed width, so byte order is time order, and the 13 bytes
  are the start of the output's hour label. All timestamps lie in one 72-hour span (spec), so lines
  that share `MM-DDTHH` share the year. A line out of order only starts another run: pass 2 gathers
  every run of an hour from every slice, so no bound on disorder is assumed.

### 8. Byte-order endpoint sort on 32-byte keys

- **Bottleneck.** The sort is on the serial path between the passes. With 16-byte keys, 600 of
  the 1,842 endpoints tied and needed a name compare; the sort was 0.20 ms of a 0.27 ms merge on
  the test server.
- **Design.** Endpoints are sorted by the first 32 bytes of their keys, as two big-endian `u128`
  numbers, then by the full name on ties.
- **Why exact.** Big-endian numbers compare like their bytes. In a key the endpoint is followed by
  a space and zeros, both below every endpoint byte, so a proper prefix sorts first: byte order.
  Ties fall back to the full bytes.

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
  output at once. And building each line in a stack buffer, then copying it out, stalled once per
  line: the copy's wide loads read bytes that had just been stored one or two at a time.
- **Design.**
  - *Lengths first.* Each hour's exact byte length is computed from its statistics and published.
    The hour stays pending with its statistics.
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
  - *Input pages.* Each parse thread drops its slices' pages (`madvise(MADV_DONTNEED)`), in
    parallel, instead of leaving 58,000 pages to the exit on one core.
  - *Page tables.* The main thread unmaps the log during pass 2 to free its page tables (about
    0.5 MB).
  - *Records.* A slice's records go when the last hour that reads them has placed them.
  - *Statistics.* Pending statistics are 24 bytes per endpoint.
  - *THP off*, so that one touched byte cannot fault in 2 MB.
- **Why exact.** Nothing released is read again.
  - The log's bytes are only read during the parse, and every endpoint name was copied.
  - A slice's records go when the reader count of its hours reaches zero, after the last of them
    has placed its values.
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

- **Design.**
  - *PGO.* The Dockerfile generates training data with a byte-identical copy of the organisers'
    generator (seed 7, sample scale), runs an instrumented build on it, and builds the final binary
    with the profile.
  - *Atomic counters.* The training run uses `-instrprof-atomic-counter-update-all`. With plain
    counters its threads lost updates, so every build got a different profile and a different
    binary.
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
  (technique 5, index 17).
- **Latencies.** One of up to 6 digits is converted by exact place values, a longer one digit by
  digit in a `u64`. A value of 2^20 − 1 or more does not fit a record's 20 bits and goes to the
  escape list, read back in order, so every `u32` latency is kept (technique 6).
- **Hours.** An hour is the first 13 bytes of the fixed-width timestamp, so byte order is time
  order, and every run of an hour is gathered from every slice: no bound on disorder is assumed
  (technique 7).
- **Order of the endpoints.** They are sorted by their first 32 bytes as big-endian numbers, then by
  the full name; the space after an endpoint is below every endpoint byte, so a proper prefix sorts
  first (technique 8).
- **Percentiles.** The ranks are the spec's nearest ranks, `ceil(p * n / 100)`. Groups of up to 64
  values count ranks exactly, ties included; larger groups take three nested
  `select_nth_unstable` calls, each rank read right after its own selection (technique 9).
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
| E2, E7: latency 4294967295 | 10 digits go to `parse_digits`, digit by digit into a `u64`; a latency of `2^20 - 1` or more goes to the escape list as a full `u32` |
| E2, E6: latency 0 | A value like any other: the escape marker is `2^20 - 1`, not 0, and rank counting never compares against the zero padding |
| E3, E8: statuses 499, 500, 599 | 5xx is "first status digit is `5`", so 499 is not an error and 500 and 599 are |
| E4, E12: an hour boundary, an earlier hour written later | Each line's own `MM-DDTHH` starts a run when it changes; runs are merged by their hour key, with no bound on disorder |
| E5: the first and last instants of the span | The hour key is the timestamp's first 13 bytes; `MM-DDTHH` is enough to see a change because the span is 72 hours |
| E9, E10, E11: equal timestamps, duplicate lines | Nothing is keyed by line or by time: every line is one record |
| E13: all seven methods | Skipped by the first space bit of the mask, whatever their length |
| E14: `/` and the 125-byte template | `/` is under 8 bytes, so it almost always takes the cold path (technique 5). An endpoint that ends past byte 91 does too; it loads 128 bytes from the endpoint's start, so a 125-byte endpoint and its space fit. Keys are 128 bytes, so every endpoint is compared in full |
| E15: 1,842 distinct endpoints | Ids 1 to 2,047 (11 bits): at most 1,842 + 12 used at 12 threads, since each thread keeps an id that lost its slot (technique 5) |
| Empty `requests.log` | `len @ 1..` fails, and the summary created at the start stays empty |

Trusted from the spec and not checked, since a check would be a branch per line: the 27-byte
timestamp, the 72-hour span, one space between fields, the 3-digit status, and 1 to 10 latency
digits then LF. A line that breaks them gives wrong output or stops at an assert, never a read
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
- **The id bound, measured.** Every seed has 1,842 endpoints, against 2,047 ids. With step 54's
  code up to 13 ids were lost in 20 runs of a seed; with steps 57 to 59, at most 1 id in 150 runs,
  and the bound is one per thread: at most 1,854 ids at 12 threads.
- **Time and memory on other seeds.** On the 30 fresh seeds step 59 ran 16.71 to 18.22 ms (medians
  of 7 runs; seed 1: 17.36 ms), with a memory peak of 18.97 to 20.19 MB on seeds 101-110 (max of 5
  runs; seed 1: 19.44 MB).
- **Not covered:** more than 20 threads (the replica has 20 CPUs).

## Error handling

Errors are panics with a message, and the build aborts on panic, so the exit status is nonzero:
- a missing file, or a failed `mmap`;
- an endpoint over 125 bytes, or more than 2,047 endpoint ids;
- records past their room;
- an hour whose written length differs from its computed one.

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
- **The memory peak.** The board read 18.79 to 19.8 MB in six runs of step 56's engine, the
  replica up to 19.58 MB as the max of 20 runs, and the final scoring takes the largest of five
  peaks. The peak sits
  2 to 3 ms into pass 2, with about 8.2 MB of records still alive. Releasing each part's records as
  soon as its hours are placed needs a different order of pass 2 (the largest hours first in each
  block is there for the peak already). Up to 3 MB of each peak is the kernel's per-CPU charge
  batching, which no program change removes. Not tried.
- **An earlier hand-off that the box can use.** Handing pass 2 the hours before the name order
  (PERF.md, steps 57 and 58) measured 1.07% faster on the replica, but ran slow in eight of its
  twelve board runs. A version in which no worker waits for the order (a write step skips until it
  is ready, and the last one computes it if needed) measured 0.70% faster than step 59 on the
  replica, for 11 more lines. It needs board samples first: the replica does not show when the
  merge's thread, the 13th on 12 CPUs, gets a CPU. Not submitted.
- **Pass 2 during the parse.** The parse is about 2.3 ms above the memory floor. Hours whose
  slices are all parsed could start early, but slices finish in waves, so we estimate at most
  about 1 ms for about 100 more lines. Not built.
