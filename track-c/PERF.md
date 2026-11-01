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
| 20 | eba58c8 | Same engine in fewer lines: no NamesFull retry (tables sized from the generator's 100,000-name pool, 8 standard deviations of room), errors as strings, `aggregate` folded into `run`, a one-counter barrier, regions chunks cut by `run_start`, `parse_run` kept out of line | run-6cd61471239e | 0.0259 | 16.4 | 590 | 76.7 |
| 21 | 66d51c8 | Runs claimed from a per-round counter (4 per thread per round); a partition's dedupe taken by any thread once its owner has not started it; pages released after the barrier; the last thread to finish writes the output | run-cbeafca6a928 | 0.0263 | 14.6 | 610 | 76.9 |
| 22 | 6b93c97 | No rounds and no barriers: runs claimed one at a time, pair buffers handed to the owner thread's inbox after each run, tables take their inboxes between runs | run-ad43390d62d8 | 0.0261 | 13.9 | 615 | 77.1 |
| 23 | 68e5b56 | Claims go round the file's stripes, so that runs parsed at the same time do not share page tables | run-2a9b09524d9d | 0.0255 | 13.6 | 616 | 77.5 |
| 24 | | Fewer lines (mmap.rs folded in, regions as locals, compact slow path, seen rows grown per name); a filer takes over an inbox that grows past 16 x threads buffers | run-a80b6d932516 | 0.0263 | 13.5 | 541 | 77.6 |
| 25 | e99accf | Trust spec 2.2 (what the generator writes): the 2.1 parser, the one-run second pass and the per-record validation removed; a 2.2 walker for long fields and the file's end; panics as tripwires | run-d1003468b70d | 0.0229 | 13.3 | 445 | 79.8 |
| 26 | 7a72ab7 | Fewer lines: one field walker for all five fields, `masks` returns the inside-quotes mask, the own inbox taken in the filing loop. Equal on the server, 10% slower on the box: reverted to step 25 (0cc7815) | run-5b165ed65fa8, run-cb0af101a699 | 0.0253, 0.0252 | | 407 | |
| 27 | 80fbcfd | Step 26 without its two suspects: the regions fill keeps its digit loop, and a thread takes its own inbox after the block release, as in step 25 | run-0495510fc87f | 0.0229 | 13.35 | 418 | 80.0 |
| 28 | c0b7fbd | Branch-free AES hash of the name: 32-byte blocks masked to the field by a sliding `KEEP` window, three AES rounds, the length mixed in (run in a slow dispatch) | run-58f46f513414 | 0.0225 | 13.19 | 420 | 80.2 |
| 29 | bb339db | The name's row in bits 8..24 of its table key: the separate id array is gone (no board run of its own: measured with step 30) | | | | 422 | |
| 30 | 84c8677 | The amount's minus sign from the 32-byte tail compare already loaded, not from a byte load | run-2bd88ce6fc49 | 0.0217 | 12.68 | 422 | 80.7 |

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

### Steps 21–24 (round 4): one slow CPU

Four board runs of unchanged code took about twice the normal time:
- 0.0778 s;
- run-a960875d2df3, 0.0467 s;
- run-66dc03aa76e1, 0.0507 s;
- run-7f86618c8878, 0.0613 s.

The test server shows the same 2x when a busy loop is pinned to one of the 12 CPUs
(`taskset -c 3 sh -c 'while :; do :; done'`): 22.4 ms becomes 42 ms.
- The cause: the scheduler's slice there is 2.8 ms. The thread that shares its CPU with the loop
  is off its core for whole slices, and every one of the 17 barriers waited for it.
- A probe of the barrier waits: 0.84 ms per thread unloaded, about 10.5 ms with the loaded CPU.
- A loop on a hyperthread sibling outside our CPU set changed nothing.

Medians of 21–41 interleaved runs, PGO builds, 12 threads:

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
  while it holds a run still holds the barrier, however small the run. The fix had to remove the
  barriers.
- Small runs cost +2 ms by themselves (d1) until the page release went back to one call per
  2 MB: every `madvise(MADV_DONTNEED)` is a round of TLB-shootdown interrupts to all 12 CPUs.
  Since step 22 the thread that parses the last run of a block of 4 releases the block.
- e1 published each run's pair set in a log, and a set was free again once all 48 partitions
  had taken it. A thread that was preempted while it held a partition then stalled the pool for
  everyone (30.8 ms loaded). Inboxes per owner thread do not stall: an owner that falls behind
  only lets its own inbox grow.
- Handing buffers over instead of copying pairs into the inbox saved 1.2 ms (the copy wrote
  each pair a second time, into lines last held by another core). Returning each buffer to the
  thread that filed it was 0.3 ms slower.
- The barrier-free version needs less memory (step 22: −1.4 MB), because pairs wait for about one
  run instead of one round. But an owner that falls behind lets its inbox grow: h5 reached
  17.6–21.9 MB with one loaded CPU. Step 24 bounds it: a filer that finds more than 16 x threads
  buffers in an inbox takes that thread's tables over (14.3–15.7 MB loaded, same session).
- Step 23 came from the profile: `native_queued_spin_lock_slowpath` was 2% of the samples. Under
  claim order, four threads parsed adjacent 0.53 MB runs inside the same 2 MB page-table page, and
  their file faults and releases contended on its lock. It was gone with the claims spread over the
  stripes: −0.5 ms on the server and the best board run (0.0255).
- Step 24's single-thread instruction count drifts between builds (963M for step 23, 986–1016M
  for the compact versions) without any change to the hot loop. A bisection showed that putting
  step 23's exact `parse_run` and `fast_record` into the compact file, or step 24's slow path
  into step 23's file, moves it by the same amount: register allocation in `parse_run` changes (60
  against 68 stack references). The 12-thread time does not change (22.35 against 22.39 ms over 41
  runs). `Partition::add` is now out of line as well, so that the worker's code around it cannot
  change its loop.

### Steps 25–26 (round 5): trusting spec 2.2

Spec 2.2 says that the generator's guarantees hold for every seed and scale, the holdout included,
and that a solution may rely on them. Until step 24, every record was checked against 2.1, and a wrong
guessed cut sent the whole file through a second pass. Neither can happen on generated data.

- What was removed: the byte-by-byte 2.1 parser (`slow_record`, `text_field`, `digits`, about 60
  lines), the `Result` plumbing through every function, the second pass, and the fast path's
  checks (`digits_ok`, field widths, quote placement, terminator shape). A name or note is quoted
  if it is longer than its empty form; the terminator is CRLF if the byte after the amount is CR.
- What replaced them: a 2.2 walker (about 20 lines) for the records the windows cannot take (a
  note past the second 64-byte window, a name past the first, the last 256 bytes of the file), and
  tripwires. Each half-run must end exactly at its cut, or `assert_eq!` stops the program. Errors
  are panics, and the build aborts on panic, so each prints a message and exits nonzero.
- Two `unsafe` spots were sound only for 2.2 input, and the RULES fixture repeated 300 times
  segfaulted on them. A one-byte name (impossible under 2.2) made the hash's range negative, and the
  walker hashed in place near the end of the file. Now a field counts as quoted only from 2 bytes up,
  and the walker hashes a padded copy. Since then the RULES variants abort with a message and do not
  crash.
- Server, PGO docker builds, 21 interleaved runs at 12 threads: step 24's code 21.84 ms, step 25
  20.27 ms (-7%). Single-thread instructions went from 1042M to 854M (-18%), cycles from 383M to 334M.
  With a busy loop on one CPU: 27.96 against 25.82 ms. Memory is unchanged within noise (13.3–15.5 MB
  over sessions). On the board: 0.0254 → 0.0229 s and 561 → 445 lines, +1.95 points.
- Step 26 cut 38 more lines and looked free on the server: 20.40 → 20.23 ms (31 runs), loaded
  23.29 → 23.26 ms, memory 13.3–13.8 MB for both, single-thread instructions 869M against 854M.
  On the box it ran 0.0253 and 0.0252 s, against 0.0229 for step 25: 10% slower. It was reverted.
  Like the populated tables (8a155db), a change that is neutral on the AMD test server can cost on the
  box: only the board decides. A bisection on the test server with non-PGO builds, each of step 26's
  changes applied alone to step 25, found single-thread instructions unchanged (948.3M) for the
  folded quote mask, the field walker, the merged own-table take, the `transmute` of the number
  lanes, and the `then_some` and tuple forms. Cycles moved within noise. All of the +13M came from
  the regions fill (`position` and a digit fold instead of the digit loop). At 12 threads every
  variant measured the same (20.05–20.31 ms), so the server cannot say which change costs on the
  box. The prime suspects are the regions fill and the merged take, which changes when a thread
  takes its own inbox.
- Step 27 kept every other step-26 cut and left out exactly those two. Server: 20.22 ms, 854.0M
  single-thread instructions (step 25: 854.2M). On the box: 0.0229 s, the same as step 25, with 27
  fewer lines (+0.15 points, best C run). Step 26 ran 0.0253 with them in, so together they cost
  about 10% on the box, while the server showed nothing at 12 threads. Which of the two (or both)
  is not separated: that would take two more board runs.
- One codegen trap, found while bisecting a slower intermediate version with non-PGO builds:
  `fast_record(..).or_else(|| slow_record(..))` in the per-record closure cost 46M more
  instructions than the same logic as a `match`. The kernel keeps the `match`.
- The spec's RULES fixture (`tests/test_c_join.py`) is valid 2.1 input but not 2.2 input. It now
  exits nonzero with a message, by design. The WORKED example is 2.2 input and passes.

### Steps 28–30 (round 6): stalls in the single-thread loop

Three cuts aimed at what a single thread waits on in the hot loop: a mispredicted branch in the
name hash, a second dependent load after the name-table probe, and a byte load that waited on the
region comma's position. Server, PGO docker builds, all four versions in one session (so step 27
reads 849.5M instructions here, against 854.0M in round 5's session):

| Version | Instructions:u (1 thread) | Cycles (1 thread) | 12 threads, median (ms) |
|---|---|---|---|
| step 27 (80fbcfd) | 849.5M | ~331M | 20.1–20.4 |
| step 28 (c0b7fbd), AES hash | 834.4M | ~312M | 19.71 |
| step 29 (bb339db), row in the key | 828.0M | ~303M | 19.66 |
| step 30 (84c8677), sign from the tail | 821.4M | ~301.5M | 19.29 |

- **Step 28.** The earlier hash (wyhash-style, a 64x64→128 multiply per 8-byte word) looped over
  words while more than 16 bytes were left, and that branch mispredicted on the 11% of names of
  17–24 bytes. The new hash takes 32 bytes at a time: the block is loaded whole and ANDed with a
  sliding window into `KEEP` (32 bytes of ones, then 32 of zeros), so bytes past the name count as
  zeros, then goes through three AES rounds (two for its bytes 16..32, which are the first round's
  key), the length in the starting state. A name over 32 bytes takes another block in a loop. The
  walker hashes a copy of the name followed by 32 zero bytes: its loads stay in bounds, and since
  the mask zeroes every byte past the name, it gets the fast path's hash. A `compile_error!` stops
  a build without AES. A multiply-mix version of the branch-free hash cost +30M instructions and was
  rejected. Single thread: -15M instructions, cycles -6%; 12 threads -2.1%. On the board 0.02291 →
  0.022525 s and +0.22 points, in a slow dispatch (the same dispatch ran A in 0.0212 s against its
  best 0.0193, and B in 0.1271 against 0.1199), so the gain is probably understated.
- **Step 29.** After a probe hit, the row of group bits was found through a separate `u16` id per
  slot: a second load that depended on the probe. The row now sits in bits 8..24 of the key itself
  (a table holds at most 40,960 names, below 2^16), with `0xFF` in the low byte so that a key is
  never 0, and the probe compares only the hash bits above it. This also drops the id arrays (2
  bytes per slot, 384 KB over 48 tables of 4096 slots at 12 threads): memory 13.5 → 12.5–13.3 MB
  on the server. The price is in the hash bits that tell names apart: the 40 compared bits plus the
  bucket bits (hash bits 8 and up, 10 of them in a 4096-slot table), 50 bits at 12 threads instead
  of all 56. With the generator's 100,000 names the chance that two are confused is about 4e-6 per
  dataset at 12 threads (3e-7 on one thread), up from 7e-8. The partition does not add bits: it is
  picked from the hash's top 32 bits, which are compared anyway. No board run of its own: it went
  to the board with step 30.
- **Step 30.** The minus sign was a byte load at `note_end + 2 + region_len`, whose address waited
  on the comma's position. It is now bit `region_len + 1` of a third compare of the tail register
  already loaded for the comma and the dot. On the board, steps 29 and 30 together: 0.022525 →
  0.021739 s, M 13.19 → 12.68, +0.54 points: the best Track C run, 80.74.
- **Lesson.** These were single-thread stall and branch cuts, and they transferred to the box:
  the server's 12-thread median went from 20.1–20.4 to 19.29 ms (about -5%), and the box from
  0.02291 to 0.021739 s (-5.1%). Step 26's line cuts were neutral on the server and cost 10% on the
  box. Cuts that remove work from the single-thread critical path (a branch miss, a dependent load)
  moved both machines the same way.

**Step 2 of the plan, killed: a dot-anchored kernel.** Under 2.2 a record's only `.` is its
amount's, and the record ends 4 or 5 bytes after it. Stage A finds every dot of a 2 KB chunk from
64-byte bitmaps. Stage B then parses each record from its start and its dot: the id's comma from 32
bytes, the name from the 64-byte bitmaps after it, the region and amount commas as the top two
comma bits of the 32 bytes before the dot, and the note's quotes from 64 bytes. Nothing carries
from one record to the next but its start. A laptop prototype measured -28% parse time against the
step-24 kernel. Ported onto step 26 (measured before step 26 was reverted), it was exact on seeds 1–4, holdout 99 and the fresh seeds, but:

| Version | 12 threads (ms) | Instructions | Cycles (1 thread) |
|---|---|---|---|
| step 26 | 20.38 | 869M | 341M |
| dot kernel, blind writes in a counted loop | 23.04 | 1072M | 372M |
| dot kernel, 3 blind writes + cold loop | 23.08 | 1052M | 370M |

The kill criterion was -8% time and -10% cycles, within two attempts. It missed both, so it was
reverted. Most of the prototype's gain had been against step 24's 2.1 checks, which step 25
already removed. Stage A adds about 30 instructions per record, and stage B's three separate
windows cost about as much as the 64-byte window plus tail compare they replace.

## Experiments that did not make it (measured, reverted)

| Experiment | Where measured | Result |
|---|---|---|
| Waiters sleep (20 µs) instead of yielding, so the kernel can move the slowed thread | server, one CPU loaded | 33.9 vs 33.1 ms |
| A pool of pair sets with a publish log (e1) | server | 22.7 / 30.8 ms (unloaded / loaded): one preempted partition holder stalls the pool |
| Pairs copied into the inbox | server | flush 1.2 ms per thread vs 0.19 handed over |
| Buffers returned to the thread that filed them | server | +0.3 ms |
| 64 runs per thread (8-run release blocks) | server | +0.9 ms, memory 12.9–13.5 MB: the tables take smaller batches and are fetched again more often |
| 16 / 24 runs per thread | server | −0.3 / ±0 ms, memory 16.4 / 14.7 MB |
| One inbox lock per partition instead of per owner thread (48 lock calls per run) | server | same time |
| Tables taken from any thread while waiting for the last runs | server | same time; removed (−9 lines) |
| Regions entered before the thread's own tables are allocated | server | 23.02 vs 23.02 ms |
| Release blocks of 8 or 16 runs | server | within 0.1 ms, memory up slightly |
| Non-PIE static binary (`-C relocation-model=static`) | server | one-record input 5.06 vs 5.12 ms, full run the same |
| Builds without PGO | server, 12 threads | the same wall time; single thread 1–2% more cycles |
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
| Branch-free name hash with multiply mixing instead of AES rounds (round 6) | server | +30M instructions against the AES version: rejected |

## Correctness checks run before every push

Since step 25 (trusting spec 2.2), on the exact docker-built binary:
- the seed-1 sha256 at 1, 2, 3, 5, 7, 12 and 16 threads, and three repeated runs identical;
- byte comparison with the reference output for sample seeds 2–4 and the holdout-like seed 99, at
  1, 3 and 12 threads;
- 4 tiny and 3 sample seeds freshly generated and compared with the reference at 1, 3, 12 and 16
  threads, and brand-new seeds (57, 58, 71, 72, 73) before each checkpoint;
- the `test_c_join.py` WORKED fixture, as written and with its records repeated 300 times so that
  the fast path sees them. The RULES fixtures must exit nonzero, without a crash;
- `objdump | grep -c vpinsrb` = 0 on the server build.

Until step 24 (general 2.1 input):

Round 4 ran all of these on the exact docker-built binary, plus loaded-CPU runs, and strace of the
RULES fixture with its records repeated 300 times. The thread spawns are counted, to check that the
one-run fallback really runs (22 clones = two sets of 11).

sha256 of the seed-1 output; byte comparison with the reference output for sample seeds 2–4 and the
holdout-like seed 99; the same output at 1, 2, 3, 5, 12 and 16 threads; 6 tiny and 6 sample seeds
freshly generated and compared with the reference at 1, 3 and all threads; the `test_c_join.py`
fixtures, as written and with their records repeated 300 times so the fast path sees them; forced
wrong cuts, which must fall back to one run and still give the same output; a tiny name table,
which must exit non-zero; 60 mutated or truncated records.csv files and 21 mutated
regions.csv files, which must exit as before, without a crash; `objdump | grep -c vpinsrb` = 0 on the
server build.
