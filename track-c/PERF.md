# Track C: performance log

Every step from the reference to the current version, with the live-board run that measured it.
Points are the board's 50·log10(K). Reruns of one commit spread about 3%, with outliers of up to
2x in dispatches where other tracks' unchanged code was slow too; our run log marks those
DISTURBED or SLOW BOX, and so does this table. Steps were first timed on a 16-vCPU AMD test server
(CPUs 0-11, interleaved runs, medians). Its kernel-side costs (page faults, cross-core atomics)
are much higher than the box's, so only the board decided what stayed.

A dash means the value was not written in our run log. Rows without a run ID had no board run of
their own, or their run is not in the log; their numbers are left out rather than guessed.

| Step | Commit | Change | Run ID | T (s) | M (MB) | S | Points |
|---|---|---|---|---|---|---|---|
| 0 | | The reference, unchanged (the board's calibration figures, not a run of ours) | | 14.44 | 367 | 108 | 0 |
| 1 | cf9c345 | mmap; quote-parity parallel split; per-thread name interners, serial merge | run-0a002c9fed81 | 0.444796 | 145.12 | 441 | 37.24 |
| 2 | c67f1ca | One lock-free name table shared by all threads; a `fetch_or` bit per (name, country) | run-ce648adbb50f | 0.124558 | 13.3 | 460 | 61.30 |
| 3 | 9ac00a6 | Transparent huge pages off: M varied 13.3–24.3 MB between runs of the same code, now stable | run-03a9af09852c | 0.129813 | 12.37 | 466 | 61.13 |
| 4 | 3c69a45 | Records in batches of 16 with prefetched table entries (board run not in our log) | | | | | |
| 5 | 42acaa7 | Profile-guided build | run-e999df62b7ec | 0.0979 | 13.55 | 528 | 63.39 |
| 6 | d893204 | SIMD bitmaps (quote, comma/LF, carry-less multiply for "inside quotes"); guessed cuts, checked; (hash, country) pairs deduplicated per hash partition between rounds, no atomics (board run not in our log) | | | | | |
| 7 | acdbc3c | `vpmovmskb` kept opaque to LLVM (see below) | run-e351b1a1c034 | 0.0553 | 16.76 | 691 | 67.78 |
| 8 | fcd0641 | regions.csv parsed in two parallel passes; groups numbered in output order | run-0be106cb3cd9 | 0.0494 | 19.82 | 777 | 67.90 |
| 9 | 6e3aed6 | Every pair buffer gets its capacity (`vec![v; n]` had cloned them empty) | run-1cd5ca162a83 | 0.0478 | 16.33 | 780 | 69.09 |
| 10 | 2246001 | Spinning barrier between rounds (server −1 ms; no board run of its own) | | | | | |
| 11 | a3acbf3 | Partition tables at most half full, with a dense id per name (server −0.5 ms; no logged board run, see step 13) | | | | | |
| 12 | 470e7e3, cb90a23 | Fewer lines: a wrong guessed cut reruns the file as one run (no second splitter); `format!` output; the mapping parsed directly again (pread gave nothing on the box) | run-2b4b2c97a9c1 | 0.0461 | 15.66 | 722 | 69.92 |
| 13 | 152db08 | The barrier's spin is bounded, then waiters sleep on a condition variable | | | | | |
| 14 | 0030dfe | Exit as soon as the output is written; each run's pages dropped by the thread that parsed it | run-8b2863810b1c | 0.04262 | 15.56 | 744 | 70.70 |
| 14b | a4e4404 | The whole amount converted by one SWAR step | run-bf0222333138 | 0.04093 | 15.47 | 755 | 71.12 |
| 15 | eb2cf64 | Per-record fast path (one 64-byte bitmap window, one 32-byte tail compare, one AVX2 number pass); batched dedupe; mallopt top pad; regions passes in the parse threads | run-db85f475f012 | 0.02677 | 15.99 | 914 | 74.96 |
| 15b | 894c8a7 | Branch-free CRLF, a 4-slot SIMD dedupe probe, byte group ids (branch misses 5.6M → 1.0M on one thread) | run-2c857d6ef1bd | 0.02634 | 15.5 | 940 | 75.18 |
| 16 | 007cea6 | Fewer lines: a byte-by-byte spec 2.1 parser replaces the SIMD general scanner; regions.csv in one pass, groups numbered as first seen and sorted at output; worker 0 is the main thread and exits right after writing; std locks instead of `UnsafeCell` | run-20006a569215, run-5c4c0328e6cb | 0.02732, 0.02706 | 16.17, — | 753 | 75.32, 75.41 |
| 17 | 48290f6 | One-pass digit loop for regions.csv; name and note bytes packed in one column; branchy min/max; fast-path bounds check out of the loop | run-bf1770e98008 | 0.025955 | 16.74 | 740 | 75.78 |
| 18 | 2bf221c | Each thread owns 4 name partitions (threads × 4) in every round: no locks, tables stay in its cache (its first run, run-66dc03aa76e1, 0.0507 s, was an outlier) | run-b9a64cd5e4d6 | 0.025757 | 15.46 | 733 | 76.24 |
| 19 | 7a7c57a | Non-temporal prefetch of the record stream 1 KB ahead, so the scan takes less of L2 and more of the 1 MB region table stays there (a box-only effect: no change on the AMD test server) | run-9389bfd9b77d | 0.025293 | 15.11 | 737 | 76.52 |
| 20 | eba58c8 | Same engine in fewer lines: no NamesFull retry (tables sized from the generator's 100,000-name pool, 8 standard deviations of room), errors as strings, `aggregate` folded into `run`, a one-counter barrier, regions chunks cut by `run_start`, `parse_run` kept out of line (its first run, run-7f86618c8878, 0.0613 s, was an outlier) | run-6cd61471239e | 0.025861 | 16.36 | 590 | 76.66 |
| 21 | 66d51c8 | Runs claimed from a per-round counter (4 per thread per round); a partition's dedupe taken by any thread once its owner has not started it; pages released after the barrier; the last thread to finish writes the output | run-cbeafca6a928 | 0.026301 | 14.62 | 610 | 76.86 |
| 22 | 6b93c97 | No rounds and no barriers: runs claimed one at a time, pair buffers handed to the owner thread's inbox after each run, tables take their inboxes between runs | run-ad43390d62d8 | 0.026124 | 13.87 | 615 | 77.13 |
| 23 | 68e5b56 | Claims go round the file's stripes, so that runs parsed at the same time do not share page tables | run-2a9b09524d9d | 0.025499 | 13.62 | 616 | 77.47 |
| 24 | dd55a82 | Fewer lines (mmap.rs folded in, regions as locals, compact slow path, seen rows grown per name); a filer takes over an inbox that grows past 16 × threads buffers | run-a80b6d932516 | 0.02631 | 13.54 | 541 | 77.58 |
| 24b | 0f06b1b | Each run parsed as two halves at once, so two records' region-table loads are in flight together | run-0dcb9601e59f | 0.025101 | 13.84 | 561 | 77.88 |
| 25 | e99accf (+ fb34663) | Trust spec 2.2 (what the generator writes): the 2.1 parser, the one-run second pass and the per-record validation removed; a 2.2 walker for long fields and the file's end; panics as tripwires. Same push: PGO training with atomic profile counters (Dockerfile) | run-d1003468b70d | 0.022853 | 13.28 | 445 | 79.83 |
| 26 | 7a72ab7 | Fewer lines: one field walker for all five fields, `masks` returns the inside-quotes mask, the own inbox taken in the filing loop. Equal on the server, 10% slower on the box: reverted to step 25 (0cc7815). The first run was in a DISTURBED dispatch | run-5b165ed65fa8, run-cb0af101a699 | 0.025257, 0.02524 | — | 407 | 78.93, — |
| 27 | 80fbcfd | Step 26 without its two suspects: the regions fill keeps its digit loop, and a thread takes its own inbox after the block release, as in step 25 | run-0495510fc87f, run-16a27c8c7af6 | 0.02291, 0.022831 | 13.35, — | 418 | 79.98, — |
| 28 | c0b7fbd | Branch-free AES hash of the name: 32-byte blocks masked to the field by a sliding `KEEP` window, three AES rounds, the length mixed in (run in a slow dispatch) | run-58f46f513414 | 0.022525 | 13.19 | 420 | 80.20 |
| 29 | bb339db | The name's row in bits 8..24 of its table key: the separate id array is gone (no board run of its own: measured with step 30) | | | | 422 | |
| 30 | 84c8677 | The amount's minus sign from the 32-byte tail compare already loaded, not from a byte load (second run of the same C code from a later push) | run-2bd88ce6fc49, run-43d3b02bb832 | 0.021739, 0.021609 | 12.68, 12.74 | 422 | 80.74, 80.79 |
| 31 | bddc535 | Memory: 64 runs per thread in release blocks of 8; one shared pool of empty pair buffers; buffers sized mean + 2 sd + 4, a grown one dropped once emptied; totals summed as each thread finishes (a third run, run-d210144850cf, 0.0266 s, was noisy) | run-649a95479a7d, run-0d8cb30b558e | 0.021915, 0.021661 | 10.63, 10.92 | 430 | 81.36, 81.37 |
| 32 | b25326b | Dockerfile only: hot loops aligned to 64 bytes in the profile-use build (`-align-loops=64`) | run-1ef2cedeac0a, run-4b1180e734e2 | 0.02147, 0.021807 | 10.59, 11.6 | 430 | 81.60, 81.04 |
| 33 | 5d6d235 | 32-bit name-table keys in buckets of 8 slots; 15 check bits of the hash in the name's row (word 3, bits 48..62, the bits of groups that never exist); more than 240 countries aborts. Both runs were in dispatches where unchanged Track A code was also slower than its best (box slower in the afternoon); memory 10.56 and 10.78 MB against 10.59-11.6 MB for step 32 | run-852c8d9d0f10, run-fde147f55fbc | 0.022314, 0.022147 | 10.56, 10.78 | 436 | 81.15, 81.14 |
| 34 | 3e7d9a4 | Fewer lines in cold code (merged `use` lines and two `let` pairs; empty groups skipped in the output loop header); the hot functions compile to the same instructions as step 33's. Run in a SLOW BOX dispatch (unchanged Track A at 0.0233 s against its best 0.0189 s) | run-37ca5d6651ae | 0.024684 | 10.99 | 430 | 79.93 |

Two more runs of step 32's code were in a DISTURBED and a SLOW BOX dispatch: run-87e294d71310
(0.022429 s) and run-dc9ee4598ca3 (0.021796 s, 11.28 MB).

The current code is step 33. Its single board run (81.15 points) is below step 32's best
(81.60), in a dispatch where B's unchanged code was slow as well; if the next samples do not
improve on step 32, it goes back.

## What each step taught

- **Step 2.** Merging per-thread name sets was most of the time. A shared table where the thread
  whose `fetch_or` flips a (name, country) bit counts that pair needs no merge at all: 0.444796 →
  0.124558 s.
- **Step 3.** The same code had measured 13.3 and 24.3 MB in two runs. With transparent huge pages,
  one touched byte of a sparse table can fault in 2 MB, depending on the host's fragmentation.
  With THP off the peak is stable.
- **Step 6.** Its board run is not in our log. It made memory and lines worse: the partitioned
  dedupe holds the pair buffers (about 4 MB) and the tables.
- **Step 7** is the largest single gain: 60 → 50.6 ms on the test server, and 0.0979 s (step 5) →
  0.0553 s on the box with step 6. Written naturally, `movemask(lo) | movemask(hi) << 32` becomes,
  inside LLVM, a 64 × i1 vector; on CPUs without AVX-512 (the box and the test server) it was then
  rebuilt one bit at a time with `bextr`/`vpinsrb`: a quarter of the run time. The development
  laptop has AVX-512 and compiled it well, which hid the problem until the server's disassembly
  was read. An inline-asm `vpmovmskb` cannot be rewritten.
- **Step 8.** The serial lookup-table parse was 4.4 ms, a tenth of the run at that point.
- **Step 9** was a plain bug, found by reading: cloning a `Vec` does not keep its capacity, so 63 of
  64 buffers grew by doubling. M 19.82 → 16.33 MB.
- **Step 13** followed a run of step 11's unchanged code (run-0b909063b14b, a docs-only push) that
  took 0.0778 s. A thread that loses its core to another process can be starved by eleven spinning
  siblings; with a bounded spin that cannot happen.
- **Step 14 came from measuring the box itself**: for one run the program exited right after a
  phase with the elapsed time as its exit code (0.25 ms units; the board shows a failed run's exit
  code). On the box, regions and cuts took 2.25 ms, the parse rounds 36.75 ms (only about 4.5 of
  them deduplication), and about 6 ms went to the kernel unmapping the 202 MB input at exit.
  Exiting right after the write, with the input dropped per run: 0.04262 s.
- **Step 15.** Server, PGO builds as the Dockerfile makes them, 12 threads: 38.3 → 24.5 ms. Single
  thread on full-seed1: 1.78G → 0.98G instructions, 590M → 372M cycles, about 506 → 282
  instructions per record (45M of the total is fixed cost: regions, startup). Box: 0.04093 →
  0.02677 s, +3.8 points.
  - The fast path takes the common record (`id,"name","note",region,amount` with quoted or empty
    text) from the 64 bytes at its start (128 when the note runs long). Any other shape, or any
    failed check, went to the old scanner, `#[cold]` and `#[inline(never)]`: inlined, it took the
    hot loop's registers (−18 instructions per record).
  - Id, region and amount are converted in one AVX2 pass. pshufb drops the dot, a byte table
    masks each field's digits, and `maddubs`/`madd`/`packus`/`madd` do the conversion.
  - Branches must not depend on CRLF vs LF, which is a coin toss. One `||` on it cost +1.7M branch
    misses and +15 cycles per record.
  - The dedupe takes a whole slice of pairs at a time, with the partition's fields as locals. Its
    probe compares a bucket of 4 slots at once, which halved the branch misses (2.0M → 1.0M).
  - Worker threads got their own malloc heaps, which grew by mprotect, 800 calls per run. Each one
    stalls every other thread's page faults. `mallopt(M_TOP_PAD, 32 MB)` brought it to 68 calls:
    −14 ms of system time and −2.8 ms wall on the server.
- **Steps 16–18** (round 2). Times on the test server, 12 threads, PGO + static builds, medians of
  21–31 interleaved runs (they include about 2.4 ms of fork/exec):
  - 894c8a7 24.2 ms → step 16 23.9 → step 17 23.7 → step 18 22.0–22.9. Memory 15.0 MB → 15.5 MB at
    12 threads (step 18 is 1 MB below step 17 there).
  - Two regressions found on the way, both 1 ms at 12 threads and invisible single-threaded: the
    per-thread pair-buffer headers sitting inline in one shared `Vec` (false sharing on their
    lengths), and `parse_run` taking `&Regions`. Regions held a `Mutex` inline, so the reference is
    not read-only to LLVM, and every record reloaded the table's address with two loads in front of
    the lookup. `parse_run` takes the `&[AtomicU8]` slice.
  - Step 18 had been rejected before (64 partitions over 12 threads, 6 vs 5 each, with locks). With
    exactly 4 partitions per thread the dedupe is balanced and a thread's tables never leave its
    core.
  - The single largest stall is the region-table lookup: 25–30% of single-thread cycles sit on the
    load. It is cache footprint, not latency: with the id masked to a smaller table (wrong output,
    timing only) 16 KB saves 2.3 ms, 256 KB 1.6 ms, 512 KB 0.7 ms.
- **Step 20** (round 3) kept the speed: server 22.35 ms vs 22.24 for 7a7c57a. Two things nearly
  lost it: folding `aggregate` into `run` put `parse_run`, inlined, inside a bigger closure whose
  register allocation cost 2–4% (fixed by `#[inline(never)]`), and a loop bound that recomputed
  the chunk end for every regions.csv line (+10M instructions).
- **Steps 21–24** (round 4): one slow CPU. Four board runs of unchanged code took about twice the
  normal time: run-0b909063b14b 0.0778 s, run-a960875d2df3 0.0467 s, run-66dc03aa76e1 0.0507 s,
  run-7f86618c8878 0.0613 s. The test server shows the same 2x when a busy loop is pinned to one
  of the 12 CPUs: 22.4 ms becomes 42 ms. The scheduler's slice there is 2.8 ms; the thread that
  shares its CPU with the loop is off its core for whole slices, and every one of the 17 barriers
  waited for it.

  | Version | Unloaded (ms) | One CPU loaded (ms) | Memory peak (MB) |
  |---|---|---|---|
  | step 20 (8 rounds, fixed runs and partitions) | 22.3 | 42.2 | 15.5–16.3 |
  | d1: runs claimed within a round (4 per thread), partitions stolen | 24.3 | 33.2 | |
  | step 21 (d5t): d1 with one page release per thread share, after the barrier | 22.4 | 32.8 | 15.3–16.1 |
  | e1: a pool of pair sets and a publish log, no barriers | 22.7 | 30.8 | |
  | step 22 (e8): inboxes per owner thread, buffers handed over | 22.7 | 25.2 | 14.2–14.6 |
  | step 23 (g1): claims round the stripes | 22.3 | 25–26.8 | 14.0–14.6 |
  | step 24 (h6) | 22.4 | 25.8 | 14.3–15.7 (loaded) |

  - Claiming runs dynamically within a round helped only partly: a thread that is off its core
    while it holds a run still holds the barrier. The fix had to remove the barriers.
  - Small runs cost +2 ms by themselves until the page release went back to one call per 2 MB:
    every `madvise(MADV_DONTNEED)` is a round of TLB-shootdown interrupts to all 12 CPUs.
  - Handing buffers over instead of copying pairs into the inbox saved 1.2 ms.
  - Step 23 came from the profile: `native_queued_spin_lock_slowpath` was 2% of the samples. Four
    threads parsed adjacent 0.53 MB runs inside the same 2 MB page-table page, and their faults and
    releases contended on its lock. Spread over the stripes: −0.5 ms on the server, 0.025499 s on
    the box.
- **Step 24b.** Two records per loop pass keep two region-table loads in flight. Server −0.38 ms;
  single-threaded, the region-load stall went from about 35% of `parse_run`'s cycles to 13%. Box:
  0.02631 → 0.025101 s, the best C run then. Three cursors were no faster.
- **Step 25** (round 5): trusting spec 2.2. Until step 24, every record was checked against 2.1,
  and a wrong guessed cut sent the whole file through a second pass. Neither can happen on
  generated data. The byte-by-byte 2.1 parser, the `Result` plumbing, the second pass and the fast
  path's checks were removed; a 2.2 walker (about 20 lines) and tripwires replaced them. Two
  `unsafe` spots were sound only for 2.2 input, and the RULES fixture repeated 300 times segfaulted
  on them; both were fixed (a field counts as quoted only from 2 bytes up; the walker hashes a
  padded copy). Server: 21.84 → 20.27 ms (−7%), single-thread instructions 1042M → 854M. Box:
  0.025101 → 0.022853 s and 561 → 445 lines, +1.95 points.
- **Step 26** cut 38 more lines and looked free on the server (20.40 → 20.23 ms, 31 runs). On the
  box it ran 0.025257 and 0.02524 s: 10% slower. It was reverted. A bisection on the server found
  all of its +13M single-thread instructions in the regions fill, and nothing at 12 threads.
- **Step 27** kept every other step-26 cut and left out the two suspects. Box: 0.02291 and
  0.022831 s, the same as step 25, with 27 fewer lines (+0.15 points). Together the two cost about
  10% on the box while the server showed nothing at 12 threads.
- **Steps 28–30** (round 6): stalls in the single-thread loop. Server, PGO docker builds, one
  session:

  | Version | Instructions:u (1 thread) | Cycles (1 thread) | 12 threads, median (ms) |
  |---|---|---|---|
  | step 27 (80fbcfd) | 849.5M | ~331M | 20.1–20.4 |
  | step 28 (c0b7fbd), AES hash | 834.4M | ~312M | 19.71 |
  | step 29 (bb339db), row in the key | 828.0M | ~303M | 19.66 |
  | step 30 (84c8677), sign from the tail | 821.4M | ~301.5M | 19.29 |

  - **Step 28.** The earlier hash looped over 8-byte words while more than 16 bytes were left, and
    that branch mispredicted on the 11% of names of 17–24 bytes. The new hash takes 32 bytes at a
    time, masked, through three AES rounds. A multiply-mix version of the branch-free hash cost
    +30M instructions and was rejected. Box: 0.02291 → 0.022525 s in a slow dispatch (the same
    dispatch ran A and B about 6–10% slow), so the gain is probably understated.
  - **Step 29.** After a probe hit, the row of group bits was found through a separate `u16` id per
    slot: a second load that depended on the probe. The row moved into the key. The price was in
    the hash bits that tell names apart: 50 at 12 threads instead of 56 (4.4e-6 per dataset
    instead of 7e-8).
  - **Step 30.** The minus sign was a byte load whose address waited on the comma's position. It is
    now a bit of a third compare of the tail register already loaded. Box, steps 29 and 30
    together: 0.022525 → 0.021739 s, M 13.19 → 12.68 MB, +0.54 points.
  - These cuts removed work from the single-thread critical path (a branch miss, a dependent load),
    and they moved both machines the same way: the server −5%, the box −5.1%.
- **Step 31** (round 7): memory. Where it was before the step, from smaps at the end of a 12-thread
  run on the server:

  | What | MB |
  |---|---|
  | Region table (a byte per region id) | 0.98 |
  | Name tables' keys (48 tables of 4096 8-byte slots) | 1.57 |
  | Seen rows (100,000 names × 32 B: 4 words of group bits, enough for 240 countries) | 3.26 |
  | Pair buffers | ~2.9 |
  | Outside the program's control: ~1 MB charged before it runs, ~0.7 MB kernel page tables and stacks, 1-2 MB per-CPU charge batching | ~3.3 |

  The pair buffers hold pairs only while they wait for their owner, about one run. 64 runs per
  thread instead of 32 halve a run (−0.9 to −1.1 MB, +0.2 ms on the server); release blocks of 8
  keep the `madvise` calls at about 2 MB each. One shared pool of empty buffers stops buffers
  sitting idle in one owner's pool while a filer makes new ones for another. Buffers are sized at
  a run's mean share plus 2 standard deviations plus 4 (113 pairs), and one that grew is dropped
  once emptied. Totals are summed as each thread finishes. Server: memory.peak 12.91 → 11.39 MB,
  time 19.9 → 20.14 ms (+1%), S 422 → 430. Box: 12.68 → 10.63 and 10.92 MB, +0.62 points.
- **Step 32.** Loop alignment is a Dockerfile flag, meant to make the hot loops' layout depend
  less on unrelated code. Box: run-1ef2cedeac0a 0.02147 s, 10.59 MB, the best C run (81.60), and
  run-4b1180e734e2 0.021807 s in a normal dispatch. Against step 31's 0.021661–0.021915 s, the
  speed is within the spread; it was kept because the best run came with it and it costs no lines.
- **Step 33.** The keys of step 31 were 64-bit, 1.57 MB over 48 tables. 32-bit keys in 8-slot
  buckets halve that, and one AVX2 compare still covers a whole bucket. A 32-bit key cannot hold
  as many hash bits, so 15 of them moved into the name's row, into the bits of groups 240..254,
  which do not exist. Server, docker `memory.peak`, cpuset 0-11, 21 interleaved runs: median 11.41
  → 10.85 MB (−0.56 MB, −4.9%), max −0.65 to −0.81 MB (the board's M is the max of five peaks).
  Time +0.12 ms (+0.6%) over 25 and 31 interleaved runs; single-thread instructions 818.2M →
  857.2M, cycles 306.5M → 305.3M, branch misses 0.555M → 0.496M. S 430 → 436. The identity bits
  went from 50 to 49.6 at 12 threads (4.4e-6 → 5.9e-6 per dataset) and from 54 to 46 on one thread
  (2.8e-7 → 7.1e-5). Box, one run in a slow dispatch: 0.022314 s, 10.56 MB, 81.15 points.

## Box probes

The board reports only T, M and S of a correct run, and the exit code of a failed one. The probes
behind step 14 (b881f34, 60e1955, 43b3419, b56f58d, each reverted by the next commit) exited with
a phase's elapsed time as their status, so the board lists them as "✗ correctness". They were
measurements, not failures of the solution. Their run IDs are not in our log.

## Experiments that did not make it (measured, reverted)

| Experiment | Where measured | Result |
|---|---|---|
| 32-bit keys with a variable shift and a 64-bit check constant (first version of step 33) | server | +0.3 ms: registers spilled. The kept version shifts by a constant |
| Threads drop their tables before the last one exits (02a535c) | board run-60031508190b | 0.025472 s against 0.025101 for step 24b: no gain, reverted in 98e7cb6 |
| Region table on one 2 MB huge page, with the first name tables in its spare megabyte (a392274, bf3a2c0) | board run-bbf744abd1a4 | 0.024972 s, S 583, 77.81 points against 77.88 for step 24b (server −0.55 ms): reverted in 2515d02 |
| Step 26's regions fill and merged own-inbox take | board run-5b165ed65fa8, run-cb0af101a699 | 0.0252–0.0253 s against 0.022853: reverted, see steps 26–27 |
| Name tables at 3/4 load instead of 5/8 | server | −0.33 MB, +0.8% time and worse collision odds |
| `munmap` of a block's input pages instead of `madvise(MADV_DONTNEED)` | not built | The cut search and the walker can read past a block's end into its neighbour; after `munmap` that read would crash |
| Waiters sleep (20 µs) instead of yielding, so the kernel can move the slowed thread | server, one CPU loaded | 33.9 vs 33.1 ms |
| A pool of pair sets with a publish log (e1) | server | 22.7 / 30.8 ms (unloaded / loaded): one preempted partition holder stalls the pool |
| Pairs copied into the inbox | server | flush 1.2 ms per thread vs 0.19 handed over |
| Buffers returned to the thread that filed them | server | +0.3 ms |
| 16 / 24 runs per thread | server | −0.3 / ±0 ms, memory 16.4 / 14.7 MB |
| 48 runs per thread in release blocks of 4 | server | +0.35 ms: more TLB-shootdown rounds |
| One inbox lock per partition instead of per owner thread | server | same time |
| Non-PIE static binary (`-C relocation-model=static`) | server | one-record input 5.06 vs 5.12 ms, full run the same |
| Populate the tables before the threads start (8a155db) | board run-b4f2d95150d7 | T 0.106, M 15.1: worse on the box (20% faster on the server), reverted in 7da570b |
| `prefetchw` and batches of 32 (a132c70) | board run-d1cae4327c17 | T 0.0995 vs 0.0979: no gain, reverted in 7f37d7e |
| Read records.csv with pread through 128 KB buffers instead of the mapping (2341842) | board run-4381f7c8201d | T 0.0502: no change on the box (6 ms faster on the server); reverted in cb90a23 |
| Shared lock-free table again, on top of the SIMD parser (d1200be) | board run-edc1e75d5cee | T 0.0539, M 13.94, 68.27 points: reverted in 1896065. 71 ms vs 41 ms on the server, where cross-core atomics are far dearer |
| Batch of 16 with prefetched region-table entries | server | cycles +4% |
| Each thread always owning the same partitions (before step 18) | server | +1 ms (64 partitions do not divide over 12 threads) |
| Separator offsets written for 4 KB at a time, simdjson stage-1 style | server | instructions +15%, cycles +14% |
| Rounds 4 / 16 instead of 8 | server | 4: −1 ms but +4 MB; 16: +1.8 ms for −1.7 MB |
| Dense name ids from one shared hash table, so pairs are u32 and dedupe needs no probing | server | 12 threads 45 → 67 ms: early inserts contend |
| Fast path over a 128-byte window from the record start (u128 bitmaps) | server | 519 instructions per record; u128 shifts and spills |
| All tables and pair buffers in a few big zeroed allocations | server | 44 ms vs 26: twice the page faults and 171 ms of system time |
| Hash computed before the record's checks | fuzz | crashed: a rejected record can have a negative name length |
| Batches of 4 / 8 / 16 records parsed with prefetches, then counted | server | +80M instructions; 12 threads 24.5 / 25.1 / 25.8 ms vs 24.5 |
| Compact region table: 32-id blocks (`rank << 32 \| bits`, 250 KB) and 200 KB of groups | server | parse 16.2 ms vs 12.8: two dependent random loads cost more than one into 1 MB |
| `madvise(MADV_POPULATE_READ)` on each run before parsing it | server | 23.9 vs 23.6 ms |
| Threads started as a binary tree | server | no change: thread starts still serialize in the kernel, about 55 µs each |
| Lean kernel (one folded validity test, BOM from one byte, quote state from bits) | server | 1011M vs 985M instructions, +0.5 ms |
| cmov min/max instead of branches | server | +10M instructions, +0.4 ms |
| Branch-free name hash with multiply mixing instead of AES rounds | server | +30M instructions against the AES version |
| Dot-anchored kernel (every record parsed from its start and its dot, no chain between records) | server | 23.04–23.08 vs 20.38 ms, 1052–1072M vs 869M instructions; killed after two attempts |

## Lessons

- **Only the board decides.** Step 26, the populated tables and the pread input were neutral or
  faster on the AMD test server, and slower or unchanged on the box. Kernel-side costs (page faults, TLB
  shootdowns, cross-core atomics) differ most between the two machines.
- **Cuts on the single-thread critical path transfer.** Steps 15, 24b and 28–30 removed branch
  misses and dependent loads, and both machines moved the same way.
- **Design for the slow CPU.** Four 2x runs of unchanged code (steps 21–24) came from one CPU
  shared with another process. Removing the barriers made the program degrade by about 15% in that
  case (22.4 → 25.8 ms on the server) instead of 2x.
- **Memory is mostly what the data needs.** After step 33 the tables and rows are what 100,000
  names need; the rest followed one design choice, how long pairs wait.
- **Name every collision odd.** Each change to the name keys came with the recomputed odds (steps
  29 and 33), not a claim of exactness.

## How correctness was verified

Since step 25 (trusting spec 2.2), on the exact docker-built binary. For step 33 (cmem2-1) the
set was:
- **Seed hashes.** Full seed 1, d8088b1cd42b…, at 1, 3, 7, 12 and 16 CPUs (`taskset`), and three
  repeated runs at 3 and 12 CPUs; sample seed 1, 9035fed36823….
- **Samples.** Sample seeds 2–4 byte-identical to the reference output at 1 and 12 threads.
- **Holdout.** The holdout-like seed 99 identical to the reference at 1, 5 and 12 threads.
- **Thread sweep and fresh seeds.** Earlier gate seeds (tiny 11–14, sample 21–23) and brand-new
  seeds (tiny 61–63, sample 64–66, made with the generator and checked with `reference/c_join.py`)
  at 1, 3, 12 and 16 threads, all identical. Earlier checkpoints used fresh seeds 57, 58 and 71–73.
- **Spec fixtures.** The `test_c_join.py` WORKED fixture, as written and with its records repeated
  300 times so that the fast path sees them, gives the expected output. The RULES fixtures abort
  with a message (SIGABRT), never a segfault.
- **Malformed input.** 32 mutants of sample seed 64 (truncations, stray quotes, deleted bytes,
  LF → x, byte flips, an open quote, no terminator, empty, header only, bad regions, more than 240
  countries) at 1 and 12 threads: exit 0 or SIGABRT, never a segfault, with the same exit codes as
  step 32's code.
- **Code check.** `objdump | grep -c vpinsrb` = 0.

Until step 24 (general 2.1 input) the checks also included forced wrong cuts, which had to fall
back to one run and still give the same output, a tiny name table, which had to exit nonzero, and
60 mutated records.csv and 21 mutated regions.csv files.
