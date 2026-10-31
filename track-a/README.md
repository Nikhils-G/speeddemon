# Track A: Ingest

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

### The problem

3.1 million log lines (239 MB) become 126,908 output lines (10.1 MB): one per (UTC hour, endpoint)
group, with the count, nearest-rank p50/p95/p99 latency, the 5xx count and the 5xx rate in parts per
million. The lines are not in time order (eight shards, interleaved in arrival order, with no
promised bound on disorder), and the percentiles need every latency of a group.

### Where the time goes

The reference spends about 80% of its 32 s parsing timestamps. Once parsing is vector code on every
core, the input itself dominates: 239 MB does not fit the box's 24 MB L3, so it streams from DRAM
once, and mapping it costs page faults. A run that exited after one phase with the elapsed time as
its exit code (PERF.md, step 12) split the box's 35 ms at that point into parse 17.5 ms, merge
1.25 ms, pass 2 about 6.75 ms, write about 3.5 ms, and 4–5 ms of freeing memory before the exit.
The steps since then each took on one of those parts. At step 19 the test server showed parse
9–10 ms, merge 0.4 ms and pass 2 about 6 ms; at step 27 it showed parse 10.6 ms, merge 0.3 ms,
pass 2 6.3 ms, and 1.8 ms outside `main` (starting the process and its exit).

Memory is the other axis. About 5.2 MB is the container's floor, and the output counts because
`/out` is a tmpfs; the mapped input does not, because the harness has it in the page cache already.
The rest of the peak is about 12.4 MB of records and output and about 3 MB of per-thread scratch.

### The design

**Parse (pass 1).** The log is mapped, followed by 256 zero bytes so that fixed-size vector loads near
its end stay inside the mapping. It is cut at line ends into one slice per hardware thread: each cut moves back to just after the
last line feed before it. The
timestamp is a fixed 27 bytes, so one 64-byte AVX2 window starting right after it holds the spaces
that end the method and the endpoint; their positions come from `tzcnt` on the mask, with no branch
on the method's letters or the endpoint's length (7.6% of lines need further 32-byte windows). The
endpoint's identity is its bytes zero-padded to 64, loaded and masked straight from the line. All
threads share one open-addressing endpoint table, allocated zeroed so that its pages are mapped as
the parse touches them: lookups are plain loads, and the rare insert copies the endpoint's bytes and
claims its slot by compare-and-swap, so there is no merge after the parse. The latency's digits are
converted by three multiply-add steps. A line becomes one u32: endpoint id, a 5xx flag, and the
latency, or an escape marker when the latency needs more than 20 bits (1.2% of lines), with the
value kept in a side list. Each thread then gives back the pages of its slice, so the exit does not
have to unmap them all on one core.

**Hours without a calendar.** Records carry no hour. Shards write in time order, so the file is a
sequence of runs of lines from one hour, and each thread records where its runs start, keyed by the
timestamp's first 13 bytes `YYYY-MM-DDTHH` read as one big-endian number. Those bytes sort the
hours and are the start of the output's hour label, so there is no date arithmetic. Between the
passes, all runs are sorted by (hour, slice) and cut into hours.

**Aggregate and write (pass 2).** Threads take whole hours from a counter. A thread gathers one
hour's runs from every slice and groups the latencies by endpoint id with a counting sort whose
arrays fit in L1/L2 (an hour is 14K–73K records). The records' pages are given back as soon as no
hour still needs them. Each group's p50, p95 and p99 come from three selections
(`select_nth_unstable`, each within the part above the last); groups of up to 32 are sorted instead.
The thread keeps only those statistics and the hour's exact output length, publishes the length, and
takes the next hour. The output file is sized to a bound and mapped shared. Once the lengths of all
earlier hours are published, an hour's start is their sum, and the thread holding it formats it
straight into the mapping. Writing while other hours are still being computed matters: when every
thread waited for all the lengths and then wrote at once, the page faults on the shared file
serialised and pass 2 took 2 ms longer (PERF.md). The program then cuts the file to its length and
exits without freeing anything.

### Why the output is exact

- **Hours.** An hour is the timestamp's first 13 bytes, read from the first line of each run. The
  fixed width makes byte order the same as time order. The parse starts a new run whenever bytes
  5–12 (`MM-DDTHH`) change; within the spec's 72-hour span, lines that share those bytes share the
  year. A line out of order only starts another run: pass 2 gathers every run of an hour from every
  slice, so no bound on disorder is assumed.
- **Endpoints.** No endpoint byte is zero, so for endpoints shorter than 64 bytes, equal zero-padded
  keys mean equal endpoints. Longer ones (63 of the 1,842, on 6.2% of lines) are also compared byte
  by byte. Every endpoint's bytes are copied when it is added, so neither that comparison nor the
  output reads the input's pages after they are released. If two threads insert the same new
  endpoint at once, one id wins and the other is never used in a record; its groups are empty and
  print nothing. Running out of ids (there are 2,048 for the spec's 1,842 endpoints and such
  duplicates) stops the program rather than giving a wrong record.
- **Order.** Endpoints are sorted by their first 16 bytes as a big-endian number, then by their full
  bytes on ties. Zero padding makes a proper prefix sort first, which is the spec's byte order.
- **Numbers.** The digit conversion is exact for 1 to 10 digits (two 8-digit halves). Latencies of
  2^20 − 1 or more are kept exactly in the side list and read back in the order they were written. A
  status is a 5xx exactly when its first digit is `5`. Ranks are `ceil(p * n / 100)`, and each value
  is read right after its own selection, before the next one reorders the slice. The rate is
  `errors * 1000000 / count` in integers.
- **Layout.** Each hour's length is computed from the same numbers the writer prints, and the writer
  asserts it wrote exactly that many bytes.
- **Checked:** the seed-1 hash, the reference outputs of sample seeds 2–4, a full-scale seed the
  board does not use, small generated seeds, an empty input, and 1 to 16 threads (PERF.md).

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise` and `prctl` declared by hand. All the
code is one file, `src/main.rs`.

## With another 24 hours

- **Measure the parse floor on the box.** A throwaway build that only touches one byte per 64 of each
  slice would show how much of the parse is DRAM traffic and page faults, and so whether the parse
  code or the pass structure is the lever. The last two experiments (PERF.md, steps 24–25) suggest
  the parse code is close to that floor.
- **Close hours while the parse is still running.** Once the parse is far enough past an hour's last
  line, a thread could aggregate and write it. Exactness would rest on a check, not on a disorder
  bound: a parse thread that later meets a line of a closed hour sets a flag, and the program then
  redoes pass 2 the way it does now. This removes the gap between the passes and overlaps pass 2's
  compute with the memory-bound parse, for about 100 more lines.
- **A 128-byte endpoint key.** Endpoints are at most 125 bytes, so a zero-padded 128-byte key would
  identify every endpoint exactly, and remove the byte compare for long endpoints and the copy of
  every endpoint's bytes.
- **Two line cursors per thread**, as the fastest One Billion Row Challenge entries do, to overlap the
  latency of each line's dependent loads.
