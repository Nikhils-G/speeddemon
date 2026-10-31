# Track C: performance log

Every step from the reference to the current version, each with the live-board run that measured it.
Points are the board's 50·log10(K). Reruns of one commit spread about 3%. Steps were first timed on a
16-vCPU AMD test server (21 runs each); its kernel-side costs (page faults, cross-core atomics) are
much higher than the box's, so only the board decided what stayed.

| Step | Commit | Change | Run ID | T (s) | M (MB) | S | Points |
|---|---|---|---|---|---|---|---|
| 0 | | The reference, unchanged | | 14.44 | 367 | 108 | 0 |
| 1 | cf9c345 | mmap; quote-parity parallel split; per-thread name interners, serial merge | run-0a002c9fed81 | 0.445 | 145 | 441 | 37.2 |
| 2 | c67f1ca | One lock-free name table shared by all threads; a `fetch_or` bit per (name, country) | run-ce648adbb50f | 0.125 | 13.3 | 460 | 61.3 |
| 3 | 9ac00a6 | Transparent huge pages off: M varied 13–24 MB between runs, now stable | run-03a9af09852c | 0.130 | 12.4 | 466 | 61.1 |
| 4 | 3c69a45 | Records in batches of 16 with prefetched table entries | run-21828838d277 | 0.1045 | 13.1 | 525 | 62.8 |
| 5 | 42acaa7 | Profile-guided build | run-e999df62b7ec | 0.0979 | 13.6 | 528 | 63.4 |
| 6 | d893204 | SIMD bitmaps (quote, comma/LF, carry-less multiply for "inside quotes"); guessed cuts, checked; (hash, country) pairs deduplicated per hash partition between rounds, no atomics | run-0c8cee11ba10 | 0.0779 | 17.2 | 680 | 64.0 |
| 7 | acdbc3c | `vpmovmskb` kept opaque to LLVM (see below) | run-e351b1a1c034 | 0.0553 | 16.8 | 691 | 67.8 |
| 8 | fcd0641 | regions.csv parsed in two parallel passes; groups numbered in output order | run-0be106cb3cd9 | 0.0494 | 19.8 | 777 | 67.9 |
| 9 | 6e3aed6 | Every pair buffer gets its capacity (`vec![v; n]` had cloned them empty) | run-1cd5ca162a83 | 0.0478 | 16.3 | 780 | 69.1 |
| 10 | 2246001 | Spinning barrier between rounds | run-7ace82c8c895 | 0.0454 | 16.5 | 816 | 69.5 |
| 11 | a3acbf3 | Partition tables at most half full, with a dense id per name | run-6a39052adf13 | 0.0446 | 17.1 | 815 | 69.5 |
| 12 | 470e7e3, cb90a23 | Fewer lines: a wrong guessed cut reruns the file as one run (no second splitter); `format!` output; the mapping parsed directly again (pread gave nothing on the box) | run-2b4b2c97a9c1 | 0.0461 | 15.7 | 722 | 69.9 |
| 13 | 152db08 | The barrier's spin is bounded, then waiters sleep on a condition variable | | | | | |
| 14 | 0030dfe | Exit as soon as the output is written; each run's pages dropped by the thread that parsed it | run-a476140e0850 | 0.0422 | 15.7 | 744 | 70.8 |
| 15 | eb2cf64 | Per-record fast path (one 64-byte bitmap window, one 32-byte tail compare, one AVX2 number pass); batched dedupe; mallopt top pad; regions passes in the parse threads | run-db85f475f012 | 0.0268 | 16.0 | 914 | 75.0 |
| 16 | 007cea6 | Fewer lines: a byte-by-byte spec 2.1 parser replaces the SIMD general scanner; regions.csv in one pass, groups numbered as first seen and sorted at output; worker 0 is the main thread and exits right after writing; std locks instead of `UnsafeCell` | run-20006a569215 | 0.0273 | 16.2 | 753 | 75.3 |
| 17 | 48290f6 | One-pass digit loop for regions.csv; name and note bytes packed in one column; branchy min/max; fast-path bounds check out of the loop | run-bf1770e98008 | 0.0260 | 16.7 | 740 | 75.8 |
| 18 | 2bf221c | Each thread owns 4 name partitions (threads × 4) in every round: no locks, tables stay in its cache | run-b9a64cd5e4d6 | 0.0258 | 15.5 | 733 | 76.2 |
| 19 | 7a7c57a | Non-temporal prefetch of the record stream 1 KB ahead, so the scan takes less of L2 and more of the 1 MB region table stays there (a box-only effect: no change on the AMD test server) | run-9389bfd9b77d | 0.0253 | 15.1 | 737 | 76.5 |
| 20 | eba58c8 | Same engine in fewer lines: no NamesFull retry (tables sized from the generator's 100,000-name pool, 8 standard deviations of room), errors as strings, `aggregate` folded into `run`, a one-counter barrier, regions chunks cut by `run_start`, `parse_run` kept out of line | | | | 590 | |

## What each step taught

- **Step 2.** Merging per-thread name sets was most of the time. A shared table where the thread
  whose `fetch_or` flips a (name, country) bit counts that pair needs no merge at all.
- **Step 6** halved the parse but made memory and lines worse, so it scored less than its T alone:
  the partitioned dedupe holds the pair buffers (about 4 MB) and the tables.
- **Step 7** is the largest single gain. Written naturally, `movemask(lo) | movemask(hi) << 32`
  becomes, inside LLVM, a 64 × i1 vector; on CPUs without AVX-512 (the box and the test server) it was
  then rebuilt one bit at a time with `bextr`/`vpinsrb`: a quarter of the run time. The development
  laptop has AVX-512 and compiled it well, which hid the problem until the server's disassembly was
  read. An inline-asm `vpmovmskb` cannot be rewritten.
- **Step 8.** The serial lookup-table parse was 4.4 ms, a tenth of the run at that point.
- **Step 9** was a plain bug, found by reading: cloning a `Vec` does not keep its capacity, so 63 of
  64 buffers grew by doubling.

- **Step 13** followed a run of the unchanged code that took 0.0778 s instead of 0.045. A thread
  that loses its core to another process can be starved by eleven spinning siblings; with a
  bounded spin that cannot happen, and the time is the same.
- **Step 14 came from measuring the box itself**: for one run the program exited right after a
  phase with the elapsed time as its exit code (0.25 ms units; the board shows a failed run's exit
  code). On the box, regions and cuts take 2.25 ms, the parse rounds 36.75 ms (only about 4.5 of
  them deduplication), and about 6 ms went to the kernel unmapping the 202 MB input at exit.

- **Step 15** (not yet on the board). Server, PGO builds as the Dockerfile makes them, 12 threads:
  38.3 → 24.5 ms. Single thread on full-seed1: 1.78G → 0.98G instructions, 590M → 372M cycles,
  about 506 → 282 instructions per record (45M of the total is fixed cost: regions, startup).
  - The fast path takes the common record (`id,"name","note",region,amount` with quoted or empty
    text) from the 64 bytes at its start (128 when the note runs long). Any other shape, or any
    failed check, goes to the old scanner, now `#[cold]` and `#[inline(never)]`: inlined, it took
    the hot loop's registers (-18 instructions per record).
  - Id, region and amount are converted in one AVX2 pass. pshufb drops the dot, a byte table
    masks each field's digits, and `maddubs`/`madd`/`packus`/`madd` do the conversion.
  - Branches must not depend on CRLF vs LF, which is a coin toss. One `||` on it cost
    +1.7M branch misses and +15 cycles per record.
  - The name hash has no branch on the length for 16 bytes or fewer. It is not recursive any
    more: the recursion had kept it from being inlined (+20 instructions).
  - The dedupe takes a whole slice of pairs at a time, with the partition's fields as locals. Its
    probe compares a bucket of 4 slots at once, which halved the branch misses (2.0M → 1.0M).
  - Worker threads got their own malloc heaps, which grew by mprotect, 800 calls per run. Each one
    stalls every other thread's page faults. `mallopt(M_TOP_PAD, 32 MB)` brought it to 68 calls:
    -14 ms of system time and -2.8 ms wall on the server.
  - Each thread finds its own cuts. regions.csv lines go through a SWAR fast path. Both
    regions.csv passes run in the parse threads (one spawn of threads instead of three).

- **Steps 16–18** (round 2). Times on the test server, 12 threads, PGO + static builds, medians of 21–31
  interleaved runs (they include about 2.4 ms of fork/exec):
  - 894c8a7 24.2 ms → step 16 23.9 → step 17 23.7 → step 18 22.0–22.9. Memory 15.0 MB → 15.5 MB at
    12 threads (step 18 is 1 MB below step 17 there).
  - Phases at 12 threads, before step 16: setup until the first parse 1.8 ms, parse rounds 12.7, dedupe
    3.3, then 0.75 ms of thread joins and frees after the last barrier, 0.3 ms output. Step 16 took the
    setup to 1.3 ms and removed the joins: worker 0 writes the output and calls `exit`.
  - Two regressions found on the way, both 1 ms at 12 threads and invisible single-threaded: the
    per-thread pair-buffer headers sitting inline in one shared `Vec` (false sharing on their lengths),
    and `parse_run` taking `&Regions`. Regions holds a `Mutex` inline, so the reference is not
    read-only to LLVM, and every record reloaded the table's address with two loads in front of the
    lookup. `parse_run` takes the `&[AtomicU8]` slice.
  - Step 18 had been rejected before (64 partitions over 12 threads, 6 vs 5 each, with locks). With
    exactly 4 partitions per thread the dedupe is balanced and a thread's tables never leave its core.
  - The single largest stall is the region-table lookup: 25–30% of single-thread cycles sit on the
    load. It is cache footprint, not latency: with the id masked to a smaller table (wrong output,
    timing only) 16 KB saves 2.3 ms, 256 KB 1.6 ms, 512 KB 0.7 ms. Prefetching does not help (below).

- **Step 20** (round 3) kept the speed: server, PGO + static, 22.35 ms vs 22.24 for 7a7c57a, 959M vs
  955M user instructions single-threaded. Two things nearly lost it:
  - Folding `aggregate` into `run` put `parse_run`, inlined, inside a bigger worker closure. Its
    registers were then allocated with the worker's: +32M instructions under PGO, 2–4% slower.
    `#[inline(never)]` on `parse_run` brought the speed back.
  - A `while at < run_start(...)` loop bound recomputed the chunk end for every regions.csv line:
    +10M instructions.
  - Most of the 143 lines were rustfmt's own expansions. The harness formats with defaults (a
    struct literal over 18 columns, or a call or chain over 60, goes vertical), so code written to
    fit them costs fewer lines without any trick.
- **The "lean kernel" did not pay off in Rust source.** On top of step 20, I tried these together:
  - the next record start from an LF bitmap of the tail;
  - the BOM from `p[0] == 0xEF`;
  - quote state from the bitmap bits;
  - every validity test folded into one `&`/`|` expression;
  - unchecked KEEP and partition indexing;
  - 64-byte Totals rows.

  User instructions went from 985M to 1011M, and the 12-thread time was +0.5 ms. LLVM split the one
  test back into six branches, and the hot loop kept about 21 stack references (the offset, the
  pointer, `name_end`, both halves of the hash's multiply, the totals and pairs pointers). Two
  checks must stay anyway: `digits_ok` and the field-width test. Without them, a wrong guessed cut
  on spec 2.1 input (the spec's own RULES fixture has an LF followed by a BOM inside quotes) is
  parsed silently instead of failing over to the one-run parse.

## Experiments that did not make it (measured, reverted)

| Experiment | Where measured | Result |
|---|---|---|
| Populate the tables before the threads start | board run-b4f2d95150d7 | T 0.106 vs 0.104, M 15.1: worse; 20% faster on the server |
| `prefetchw` and batches of 32 | board run-d1cae4327c17 | T 0.0995 vs 0.0979: no gain |
| Read records.csv with pread through 128 KB buffers instead of the mapping (2341842) | board run-4381f7c8201d | no change on the box (6 ms faster on the server); kept, neutral |
| Shared lock-free table again, on top of the SIMD parser (d1200be) | board run-edc1e75d5cee | T 0.0539, M 13.9, 68.3 pts vs 69.5: reverted. 71 ms vs 41 ms on the server, where cross-core atomics are far dearer |
| Batch of 16 with prefetched region-table entries | server | cycles +4% |
| Each thread always owning the same partitions | server | +1 ms (64 partitions do not divide over 12 threads) |
| Same with `threads × 8` partitions of any size | server | +0.7 ms |
| Prefetching the next pairs' table slots in the dedupe | server | no change |
| Separator offsets written for 4 KB at a time, simdjson stage-1 style | server | instructions +15%, cycles +14% |
| Rounds 4 / 16 instead of 8 | server | 4: −1 ms but +4 MB; 16: +1.8 ms for −1.7 MB |
| Runs claimed dynamically within a round | server | no change |
| Dense name ids from one shared hash table, so pairs are u32 and dedupe needs no probing | server | 12 threads 45 → 67 ms: early inserts contend |
| Fast path over a 128-byte window from the record start (u128 bitmaps) | server | 519 instructions per record; u128 shifts and counts, and spills. Replaced by 64 bytes plus the tail |
| Region comma and dot found by SWAR in 8-byte words | server | about the same instructions as one 32-byte compare, more lines |
| Fast path not inlined | server | 344 vs 317 instructions per record |
| All tables and pair buffers in a few big zeroed allocations, fixed pair segments | server | 44 ms vs 26: twice the page faults (zero-page read, then write) and 171 ms of system time |
| 128 or 256 partitions so that a table fits in L1 | server | no change single-threaded |
| Hash computed before the record's checks | fuzz | crashed: a rejected record can have a negative name length. Hash only after the checks |
| Records counted one late, each region entry prefetched when parsed | server | +130M instructions, same time |
| Batches of 4 / 8 / 16 records parsed with prefetches, then counted | server | +80M instructions; 12 threads 24.5 / 25.1 / 25.8 ms vs 24.5 |
| Compact region table: 32-id blocks (`rank << 32 \| bits`, 250 KB) and 200 KB of groups | server | parse 16.2 ms vs 12.8: two dependent random loads cost more than one into 1 MB |
| Name table 7/8 full with seen rows by slot (no ids) | server | −23% table memory, +0.8M branch misses, +1 ms |
| 4096-slot tables (half the load) | server | +1.3 ms: the larger footprint |
| `madvise(MADV_POPULATE_READ)` on each run before parsing it | server | 23.9 vs 23.6 ms |
| Threads started as a binary tree | server | no change: thread starts still serialize in the kernel, about 55 µs each |
| 128 partitions (before step 18) | server | 23.8 vs 23.6 ms |
| ROUNDS 6 / 10 / 12 / 16 after step 18 (8 kept) | server | 22.6 / 22.5 / 22.7 / 23.2 ms vs 22.4; memory 17.5 / 14.8 / 14.4 / 13.8 MB vs 15.6 |
| Lean kernel (see step 20) | server | 1011M vs 985M instructions, +0.5 ms |
| A fast loop up to the last 256 bytes, then a slow loop for the tail | server | 982M vs 985M instructions; 22.60 vs 22.84 ms, within noise |
| Unchecked partition index on top of that | server | 972M instructions; 22.73 ms, within noise |
| cmov min/max instead of branches | server | +10M instructions, +0.4 ms |
| First hash round always taken (no branch for 17–24-byte names) | server | branch misses 960k → 571k, but +30M instructions (+57M under PGO), +0.3 ms |

## Correctness checks run before every push

sha256 of the seed-1 output; byte comparison with the reference output for sample seeds 2–4 and the
holdout-like seed 99; the same output at 1, 2, 3, 5, 12 and 16 threads; 6 tiny and 6 sample seeds
freshly generated and compared with the reference at 1, 3 and all threads; the `test_c_join.py`
fixtures, as written and with their records repeated 300 times so the fast path sees them; forced
wrong cuts, which must fall back to one run and still give the same output; a tiny name table,
which must exit non-zero; 60 mutated or truncated records.csv files and 21 mutated
regions.csv files, which must exit as before, without a crash; `objdump | grep -c vpinsrb` = 0 on the
server build.
