# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## What the program does

It reads `requests.log`: 3.1 million lines (239 MB) of
`timestamp method endpoint status latency`. It writes `summary.txt`: one line per (UTC hour,
endpoint) group, 126,908 lines (10.1 MB) for seed 1. Each line has the count, the nearest-rank
p50, p95 and p99 latency, the 5xx count and the 5xx rate in parts per million. The output is
sorted by hour, then by endpoint in byte order.

Two things make the problem harder than it looks. The lines are not in time order: eight shards
are interleaved in arrival order, and the spec promises no bound on the disorder. And exact
percentiles need every latency of a group, not a running summary.

## Where the time goes on the box

The best board run is 0.0189 s (run-6cca232c0f6a). Two timing probes on the box split it. A probe
is a temporary build that exits with the elapsed time as its exit code, in 0.1 ms units, because
the board reports only T and the exit code of a failed run (PERF.md, step 32):

- The parse ends at 11.0 ms (run-7d4218a4cdba, exit code 110).
- A build that only touches one byte per 64 of each slice ends at 7.8 ms (run-be516aa463a5,
  exit code 78). That is the floor of reading 239 MB from DRAM and taking its page faults.

So the parse runs about 3 ms above the memory floor, and about 8 ms of T comes after the parse:
the merge, pass 2, the write and the exit. Memory is the other axis. About 4.7 MB is the
container's floor (an empty input). The output counts, because `/out` is a tmpfs. The mapped
input does not: the harness has it in the page cache already, charged to the host.

## The design

The program has five parts. Each one exists because of one bottleneck.

| Part | Bottleneck it addresses |
|---|---|
| Mapped, padded input; one slice per thread | 239 MB to read once; no copy, no memory charge, no end-of-file special case |
| Branch-free AVX2 line parse | Branch mispredictions on the method and endpoint length; instructions per line |
| 4-byte records, hours as runs | Memory per record; a calendar per line; unbounded disorder |
| Pass 2 by hour, counting sort, selection | Cache misses of a global sort; full sorts for three ranks |
| Output written in place, out of order; no teardown | Serial writes; page faults on one file lock; the cost of freeing at exit |

**Parse (pass 1).** The log is mapped, followed by 256 zero bytes, so that fixed-size vector
loads near its end stay inside the mapping. It is cut at line ends into one slice per hardware
thread; each thread finds its own cut. The timestamp is a fixed 27 bytes, so one 64-byte AVX2
window right after it holds the spaces that end the method and the endpoint. Their positions come
from `tzcnt` on the mask, with no branch on the method's letters or the endpoint's length. The
endpoint's identity is its bytes zero-padded to 64, loaded and masked straight from the line. All
threads share one endpoint table: lookups are plain loads, and the rare insert claims its slot by
compare-and-swap, so there is no merge of tables after the parse. The latency's digits are
converted by three multiply-add steps. A line becomes one `u32`. Each thread then gives back the
pages of its slice.

**Merge.** One set of threads runs both passes: a worker publishes its parse and waits, and the
main thread does the short serial merge. It sorts the endpoints, sorts all runs by (hour, slice),
counts how many hours read each slice's records, and maps the output. Then it publishes all of
it. While pass 2 runs, the otherwise idle main thread unmaps the log (which frees its page tables
too) and gives back the endpoint keys.

**Pass 2.** Threads take whole hours from a counter, in blocks of one hour per thread, the largest
hour of each block first. A thread gathers an hour's runs from every slice and groups the
latencies by endpoint id with a counting sort whose arrays fit in L1/L2 (an hour is 14K to 73K
records). Records are given back as soon as no hour still reads them. Each group's p50, p95 and
p99 come from three selections; groups of up to 32 values are sorted instead. The thread keeps
only the statistics (20 bytes per endpoint) and the hour's exact output length, publishes the
length, and takes the next hour.

**Write.** The output file is sized to a bound and mapped shared. Once the lengths of all earlier
hours are published, an hour's start is their sum, and the thread holding it formats it straight
into the mapping. Each line is built backwards in a stack buffer from a two-digit table. The
program then cuts the file to its length and exits without freeing anything.

## Techniques

Each technique below names the bottleneck, what the design does about it, and why the output
stays exact for every seed of the generator.

### 1. Mapped input with a zero pad

- **Bottleneck.** Reading 239 MB into a buffer would cost 239 MB of memory and a copy. Vector
  loads near the end of the file would need a slow tail path.
- **Design.** `mmap` of the pre-warmed file: no copy, and its pages are charged to the host. The
  range `len + 256` is first reserved as anonymous zero pages, then the file is mapped over its
  start with `MAP_FIXED`. Every line can be read with full 64-byte loads.
- **Why exact.** The pad is zeros, and every load is bounded (the SAFETY comment in `parse` adds
  up the offsets: the furthest load ends within 256 bytes of a line start). The dataset is mounted
  read-only, so the bytes cannot change under the mapping.

### 2. Line-aligned slices, one per thread

- **Bottleneck.** A serial scan for line ends would be a pass over 239 MB on one core.
- **Design.** Cut `p` is just after the last LF before byte `len * p / threads`. Each thread
  computes its own two cuts.
- **Why exact.** Cuts never decrease and always follow an LF, so every line is in exactly one
  slice. Two cuts in one line give an empty slice, which is harmless. The thread count changes
  the slices, the runs and the order of endpoint ids, but none of them reaches the output: the
  output was checked byte for byte at 1 to 16 threads.

### 3. Branch-free AVX2 field ends

- **Bottleneck.** There were 2.2 branch mispredictions per line, from the method (GET 46%, POST
  37%) and from the endpoint length (49% shorter than 32 bytes).
- **Design.** One 64-byte window of space bits after the timestamp. The first set bit ends the
  method, the next one ends the endpoint (`tzcnt`, then clear the lowest bit). Endpoints whose
  space is past byte 92 (7.6% of lines) take further 32-byte windows, at most to the one that
  holds the space after a 125-byte endpoint. `vpmovmskb` is inline asm: written as the
  intrinsic, LLVM turns it into a vector of booleans, and on CPUs without AVX-512 (the box) it
  then built the 64-bit mask one bit at a time.
- **Why exact.** The timestamp is fixed at 27 bytes and the fields are separated by one space
  (spec), so the first two spaces after byte 27 are the method's and the endpoint's. A line with
  no method space, or an endpoint over 125 bytes, fails an `assert!` and stops the program.

### 4. Endpoints as 64-byte vector keys in one shared table

- **Bottleneck.** Per-thread tables need a serial merge. `==` on a 64-byte array compiled to a
  `memcmp` call per line (12% of one thread's cycles).
- **Design.** The key is the endpoint's first 64 bytes, ANDed with a sliding window into `KEEP`
  so that bytes from the space on are zero. The hash folds the key to 16 bytes and takes two CRC32
  steps. One open-addressing table with 8 times as many slots as possible ids (11% load) is shared
  by all threads. A lookup is two vector XORs and one `vptest`. The insert is `#[cold]`: it takes
  an id, copies the key and the name's bytes, and publishes the id with a compare-and-swap.
- **Why exact.** No endpoint byte is zero, so for endpoints shorter than 64 bytes, equal keys mean
  equal endpoints. Longer ones (63 of the 1,842 endpoints, on 6.2% of lines) are also compared
  byte by byte. If two threads insert the same endpoint at once, the slot's compare-and-swap
  picks one id, and every record gets that id; the losing id is never in a record, so its groups
  are empty and print nothing. More than 2,048 ids stops the program. The names are copies, so
  they stay valid after the log's pages are given back.

### 5. 4-byte records with an escape list

- **Bottleneck.** Memory. The records are the largest thing the program allocates. With 8-byte
  records alive while the output was written, the box measured 44.66 MB (run-73fe01d551bc); 4-byte
  records, freed before the output, took it to 24.3 MB (run-6154ff9e18eb, PERF.md step 8).
- **Design.** A record is `endpoint id << 21 | 5xx << 20 | latency`. A latency of `2^20 - 1` or
  more is stored as that marker, and its value goes to a side list (1.2% of lines). The side-list
  slot is written for every line and kept only when needed, so there is no branch. Both arrays are
  sized for the worst case, so the loop has no capacity checks; only touched pages get memory.
- **Why exact.** Latencies are 32-bit (spec). Escaped values are read back in the order they were
  written, run by run, from each run's first escape index. A status is 5xx exactly when its first
  digit is `5`.

### 6. Hours as runs, with no calendar

- **Bottleneck.** Parsing a timestamp per line was about 80% of the reference's time. A bound on
  disorder would make the program wrong on a seed that breaks it.
- **Design.** Records carry no hour. Shards write in time order, so the file is a sequence of
  runs of lines from one hour. A thread starts a new run when bytes 5..13 of the timestamp
  (`MM-DDTHH`, one 8-byte load) change. The run's key is the first 13 bytes (`YYYY-MM-DDTHH`) read
  as a big-endian number. The merge sorts all runs by (hour, slice) and cuts them into hours.
- **Why exact.** The timestamp has a fixed width, so byte order is time order, and the 13 bytes
  are the start of the output's hour label. All timestamps lie in one 72-hour span (spec), so lines
  that share `MM-DDTHH` share the year. A line out of order only starts another run: pass 2 gathers
  every run of an hour from every slice, so no bound on disorder is assumed.

### 7. Byte-order endpoint sort on 32-byte keys

- **Bottleneck.** The sort is on the serial path between the passes. With 16-byte keys, 600 of
  the 1,842 endpoints tied and needed a name compare; the sort was 0.20 ms of a 0.27 ms merge on
  the test server.
- **Design.** Endpoints are sorted by the first 32 bytes of their zero-padded keys, as two
  big-endian `u128` numbers, then by the full name on ties.
- **Why exact.** Big-endian numbers compare like their bytes. Zero padding makes a proper prefix
  sort first, which is byte order, since no endpoint byte is zero. Ties fall back to the full
  bytes.

### 8. Hour-local counting sort and selection

- **Bottleneck.** Global histograms and a scatter over a 12 MB array did not scale with threads
  (1 thread 195 ms, 12 threads 60 ms on the test server). A full sort per group is more work than
  three ranks need.
- **Design.** A thread takes one hour, counts per endpoint id (the 5xx count in the high half of
  the same `u64`, one add per record), turns the counts into starts, and places each latency.
  The arrays fit in L1/L2. p50 comes from `select_nth_unstable`, p95 from a selection within the
  part above p50, and p99 within the part above p95. Groups of up to 32 values (91% of groups) are
  sorted instead; 32 was the best of 16, 32 and 64. The scratch array is sized once, for the
  largest hour.
- **Why exact.** Ranks are `ceil(p * n / 100)`, 1-indexed. Each value is read right after its
  own selection, before the next one reorders the slice (reading all three at the end gave wrong
  output once, see PERF.md). The selections are exact; only the order of the work changes.

### 9. Out-of-order compute, in-order placement

- **Bottleneck.** Writing hours in order made a thread that finished a small hour wait for a large
  one. A write phase after a barrier was 2 ms slower: twelve threads faulting in the shared
  output at once.
- **Design.** Each hour's exact byte length is computed from its statistics and published. The
  hour stays pending with its statistics. Once every earlier hour's length is known, its start
  is their sum, and its thread writes it into the shared mapping, while the other threads keep
  computing. In each block of one hour per thread, the largest hour goes first, so it ends with
  its block instead of holding up the small hours after it (up to 30 hours were pending at once).
- **Why exact.** The length comes from the same numbers the writer prints (`line_len` and
  `write` share `numbers`), and the writer asserts that it wrote exactly that many bytes. The
  rate is `errors * 1000000 / count` in integers. Every hour's bytes are its own range of the
  file, and the file is cut to the sum of the lengths.

### 10. Memory follows the work

- **Bottleneck.** The peak is in pass 2: records, the growing output, the scratch arrays and the
  pending statistics, all at once.
- **Design.** Each parse thread drops its slice's pages (`madvise(MADV_DONTNEED)`), in parallel,
  instead of leaving 58,000 pages to the exit on one core. The main thread unmaps the log during
  pass 2 to free its page tables (about 0.5 MB). Long runs (142 runs, 93% of the records) are
  dropped as soon as they are placed; a slice's other records go when the last hour that reads
  them is done. Pending statistics are 20 bytes per endpoint. Transparent huge pages are off, so
  that one touched byte cannot fault in 2 MB.
- **Why exact.** Nothing released is read again. The log's bytes are only read during the parse,
  and every endpoint name was copied. A slice's records go when the reader count of its hours
  reaches zero, after the last of them has placed its values. Released input pages would fault
  back in from the page cache if they were read; anonymous pages would read as zeros, but nothing
  reads them.

### 11. No teardown

- **Bottleneck.** Dropping the tables one free at a time took 4 to 5 ms on the box.
- **Design.** Once the output is cut to its length, the program calls `exit(0)`.
- **Why exact.** The output is complete in the page cache before the exit; the kernel frees the
  rest.

### 12. Build

- **Design.** Profile-guided: the Dockerfile generates training data with a byte-identical copy of
  the organisers' generator (seed 7, sample scale), runs an instrumented build on it, and builds
  the final binary with the profile. The training run uses atomic profile counters
  (`-instrprof-atomic-counter-update-all`): with plain counters, its threads lost updates, so
  every build got a different profile and a different binary. Loops are aligned to 64 bytes
  (`-align-loops=64`), so that their placement depends less on unrelated code. Fat LTO, one
  codegen unit, `panic = "abort"`, a static binary, and `target-cpu=native` (the board builds on
  the box). A `compile_error!` stops a build without AVX2.
- **Why exact.** The build changes speed only. Each correctness check runs the binary built by this
  directory's Dockerfile.

## Error handling

Errors are panics with a message, and the build aborts on panic, so the exit status is nonzero:
a missing file, a failed `mmap`, an endpoint over 125 bytes or a line without a method space,
more than 2,048 endpoint ids, and an hour whose written length differs from its computed one. The
spec's empty input gives an empty summary.

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise`, `munmap` and `prctl` declared by
hand. All the code is one file, `src/main.rs`.

## With another 24 hours

- **Close hours while the parse is still running.** The parse is about 3 ms above the box's memory
  floor, and pass 2 is compute that could fill those stalls. A parse thread that later meets a
  line of a closed hour would set a flag, and the program would then redo pass 2 as it does now,
  so exactness would not rest on a disorder bound. About 100 more lines.
- **A 128-byte endpoint key.** Endpoints are at most 125 bytes, so a zero-padded 128-byte key
  would identify every endpoint and remove the byte compare for long ones.
