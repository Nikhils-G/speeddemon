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

## Correctness checks run before every push

sha256 of the seed-1 output; byte comparison with the reference output for sample seeds 2–4; the
same output at 1, 2, 3, 5, 12 and 16 threads; forced wrong cuts, which must fall back to the
quote-parity split and still give the same output; a tiny name table, which must overflow and retry;
`objdump | grep -c vpinsrb` = 0 on the server build.
