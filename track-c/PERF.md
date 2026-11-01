# Track C: performance log

**Result.** The reference runs 14.44 s at 367 MB and S 108. The submitted code (step 58, b8b733a,
S 487) runs 0.0171 to 0.0174 s on the board in its normal runs (run-605bf7359da0, 0.017134 s,
10.16 MB, 83.82 points; run-8f3d7064f29b, 0.01729 s; run-819427baa4d8, 0.017376 s), and step 57,
the same hot code, 0.0170 to 0.0173 s in its normal runs (run-c56b067ab64d, 0.016978 s, 10.74 MB, 83.71
points; run-9ac7e3c0222d, 0.017339 s). Three ideas carry it: a simdjson-style parse that never reads a field's bytes
twice (steps 6-15), cuts and a record path that trust spec 2.2 behind tripwires (step 25), and a
join and a distinct-name count with no shared state, whose one random load is prefetched a whole
record ahead (steps 46-49). The last nine steps traded 36 lines (0.25 points of S) for review
fixes, a parallel teardown and two fixes of the box's rare slow and memory-heavy runs. Every run ID
below is in the board's history for team SALAAR; "probe" marks a timing build that reports through
its exit code ("Box probes").

| What moved the score most | Step | Board, before → after |
|---|---|---|
| One name table shared by all threads instead of a serial merge | 2 | 0.444796 → 0.124558 s |
| `vpmovmskb` kept opaque to LLVM, with step 6's SIMD bitmaps | 7 | 0.0979 → 0.0553 s |
| A per-record fast path | 15 | 0.04093 → 0.02677 s, +3.8 points |
| Trusting spec 2.2 | 25 | 0.025101 → 0.022853 s and 561 → 445 lines, +1.95 points |
| Pairs that wait less: memory | 31 | 12.68 → 10.63 MB, +0.62 points |
| Instruction cuts in the record path | 37-42 | medians 0.02081 s (step 37) → 0.019321 s (step 39) |
| One table per thread, deferred counts, the next start from the LF | 46-49 | replica −6.9% in all; best run 0.017362 s |

Every step from the reference to the current version, with the live-board run that measured it.
Points are the board's 50·log10(K). Reruns of one commit spread about 3%, with outliers of up to
2x in dispatches where other tracks' unchanged code was slow too. Our run log marks those
DISTURBED or SLOW BOX, and so does this table. Step 46's binary ran 0.018414 to 0.020984 s in
nine runs.

Before pushing, steps were timed in three ways ("How we measured", near the end):
- every step: on a 16-vCPU AMD test server (CPUs 0-11, interleaved runs, medians), whose
  kernel-side costs (page faults, cross-core atomics) are much higher than the box's;
- with in-process probes on the box;
- from step 42 on (Sun 13:20 UTC): on a replica of the scoring box, with the same CPU and the
  board's own timer.

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
| 33 | 5d6d235 | 32-bit name-table keys in buckets of 8 slots; 15 check bits of the hash in the name's row (word 3, bits 48..62, the bits of groups that never exist); more than 240 countries aborts. Both runs were in dispatches our log marks as a slower box | run-852c8d9d0f10, run-fde147f55fbc | 0.022314, 0.022147 | 10.56, 10.78 | 436 | 81.15, 81.14 |
| 34 | 3e7d9a4 | Fewer lines in cold code (merged `use` lines and two `let` pairs; empty groups skipped in the output loop header); the hot functions compile to the same instructions as step 33's. The first run was in a SLOW BOX dispatch (unchanged Track A at 0.0233 s against its best 0.0189 s); the second, run-085364b6513f, in a normal one (nine board runs of this code in all: see below) | run-37ca5d6651ae, run-085364b6513f | 0.024684, 0.022277 | 10.99, 10.39 | 430 | 79.93, 81.28 |
| 35 | 2c8ba1c | Output lines written into one string (`write!` into a String, `money()` gone) | run-a9ee2ce7006d, run-b0a4d0918d2a, run-6a13aed936f1 | 0.021639, 0.022423, 0.021718 | 10.63, 10.44, 10.65 | 429 | 81.51, 81.20, 81.46 |
| 36 | 82cf0cf (board runs from 82cf0cf, 5a0612d, 045a6b5, 9ce324f) | Code-quality pass: comments, names and SAFETY arguments; named `Record`, `Field` and column consts; `fill`, `merge` and `drop_pages` as functions; the hot functions compile to the same instructions as step 35's in a build without the profile. The last run came in a dispatch where Track A ran its fastest time then (0.018636 s), so C's slow runs are not only a slow box. The M and points of the third and fourth runs are from `board.py` output: runs.log has only their T | run-5063c99104c2, run-3b36d6c3b4df, run-84ffefa565f7, run-f93690fc8898 | 0.02165, 0.02189, 0.021882, 0.023947 | 10.57, —, 10.77, 11.03 | 469 | 81.23, —, 81.04, 79.95 |
| 37 | 4503e60 (runs from later pushes) | Fewer instructions per record: unescaped lengths from the inside-quotes bits, digit masks from a table | run-69c7ee1ea546, run-77c8650252ea, run-cd90bd2f3b18 | 0.022621, 0.021133, 0.020547 | 10.34, 10.95, 10.43 | 486 | 80.74, 81.23, 81.74 |
| 38 | 8da6bc0 (probe, removed in eea4759) | In-process probe: step 37's hot code against c-02's | run-4e00d26e2d6f | probe | probe |  |  |
| 39 | ed1ac29 (later runs from 124a828 to 55ba5f9) | c-02: `numbers` without a table, group numbers stored directly, a one-block name hash; 18 fewer lines | run-8745d6dee8fb, run-459fe675191f, run-4fb03e52a61c, run-3f326ab6046e, run-9cfb0aeaf151, run-e73457094c3d, run-1b396e172d7e, run-0c667e027996, run-478c07a983fa | 0.021787, 0.019119, 0.019659, 0.020799, 0.019031, 0.019305, 0.019324, 0.019069, 0.019321 | 10.53, 10.68, 10.1, 10.72, 10.55, 10.64, 10.68, 10.72, 10.53 | 468 | 81.19, 82.54, 82.48, 81.62, 82.65, 82.46, 82.43, 82.56, 82.49 |
| 40 | 43fbf02 (probe, removed in 344128e) | In-process probe: step 39's hot code against c-04's | run-be32ebf496b3 | probe | probe |  |  |
| 41 | 09d3b3d (probe, removed in 55ba5f9) | In-process probe: step 39's hot code against c-06's | run-cc48b6883434 | probe | probe |  |  |
| 42 | eafffeb, 9bb1943 | c-07: c-04 and c-06, and the record loop bounded by `fast_end` (replica −0.90% against step 39) | run-b3e8acb52368, run-f610158d2d95 | 0.021546, 0.019026 | 10.12, 10.36 | 467 | 81.49, 82.74 |
| 43 | 9369bc4 (probe, removed in 9bb1943) | In-process probe: 4 name partitions per thread against 2 | run-bba152c3d52a | probe | probe |  |  |
| 44 | 422624f (probe, removed in 9bb1943) | In-process probe: the input released per 8 runs against once per thread | run-66612ee03263 | probe | probe |  |  |
| 45 | 22ac85a (probe, removed in 4d6a1ea) | In-process probe: 4 name partitions per thread against 1 | run-a22416223330 | probe | probe |  |  |
| 46 | 4d6a1ea | c-11: one name table per thread, the input released once per thread, a zero-state hash (replica −2.13%) | run-d350c1d86d5b, run-3d2a8eb59f23, run-eaba9c928599, run-26b559376633, run-fce97229fefc, run-0255d249afa8, run-5faf8568ab9f, run-a465337f1b29, run-3184759ea0d5 | 0.018743, 0.018506, 0.020824, 0.018414, 0.018866, 0.018932, 0.018846, 0.018459, 0.020984 | 10.13, 10.48, 10.6, 10.29, 10.36, 10.64, 10.31, 10.48, 10.29 | 458 | 83.06, 83.05, 81.72, 83.19, 82.89, 82.74, 82.93, 83.08, 81.77 |
| 47 | 58b4b91 (later runs from 19a626a, 813ac5d) | c-12: the region entry prefetched, each pair of records counted one pass later (replica −3.54%) | run-bc6928927710, run-b6bcc9f30510, run-3251e80ac9c8 | 0.018023, 0.018148, 0.017986 | 10.68, 11.72, 9.48 | 468 | 83.19, 82.71, 83.73 |
| 48 | 48ee977 (later runs from d29ece2, 594fcee) | c-13: each record counted after the next is parsed, with line cuts (replica −2.15%; DISTURBED third run) | run-36e450a5043d, run-9f1044acde45, run-630dbceeb255 | 0.01758, 0.017572, 0.020413 | 10.4, 10.45, 10.03 | 435 | 83.81, 83.79, 82.34 |
| 49 | 783e796 (later runs from e2d0381, a comment change, and ffef5f8, 44e0f8f, e466e5a, d45040c, f44e04e) | c-14: the next record's start from its LF; named helpers back (replica −1.36%; fourth run DISTURBED) | run-9ebe33aa6b74, run-46d6bf16c5bd, run-3162b4427a7c, run-b995ed5023b0, run-686a1a48cf08, run-5c1bc42d0b38, run-0f0e3f872458 | 0.018128, 0.017362, 0.017558, 0.021429, 0.017192, 0.01728, 0.017662 | 11.8, 9.32, 10.47, 10.51, 10.28, 10.38, 10.55 | 451 | 82.81, 84.30, 83.68, 81.50, 83.99, 83.89, 83.58 |
| 50 | 15dd452 (second run from 4968eb1) | c-15: review fixes in comments and named constants only; one thread's odds corrected to 42 bits (replica +0.11%: the same speed) | run-f3ad1090840c, run-93653aed6587 | 0.017533, 0.017646 | 10.65, 10.24 | 457 | 83.57, 83.67 |
| 51 | 2c3726c (later runs from bd025d6, c25a4fc, a106aad, 88af8ca, 398eb51) | c-16: while the last thread writes, the others drop their tables' pages and a share of the join table and of regions.csv; two review fixes (replica −0.45%; first run in a slow dispatch, fifth DISTURBED) | run-ed4aaf4a529a, run-011c441723f3, run-95f02168b6f7, run-793b8634e7f2, run-f668965f3eca, run-798d3b5284a7 | 0.020944, 0.017332, 0.017418, 0.017226, 0.021534, 0.017095 | 10.04, 9.94, 10.29, 9.89, 11.38, 10.63 | 464 | 81.85, 83.95, 83.75, 84.04, 81.01, 83.81 |
| 52 | 53173d3 (later runs from 42de64e, 86b9c7c, 807ce06) | c-17: the input's pages dropped in two parts, two thirds before the last takes and the last third by the other threads while thread 0, now always the writer, writes the output (replica about −0.5%: −0.07% to −0.68% over four builds; probe builds of the same split −0.52% to −1.02%; third run: C alone slow) | run-ae04f5d6b6e3, run-beae5769413b, run-2de41de3360e, run-f42a9af9241c | 0.017364, 0.017247, 0.020836, 0.017135 | 10.44, 10.59, 11.34, 10.25 | 467 | 83.70, 83.71, 81.36, 83.92 |
| 53 | 30c2691 (second run from 33ecba3) | c-18: the record parser's four early-exit guards through a named helper, `ensure` (replica +0.14%, 67 of 151 pairs: the same speed; first run: C alone slow) | run-481bdc9d6143, run-0ab5efa1c8b9 | 0.020766, 0.01721 | 10.51, 10.42 | 470 | 81.71, 83.78 |
| 54 | eb1258d (later runs from 3aa95c6, 5fda0c0, 96502b4, 475aae6) | c-19: a filer whose takeover finds the table locked yields its core, which ends rare memory spikes; `drop_pages` takes a raw pointer; comments brought up to date (replica −0.28% against step 53; first run DISTURBED, third in a slow dispatch) | run-ee5c6aeac392, run-de2dd8d747e2, run-f3dcdb9fca12, run-ac57ad222a30, run-adf9a0251048 | 0.020717, 0.017377, 0.02109, 0.017101, 0.019528 | 10.1, 10.32, 9.93, 10.24, 10.39 | 471 | 81.90, 83.71, 81.77, 83.92, 82.41 |
| 55 | 2f05cec (second run from 854810e) | c-20: a last record without a terminator, which 2.1 allows, read like any other, with the reference's output; the top comment says exactly what is answered and how (replica +0.33% and −0.10% in two builds: build noise; both runs: C alone slow) | run-d4f9240d849b, run-a968d9c22097 | 0.020511, 0.020647 | 10.06, 10.15 | 471 | 82.02, 81.91 |
| 56 | ffb2e77 | c-21: after the region wait, a thread that finds another of ours on its CPU moves to the lowest free CPU of its affinity mask (replica −0.39%, 115 of 201; runs over 19.5 ms 9 → 2 and 12 → 6 in two sets of 300 pairs) | run-2fae75090a37 | 0.017003 | 10.38 | 483 | 83.84 |
| 57 | aa1fe77 (later runs from 5216711, outside the queue, and 97b4897) | c-22: a failed CPU lookup, mask read or move leaves the thread where it is, with nothing asserted (replica +0.16%, 87 of 201: the same speed) | run-9ac7e3c0222d, run-fbcbe364dd0f, run-c56b067ab64d | 0.017339, 0.017057, 0.016978 | 10.31, 10.49, 10.74 | 483 | 83.66, 83.76, 83.71 |
| 58 | b8b733a (later runs from 3bd65f7, fabe12e, c36861d and 8a293ff) | c-23: the CPU move as a named function (`move_off_shared_cpu`), `mmap` errors that name their cause, `fill` indexing the codes by `(a * 26 + b) * 26 + c`, review comments; the hot code compiles as step 57's (replica −0.11%, 106 of 201; mem.sh, 20 runs, max 10.50 MB). **The submitted code** | run-819427baa4d8, run-42f5d2e69825, run-a9d75c6b2bc1, run-605bf7359da0, run-8f3d7064f29b | 0.017376, 0.020187, 0.018676, 0.017134, 0.01729 | 10.17, 10.92, 10.57, 10.16, 10.29 | 487 | 83.67, 81.73, 82.72, 83.82, 83.67 |

Two more runs of step 32's code were in a DISTURBED and a SLOW BOX dispatch: run-87e294d71310
(0.022429 s) and run-dc9ee4598ca3 (0.021796 s, 11.28 MB).

**Code-quality pass (82cf0cf, step 36), on the box:** run-5063c99104c2 0.02165 s, 10.57 MB, 81.23
points (runs.log 09:23Z); run-3b36d6c3b4df 0.02189 s (runs.log 09:28Z, a normal C run in a slow
dispatch for A and B); run-84ffefa565f7 0.021882 s, 10.77 MB, 81.04, and run-f93690fc8898
0.023947 s, 11.03 MB, 79.95 (`board.py`). Its hot code is step 35's, and three of the four runs
fall in step 35's range (0.021639 to 0.022423 s), so the pass cost no speed; it cost points
through S (429 → 469).

**The submitted code is step 58 (c-23, b8b733a), S 487**: step 57 with the review's code items. The
CPU move is a named function, `move_off_shared_cpu`, called after the region wait, whose comment
says that a moved thread stays on its new CPU and why that is safe enough; `mmap`'s failure message
names the OS error; `fill` indexes the country codes by `(a * 26 + b) * 26 + c`; and `parse_run`'s
comment keeps the why, with its measurements in this file (steps 47-48). Against step 57's build,
`add`, `walk_record`, `write_output`, `run` and `main` compile to the same instructions, and
`parse_run` to the same 920 but for the usual 4-line block swap between PGO builds. Replica −0.11%
(106 of 201 pairs); the gate passed, and fresh full seeds 101-105 gave the reference's output as
root and as uid 65534; mem.sh, 20 runs, max 10.50 MB (step 57: 10.43). Board: run-605bf7359da0,
0.017134 s, 10.16 MB, 83.82 points; run-8f3d7064f29b, 0.01729 s; run-819427baa4d8, 0.017376 s; and
0.018676 and 0.020187 s in two slower dispatches.

**Step 57 (c-22, aa1fe77), S 483**: step 55 with step 56's move off a shared
CPU, and step 57's handling of a failed CPU call (the thread stays where it is). Replica: step 56
−0.39% (115 of 201 pairs) and, in two sets of 300 interleaved pairs against step 55, medians
−0.27% and −0.23%, with runs over 19.5 ms 9 → 2 and 12 → 6; step 57 +0.16% (87 of 201), the same
speed. Board: run-c56b067ab64d, 0.016978 s, 10.74 MB, 83.71 points; run-fbcbe364dd0f, 0.017057
s, 10.49 MB, 83.76 points; run-9ac7e3c0222d, 0.017339 s; and step 56's run-2fae75090a37, 0.017003
s, all inside docker as uid 65534.

**Step 55 (c-20, 2f05cec), S 471**: step 54 with a last record without a
terminator read like any other. Only the cold record walker changed: `parse_run` and `add` compile
to the same instructions as step 54's. Replica +0.33% (82 of 201 pairs) and −0.10% (103 of 201) in
two builds of it, build-to-build noise. Its first run, 0.020511 s (run-d4f9240d849b, 10.06 MB,
82.02 points), came in a dispatch where C alone ran slow (A 0.017182 s, B 0.087799 s).

**Step 54 (c-19, eb1258d), S 471**: step 52 with step 53's named helper for the parser's early
exits, and step 54's fix of rare memory spikes and review fixes.
- *Board.* run-ac57ad222a30, 0.017101 s, 10.24 MB, 83.92 points; run-de2dd8d747e2, 0.017377 s;
  0.020717 s in a DISTURBED dispatch, where B's unchanged code ran 65% over its previous run; and
  0.02109 s in a dispatch where B's new code ran slow too (0.132044 s); and 0.019528 s. Step 53 ran 0.01721 s, and
  0.020766 s in a dispatch where C alone ran slow.
- *Replica.* Step 53 +0.14% against step 52 (67 of 151 pairs; two builds of one source differ by
  +0.09%), step 54 −0.28% against step 53 (117 of 201).
- *Memory.* 200 runs of each production build: a max of 12.00 MB for step 53, 10.82 MB for step
  54 ("What each step taught", step 54).

**Step 52 (c-17, 53173d3), S 467**: step 49's hot code (`parse_run` and `add` compile to the same
instructions), step 50's review fixes, and steps 51 and 52's teardown.
- *Board.* Step 52: run-f42a9af9241c, 0.017135 s, 10.25 MB, 83.92 points; run-beae5769413b,
  0.017247 s; run-ae04f5d6b6e3, 0.017364 s; and 0.020836 s (run-2de41de3360e) in a dispatch where
  C alone ran slow (A 0.016398 s, B 0.08596 s). Step 51:
  run-793b8634e7f2, 0.017226 s, 9.89 MB, 84.04 points; run-798d3b5284a7, 0.017095 s,
  the fastest Track C time (10.63 MB, 83.81 points); run-011c441723f3, 0.017332 s;
  run-95f02168b6f7, 0.017418 s; 0.020944 s in a dispatch where A's unchanged code also ran 17%
  over its previous run; and 0.021534 s in a DISTURBED one. Step 50 ran 0.017533 and 0.017646 s.
- *Replica.* Step 50 is the same speed as step 49 (+0.11%, 100 of 201 pairs; two builds of step
  49 itself read +0.09%). Step 51 is 0.45% faster than step 50 (109 of 201), and 0.56% and 0.60%
  in two more sets (119 and 114 of 201). Step 52 is about 0.5% faster than step 51: −0.68% (121
  of 201) in a first check, and −0.07% to −0.68% over four builds of the same source. Builds
  of one source differ by about ±0.3% here, because the profile-guided build trains on a threaded
  run.
- *Points.* Step 49's run-46d6bf16c5bd (0.017362 s, 9.32 MB, S 451) has the best points, 84.30.
  Steps 50 to 52 cost 16 lines, 0.11 points of S; steps 51 and 52's −1.0% or so is worth about
  0.11 points.
- *Memory.* Four alternating sets of 5 runs on the replica: a max of 10.13 to 10.42 MB for step
  51 against 10.28 to 11.17 MB for step 50, and 10.12 to 10.50 MB for step 52 against 10.17 to
  10.56 MB for step 51. Equal or lower: the peak is during the parse.

**Step 49 (c-14, 783e796, S 451).**
- *Board.* run-9ebe33aa6b74, 0.018128 s, 11.8 MB, 82.81 points; run-46d6bf16c5bd, 0.017362 s,
  9.32 MB, 84.30 points, the best Track C run; run-3162b4427a7c, 0.017558 s; 0.021429 s in a
  DISTURBED dispatch, where A's and B's code ran slow too; run-686a1a48cf08, 0.017192 s (10.28 MB,
  83.99 points: below the best run's points through M); and run-0f0e3f872458, 0.017662 s. Its two
  M readings, 11.8 and 9.32 MB, show how
  far one binary's peak moves from run to run.
- *Replica.* Step 49 is 1.36% faster than step 48 (147 of 201 pairs won), and with steps 47 and
  48 about 6.9% faster than step 46.
- *Memory.* The board's 11.8 MB is the highest reading of this design. On the replica the max of
  7 runs was 10.26 and 10.05 MB in two sets: one binary's M varies by about a megabyte from run
  to run (the kernel's per-CPU charge batches, "How we measured").
- *Lines.* The line cuts merged into it measured within the noise: cl-1 −0.36% against step 46
  (122 of 201 pairs; an A/A test of two builds of step 46 read −0.18%), and cl-2, which put named
  functions back, +0.25% on average over seven pairings of builds against step 48, with step 48's
  own A/A at −0.19%.

**Step 48 (c-13, 48ee977, S 435)**: run-36e450a5043d (0.01758 s, 10.4 MB, 83.81 points) and
run-9f1044acde45 (0.017572 s, 10.45 MB, 83.79), then 0.020413 s in the disturbed dispatch of 14:50
UTC. It was 2.15% faster than step 47 on the replica (183 of 201 pairs).

**Step 46 (c-11, 4d6a1ea, S 458)** ran nine times: 0.018414 to 0.020984 s, median 0.018846 s. Its
best is run-26b559376633 (0.018414 s, 10.29 MB, 83.19 points). It had two slow runs:
- 0.020824 s at 13:43 UTC, in a dispatch where Track A's a-09 was slow too (0.019543 s,
  against its fast runs of 0.0166-0.0170 s);
- 0.020984 s at 14:13 UTC, in the dispatch where Track B's b-stack-1 ran 0.103801 s against its
  replica time of 91 ms, while Track A ran fast (0.016307 s).

Against step 42, the replica measured it 0.405 ms (2.13%) faster, winning 26 of 31 pairs. Against
step 39, c-07 had measured 0.172 ms faster (25 of 31).

**Step 37's code** ran 23 times on the board, from 10:09 to 12:07 UTC: 0.020505 to 0.025598 s,
median 0.02081 s. Its best-scoring run is run-cd90bd2f3b18 (0.020547 s, 10.43 MB, 81.74 points).

**The probes of steps 38, 40 and 41 compare two versions of the hot code.** They can carry a
layout bias, as Track B measured for its probes (+0.7 ms with 4 KB-aligned functions). Step 41
read 1.3 ms per parse pass in c-06's favour. Yet the replica measured c-02 → c-07 (c-06's code
plus one more cut) end to end at only −0.17 ms. So for those steps the replica's numbers are the
ones the decisions rest on. The probes of steps 43 to 45 run the same code in both modes with
different constants, so no layout difference enters them.

Step 35's runs are run-a9ee2ce7006d (0.021639 s, 10.63 MB, 81.51
points), run-b0a4d0918d2a (0.022423 s, 81.20), run-6a13aed936f1 (0.021718 s, 81.46),
run-718519297500 (0.021659 s, 81.59) and run-5514f073467f (0.021643 s, 81.43), plus one at
0.024088 s in a slow dispatch whose run ID is not in our log. Step 34's code had nine board runs:
run-b52eb817e73d 0.022131, run-085364b6513f 0.022277, run-c22dfd35468d 0.022346, run-ea147e35f6ca
0.022517, run-bd2e07eba1e1 0.022515, run-92c59d19bd2f 0.022606, run-005966bd2a44 0.022879,
run-37ca5d6651ae 0.024684 (SLOW BOX) and run-4e8d1ce215a3 0.025734 s; median 0.022517 s. The best-scoring
C run is still step 32's run-1ef2cedeac0a (0.02147 s, S 430, 81.60 points).

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
  Tracks A and B are now under a board A/B test without the flag (fcc1dd3); C keeps it.
- **Step 33.** The keys of step 31 were 64-bit, 1.57 MB over 48 tables. 32-bit keys in 8-slot
  buckets halve that, and one AVX2 compare still covers a whole bucket. A 32-bit key cannot hold
  as many hash bits, so 15 of them moved into the name's row, into the bits of groups 240..254,
  which do not exist. Server, docker `memory.peak`, cpuset 0-11, 21 interleaved runs: median 11.41
  → 10.85 MB (−0.56 MB, −4.9%), max −0.65 to −0.81 MB (the board's M is the max of five peaks).
  Time +0.12 ms (+0.6%) over 25 and 31 interleaved runs; single-thread instructions 818.2M →
  857.2M, cycles 306.5M → 305.3M, branch misses 0.555M → 0.496M. S 430 → 436. The identity bits
  went from 50 to 49.6 at 12 threads (4.4e-6 → 5.9e-6 per dataset) and from 54 to 46 on one thread
  (2.8e-7 → 7.1e-5). Box: 81.15 and 81.14 points, both in dispatches our log marks as a slower box.
- **Step 34.** Cold code only, S 436 → 430; the hot functions compile to the same instructions.
  Its normal-box run, 0.022277 s and 10.39 MB, is the lowest C memory peak in our log, and 0.32
  points under step 32's best run.
- **Step 35.** The output was a String per line and per amount (`format!` and `money()`); one
  String written into with `write!` takes the output from 0.2 to 0.1 ms on the server, at the end,
  when no other thread has work left. S 430 → 429. Box: run-a9ee2ce7006d, 0.021639 s, 81.51 points.
- **Step 37: fewer instructions in the same design.** Unescaped lengths come from the inside-quotes
  bits, one popcount per field instead of a quote popcount and its arithmetic. Digit masks come from
  a 128-entry table, the BOM from one byte compare, and the name lookup tests free slots only after
  no key matched. Test server: 855.3M → 777.8M instructions on one thread (−9.1%, 257.6 → 234.3
  per record), 20.17 / 20.32 → 19.62 / 19.43 ms (−0.72 ms, 31 interleaved runs, two PGO builds
  each). On the box its runs settled at about 0.0206 s (median 0.02081 s over 23 runs, 10:09 to
  12:07 UTC), against 0.0216-0.0224 s for step 35, so the 9.1% instruction cut gave about 4% on the
  box.
- **Steps 38-39: instruction cuts, again.** Step 39 (c-02): `numbers` reverses each word's bytes in
  its shuffle, so byte k weighs 10^k, and a number is the bytes below the word's lowest non-digit
  byte, `(x − 1) & !x` on the non-digit mask, with no table. Group numbers are stored directly (0
  = `_UNMATCHED`, countries 1..=240). Names over 32 bytes go to the walker, so the fast path hashes
  one block. `i >= 8` is asserted once per run instead of per record. One thread: 777.8M →
  701.2M instructions (−9.8%), and 18 fewer lines. The probe of step 38 read 110: c-02's hot code was
  1.0 ms faster per parse pass than step 37's, inside one process. Step 39's nine board runs took
  0.019031 to 0.021787 s (median 0.019321 s), against step 37's median of 0.02081 s: about −7%,
  with 9.8% fewer instructions and 18 fewer lines. Its best run, run-9cfb0aeaf151 (0.019031 s,
  82.65 points), was the best Track C run then.
- **Steps 40-42: smaller cuts, and a lesson about the probe.** c-04 takes the amount's sign from
  one byte load (the byte after the region's comma) instead of a `-` bitmap, `name_quoted` from the
  inside-quotes mask, and in `add` the bucket's byte offset straight from the pair, with the check
  bits compared in place (12 threads: −4.2% instructions). c-06 boxes the walker's result (returned
  by value, its `Record` stayed in memory and the fast path stored every field of every record into
  it) and fixes the join table at 2^20 entries (listed ids are below 1,000,000, spec 2.3), so the
  clamp is to a constant and no last-line scan sizes the table. Step 42 (c-07) adds the record loop
  bounded by `fast_end` too, so `fast_record`'s own test of it is dropped (2 instructions per record,
  objdump): one thread 701.2M → 645.1M instructions (−8.0% from step 39). Replica against step 39:
  31 pairs, medians 19.129 → 18.940 ms, paired median −0.172 ms, 25 won. The probes of steps 40 and 41 read
  103 and 113: c-04's hot code 0.3 ms and c-06's 1.3 ms faster per parse pass than step 39's.
  Step 42 is c-06 plus one more cut. On the replica it was only 0.172 ms (0.90%) faster than step
  39, end to end, winning 25 of 31 pairs. Its board runs were 0.021546 and 0.019026 s. In-process
  probes of two code versions can carry a layout bias (Track B's identical copies read +0.7 ms
  even with 4 KB-aligned functions), and a parse pass is not the whole program. The replica is the
  number to believe.
- **Steps 43-45: one partition per thread.** Four name partitions per thread had been chosen on the
  AMD server, so that each table stayed near L1. On the box the probes read 101 for two partitions
  and 105 for one: one table per thread was 0.5 ms faster per parse pass than four. The same code
  runs in both modes, so no layout bias enters these readings. Releasing the input once per thread
  after the parse (12 `madvise` calls instead of 96, each a round of TLB-shootdown interrupts to the
  other threads) read 101: 0.1 ms faster.
- **Step 46 (c-11)** combines them, plus the hash's zero start state. One table per thread means one
  buffer per owner per run, and the takeover bound becomes 4 × threads buffers (the same bytes as
  16 × threads before). Each thread drops one slice of the input's pages after the parse, 12
  `madvise` calls instead of 96. Replica against step 42: 31 pairs, medians 19.010 → 18.550 ms,
  paired median −0.405 ms, 26 won; memory, max of 5 runs, 10.22, 10.67 and 10.66 MB in three
  sets; 9 fewer lines. A name has no zero byte (spec
  2.2), so the zeros past it already mark its end, and the length no longer seeds the state (12
  threads: 653.5M → 640.9M instructions).
  - *Replica.* −2.13% against step 42 (26 of 31 pairs).
  - *Board.* Nine runs, median 0.018846 s, against step 42's two runs (0.019026, 0.021546 s) and
    step 39's median of 0.019321 s.
  - *The price, in identity bits.* With one table per thread, a name's partition holds log2(12) =
    3.6 bits instead of log2(48) = 5.6, and the bucket gains 2 bits from the larger table. The
    tables compare 47.6 hash bits at 12 threads instead of 49.6. The odds that two of the
    generator's 100,000 names are taken for one go from about 5.9e-6 to 2.4e-5 per dataset (README,
    technique 6).
- **Steps 47-48: give a known-random load time to arrive.** The region table is 1 MB, and on the
  box's CPU its random reads miss L2. A record's region id is known early in its parse; its group
  is needed only to count the record.
  - *Step 47* prefetched the entry as soon as the id was known and counted each pair of records
    one loop pass later: +9.7% instructions (12 threads: the records are kept across a pass), and
    on the replica 31 pairs, medians 18.554 → 17.899 ms, −0.658 ms (−3.54%), 29 won. Memory, max of
    5 runs: 11.64, 11.05 and 10.50 MB; +10 lines.
  - *Step 48* counts each record right after the next record is parsed, so the load still has a
    record's parse to arrive and fewer records wait in registers: 201 pairs against step 47,
    17.982 → 17.628 ms, −0.387 ms (−2.15%), 183 won. Against step 46 the hot path alone measured
    −0.877 ms (−4.72%, 198 of 201), and against step 47's counting one pair later −0.28 ms
    (−1.5%, 201 pairs); without the prefetch it was +2.0% (101 pairs), with three cursors +2.1% (51
    pairs), and with the cursors in arrays +3 ms (the arrays stay in memory). It was merged with cl-1's line cuts (S 468 → 435), which leave
    the hot path untouched: `run` folded into `main`, the libc constants as commented numbers,
    `drop_pages`, `many`, `merge` and `digits` inlined, `_UNMATCHED` as group 0 of the group list.
    Memory, max of 7 runs: 10.32, 10.57 and 10.39 MB.
  - *Prefetch only* (counting at once) gave −0.26%; *deferral without the prefetch* +1.22% (both
    in the experiments table). The two work only together.
- **Step 49: shorten the chain from one record to the next.** A record's start used to come from
  the amount's end: the dot's position in the tail, then a byte load to see whether a CR follows.
  Every record ends with an LF (a CRLF or an LF; spec 2.2 has no other CR), and the tail's 32
  bytes are already loaded, so the next start is now `note_end + 2 + tzcnt(tail == '\n')`: no dot
  and no CR load on the chain (−0.57%, 131 of 201 pairs). The region id is clamped once, in
  `record`, and the count reads the entry without a check (−0.44%, 57 of 101). Together −1.36%
  against step 48 (147 of 201 pairs). Memory, max of 7 runs: 10.26 and 10.05 MB. S 435 → 451,
  mostly from cl-2's named helpers (below).
  - *A behaviour change, found by the gate.* Two of the 32 mutants replace a record's terminator
    LF by `x`. Step 46 answered them silently; step 49 aborts (exit 134). The reference rejects
    both inputs too ("bad integer 'x478058'", "expected 5 fields, got 9"), so the new answer is the
    honest one.
- **Lines and readability.** cl-1 cut 34 lines outside the hot path (S 458 → 424 on step 46's
  code), some by folding named functions into their callers. cl-2 put `run`, `merge`,
  `drop_pages` (with its `# Safety` contract), `field` and `number` back as functions, since a cut
  that makes the code harder to read costs more with the judges than its lines earn. It kept the
  cuts that remove repeated code. Both measured within the noise.
- **Step 50: review fixes, nothing on the hot path.** The libc constants are named (`PROT_READ`,
  `MAP_PRIVATE`, `MADV_DONTNEED`, `M_TOP_PAD`, `PR_SET_THP_DISABLE`, and `PAGE` in `drop_pages`).
  The comments now say what the parser checks and what happens outside spec 2.2 (from the 21
  one-record mutants, "How the final number was verified"), and one thread's odds are corrected
  to 42 compared bits. `parse_run` and `add` compile to the same instructions as step 49's. The
  replica read +0.11% (100 of 201 pairs), against +0.09% for two builds of step 49 itself. Those
  two builds differ in the worker closure's code: the profile-guided build trains on a
  multi-threaded run, whose counts vary from build to build. S 451 → 457.
- **Step 51: a parallel teardown.** The exit freed, on one core, every page still mapped: each
  thread's name table (about 83 pages each at 12 threads), the 1 MB join table and regions.csv's
  532 pages. Now, once every thread has added its totals, the threads other than the writer drop
  their own table's pages and a share each of the join table and of regions.csv while the last
  thread writes the output. Replica: −0.45% (109 of 201 pairs) in a first check, −0.56% and
  −0.60% (119 and 114 of 201) in two more, and −0.94% (128 of 201) for a probe build of the same
  code; the tables alone −0.59% (122 of 201). An earlier version at step 24b (02a535c,
  "Experiments") had shown no gain on the board. Two review fixes ride along: a comment on what a
  table's keys hold, and the wait's yield as an `if` (alone: +0.02%, 98 of 201). Phase probes
  (21 runs against step 50): the exit came 0.581 ms after the write instead of 0.709, with 3.9 MB
  still mapped instead of 10.0. S 457 → 464.
- **Step 52: the input's last third dropped during the write.** In step 51 the input's pages
  (0.53 ms at 12 threads, on every thread) were still dropped before the takes, the merge and a
  0.2 ms write during which 11 threads were idle. Now each thread drops its share of the first two
  thirds before the last takes, and the threads other than the writer drop the last third while
  thread 0, now always the writer, writes. Split points in probe builds: a half −0.58%, three
  fifths −0.68%, two thirds −0.8 to −1.0%, three quarters −0.41%. The fixed writer alone measured
  neutral (+0.04%, 99 of 201); it lets the others take the writer's shares. S 464 → 467.
- **Step 53: a named helper for the early exits.** `fast_record`'s four `(cond).then_some(())?`
  guards became `ensure(cond)?`, a three-line inlined helper. `parse_run` has 920 instructions in
  both builds, the same code with two blocks placed in another order, and `add` is identical.
  Replica +0.14% (67 of 151 pairs), within an A/A of +0.09%. S 467 → 470.
- **Step 54: rare memory spikes, found and fixed.** The robustness runs saw step 52 peak at 11.1 to
  12.1 MB in about one run in 50 to 100, against a median of about 10.0 MB. We added per-run
  counters and ran 200 runs under the memory sampler: the four that spiked (10.49 to 13.21 MB,
  against a median of 9.87 MB for all 200) all had 142 to 630 takeovers, inboxes of 113 to 178 buffers and 446 to 1,271 fresh
  buffers; normal runs had no takeover, inboxes of at most 17 buffers and about 350 fresh ones. An
  owner held its table but had lost its core, so the filers' `try_lock` for the takeover failed,
  and every filer kept filing into its inbox until the pool ran dry: two threads on one CPU, about
  2% of runs. The fix is one line: a filer whose takeover fails yields its core, which the owner
  has most likely lost to it.
  - 200 runs with the counters: max 10.47 MB, 99th percentile 10.40 (before: 13.21 and 11.24); 3
    runs with takeovers, the worst at 54 buffers.
  - Production builds, 200 runs each: max 12.00 MB before, 10.82 MB after.
  - Forced stacking (12 threads on 11 CPUs, 30 runs): max 13.37 MB before, 10.53 after; the
    median time 20.55 → 20.71 ms.
  - Not taken: blocking on the owner's lock before the takeover, also one line: 200 runs max 10.40
    MB, but 11.18 MB under forced stacking.
  - Speed: the yield alone −0.15% (109 of 201) against step 53; with the review fixes −0.28% (117
    of 201). The review fixes: `drop_pages` takes a raw pointer, so that no reference to the
    dropped memory is live across the call, and three comments that still named the last worker
    as the writer. S 470 → 471.
- **Step 55: a last record without a terminator.** Spec 2.1 allows it, the generator never writes
  it, and steps 52 to 54 stopped on it with an index out of range in `field`. An unquoted field
  now also ends at the end of the file, and the position after a field is clamped to the file's
  length, so the walker reads such a record like any other and the output equals the reference's.
  The top comment now says what is answered as parsed and what is wrong (unquoted names and notes
  give wrong byte counts). The gate passes except two mutants, changed on purpose: one without a
  final terminator now exits 0 with the reference's output, and one cut after a one-decimal amount
  exits 0 like any one-decimal amount, where the reference rejects it. S unchanged.
- **Step 56: two threads on one CPU.** About one C run in four on the board ran 20% slow (0.0204 to
  0.0215 s against 0.0171 to 0.0174 s), with A normal in the same dispatches, and 4 of steps 54
  and 55's first 6 runs. Phase marks of slow replica runs showed one thread stalled 2.5 to 5.5 ms
  early: two of our threads on one CPU while another idled, which the scheduler did not move apart
  for most of the run. Now, after the region wait, each thread sets its CPU's bit (`sched_getcpu`)
  in one shared word; a thread that finds the bit already set moves to the lowest CPU of its
  affinity mask (`sched_getaffinity`, read once) that no thread has claimed (`sched_setaffinity`),
  and stays there for the rest of the run. Only movers are pinned: pinning every thread at its
  start was worse (18 slow runs in 300 against 4, "Experiments"), and a move happens in about 3% of
  runs (9 of 300 on the replica; 8 of those 9 ran at normal speed). A normal run does nothing.
  Measured on the replica:
  - two sets of 300 interleaved pairs against step 55: medians 17.16 against 17.22 ms and 17.16
    against 17.18 ms; runs over 19.5 ms 2 against 9 and 6 against 12; the 98th percentile 18.45
    against 20.27 ms and 19.34 against 20.52 ms;
  - induced stacking (all threads started on one CPU), 41 runs: median 20.57 ms against 43.96;
  - forced stacking (12 threads on 11 CPUs): 20.14 against 20.30 ms, and no CPU outside the mask
    used (an earlier draft that took CPUs 0 to threads − 1 escaped to CPU 11: the mask fixed it);
  - ab.sh, 201 pairs: −0.39% (115 of 201).
  Not fixed: runs where one thread stalls late (its last run ends 1.5 ms late and its input drop
  takes 1.6 ms instead of 0.35), about 2% of replica runs. S 471 → 483.
- **Step 57: failed calls change nothing.** A failed `sched_getcpu` reads as a CPU outside the
  mask, which never counts as shared; a failed `sched_getaffinity` leaves no CPU to move to; a
  failed `sched_setaffinity` leaves the thread where it is. Nothing asserts on them. Replica
  +0.16% (87 of 201); the seed-1 hash also as uid 65534 under the board's timer. S unchanged.
- **Step 58: the review's code items.** The CPU move became a function with its own comment, a
  failed `mmap` now says why, and the replica numbers left `parse_run`'s comment for this file. The
  hot code compiles to the same instructions as step 57's. Replica −0.11% (106 of 201). S 483 →
  487.

## Box probes

The board reports only T, M and S of a correct run, and the exit code of a failed one. A probe
exits with a measurement as its status, so the board lists it as "✗ correctness": these were
measurements, not failures of the solution. Each probe commit was reverted by the next one. The
probes behind step 14 (b881f34, 60e1955, 43b3419, b56f58d) timed phases; their run IDs are not
in our log.

Step 34's code was probed again. The phase probes exit with the time from `main` to the written
output in 0.1 ms units. The A/B probes run the whole parse once as a warm-up and then 8 times in
one process, ABBA ABBA between mode 0 (the shipped code) and mode 1, and exit with 100 + (mean
mode-0 pass − mean mode-1 pass) in 0.1 ms units.

| Probe | Commit | Run ID | Exit | Reading |
|---|---|---|---|---|
| Phase: unmodified | 2baa984 | run-de1523b1571f | 247 | 24.7 ms (a slow dispatch: Track B 0.1312 s) |
| Phase: no region lookup | 5afab2c | run-39f6f9b9a951 | 216 | 21.6 ms (Track B 0.1296 s, slow) |
| Phase: region table cut to its first 512 KB | e3a71bc | run-333f2f901d71 | 210 | 21.0 ms (Track B 0.1211 s) |
| Phase: no deduplication | 55af820 | run-81f53eabfc7a | 151 | 15.1 ms, M 8.39 MB (Track B 0.1243 s) |
| A/B: full region table, mode 1 its first 512 KB | 8ee6e49, 6923e1e | run-d915ef465a1b, run-cb37e117cec4 | 105, 106 | the full table 0.5 and 0.6 ms slower |
| A/B: `add`, mode 1 a dedupe that prefetches the rows of 32 pairs at a time | 41cf8a0, d04fc28 | run-6e94feab4883, run-63ccad95bdb6 | 104, 104 | the batched dedupe 0.4 ms faster, both times |

The phase probes ran in four dispatches whose box state differed (Track B's unchanged code took
0.1211–0.1312 s), so they are too noisy for a 5% call; the A/B probes compare within one process.

Later probes, on steps 37 to 42, use the same A/B method, built with every function aligned to
4 KB (`-C llvm-args=-align-all-functions=12`) so that both modes' code lands alike. A probe
exits early only on inputs over 100 MB, so the Dockerfile's PGO training run exits 0.

| Probe | Commit | Run ID | Exit | Reading |
|---|---|---|---|---|
| Two code versions: step 37's hot code, mode 1 c-02's (step 39) | 8da6bc0 | run-4e00d26e2d6f | 110 | c-02 1.0 ms faster per parse pass |
| Two code versions: step 39's hot code, mode 1 c-04's | 43fbf02 | run-be32ebf496b3 | 103 | c-04 0.3 ms faster per pass |
| Two code versions: step 39's hot code, mode 1 c-06's | 09d3b3d | run-cc48b6883434 | 113 | c-06 1.3 ms faster per pass; the replica's end-to-end c-02 → c-07 was −0.17 ms (step 42) |
| Constants on step 42's code: 4 partitions per thread, mode 1 two | 9369bc4 | run-bba152c3d52a | 101 | two 0.1 ms faster per pass |
| Constants, one partition per thread: pages released per 8 runs, mode 1 once per thread after the parse | 422624f | run-66612ee03263 | 101 | once per thread 0.1 ms faster |
| Constants: 4 partitions per thread, mode 1 one | 22ac85a | run-a22416223330 | 105 | one 0.5 ms faster per pass |

The first three compare two versions of the hot functions, and two copies of a function can run
at different speeds just because of where they land (Track B measured +0.7 ms for identical
copies with this alignment). So they were hints for what to build next, and the replica decided.
The last three run one copy of the code with different constants, so they carry no such bias.

## Experiments that did not make it (measured, reverted)

| Experiment | Where measured | Result |
|---|---|---|
| 32-bit keys with a variable shift and a 64-bit check constant (first version of step 33) | server | +0.3 ms: registers spilled. The kept version shifts by a constant |
| Each thread pinned to its own CPU at its start, never released (step 56 work) | replica, 300 pairs against step 55 | 18 slow runs against 4, median 17.31 against 17.25 ms |
| The main thread pins itself to CPU w before spawning thread w, so that the child starts there; and the same with every thread released after the region wait (step 56 work) | replica, 300 pairs against step 55 | 1 slow run against 5 and 0 against 2, but medians +1.2% (17.45 against 17.24 and 17.43 against 17.23 ms): the main thread's 11 migrations make the spawn loop 0.53 ms longer |
| Each child started on {the main thread's CPU, its own} (step 56 work) | replica, 300 pairs against step 55 | 166 slow runs against 0, median 20.26 ms |
| Fewer malloc arenas (`M_ARENA_MAX`) | replica, 40 runs each in 4 interleaved rounds, the expected max of 5 by bootstrap | Step 58: median 10.11 MB, expected max of 5 10.37 MB. 3 arenas 9.91 / 10.19 MB (−0.18), T +0.79% (61 of 201); 6 arenas 9.87 / 10.19 MB (−0.18), T +0.24% (89 of 201); 2 arenas T +0.97% (41 of 201), and on step 57 median −0.5 MB, T +1.07%; 4 arenas −0.15 MB, +0.39%. Two 10-run screens had suggested −0.6 to −0.7 MB on the max; 40 runs shrank it to −0.18 MB |
| Each thread's own inbox taken before its hand-off, so the buffer pool refills before fresh buffers are allocated (on step 58) | replica | Median 10.14-10.16 MB against 10.15-10.18, max 10.34 and 10.62 against 10.52 and 10.41: no change |
| The input's page tables freed during the parse, by `munmap` of fully parsed whole 2 MB stretches | analysis, not built | At most −0.35 MB at the parse-end peak, for 15 to 20 lines, and `munmap` takes the mapping's write lock, stalling every thread's page faults; dropping pages progressively (96 calls) had already measured slower than the drop at the end |
| Constants swept on step 58 (rows below): non-PGO screens with the Dockerfile's flags minus the profile, then two PGO builds a side for anything that might pass; every variant gave the reference output | replica | Kept as they are: 64 runs a thread, a takeover bound of 4 × threads, the prefetch 1 KB ahead, the buffer margin, 64 region chunks |
| 48 runs a thread instead of 64 | replica | T −0.38% (127 of 201), but memory up: max of 20 runs 11.19 against 10.44 MB, median +0.34 MB |
| 96 or 128 runs a thread | replica, 2 PGO builds a side | 96: T +0.70 to +0.75%, expected max of 5 10.02 against 10.37 MB, about +0.07 points net, under our bar of +0.1 for a constant; 128: T +2.21% and +2.37%, 9.70 MB, about +0.03 points |
| A takeover bound of 2 × or 8 × threads buffers instead of 4 × | replica | −0.14% (114 of 201) and −0.07% (106 of 201); memory equal |
| The non-temporal prefetch 256, 512, 768 or 2,048 bytes ahead instead of 1,024 | replica, PGO for 512 to 2,048 | 512: non-PGO −0.91% (166 of 201), but PGO +0.07 to +0.30% over 2 × 2 builds: the faster PGO loop needs the longer distance, so a non-PGO screen misleads for this knob; 768: −0.28% and −0.32%, about +0.03 points; 2,048: +1.53% and +1.59%; 256: non-PGO +5.69% |
| 96 runs a thread with the prefetch 512 bytes ahead | replica, PGO | T +0.81% and +1.16%, expected max of 5 9.95 and 9.99 MB: about +0.06 points net |
| The pair-buffer margin at mean + 1 sd + 4, or mean + 3 sd | replica | + 1 sd + 4: T +0.49% (60 of 201) and expected max of 5 10.91 against 10.30 MB, as more buffers overflow and regrow; + 3 sd: −0.17%, 10.20 MB: equal |
| `madvise(MADV_POPULATE_READ)` on each run's bytes before `parse_run`, from the run's start rounded down to a page; or on a thread's whole stripe at its first claim (on step 58) | replica, PGO builds, 201 pairs | Per run: minor faults 4,927 → 2,620, T +1.95% (won 10 of 201), as in an earlier measurement (+1.96%); per stripe: faults → 1,882, T +3.28% (won 5 of 201). The fault-around traps halve, but their mapping work still happens inside the call, which stalls the parse instead of letting the faults land as the stream reaches them; per stripe, the first runs also wait for 16.8 MB to be mapped |
| 32 or 128 regions.csv chunks instead of 64 | replica | −0.15% (107 of 201) and +0.04% (95 of 201): equal |
| A compact join table read with one load (64 ids in 32 bytes: a bitmap and 24 inline group bytes, 500 KB) | analysis, not built | The footprint probe gives at most −0.31% at 512 KB, and the lookup costs about 10 instructions per record; the 18-instruction rank table measured +6.6% |
| Threads drop their tables before the last one exits (02a535c) | board run-60031508190b | 0.025472 s against 0.025101 for step 24b: no gain, reverted in 98e7cb6 |
| The end phase reordered: takes and merge first, then the writer writes while the others drop the input in claimed slices, and exits once all are dropped (step 51 work) | replica | +0.48% (88 of 201) with 4 × threads slices, +0.27% (91 of 201) with 2 ×, +0.45% (90 of 201) with 8 × |
| The join table's shares dropped before the takes, on the critical path (step 51 work) | replica | +0.29% (89 of 201) |
| Region table on one 2 MB huge page, with the first name tables in its spare megabyte (a392274, bf3a2c0) | board run-bbf744abd1a4 | 0.024972 s, S 583, 77.81 points against 77.88 for step 24b (server −0.55 ms): reverted in 2515d02 |
| Step 26's regions fill and merged own-inbox take | board run-5b165ed65fa8, run-cb0af101a699 | 0.0252–0.0253 s against 0.022853: reverted, see steps 26–27 |
| Name tables at 3/4 load instead of 5/8 | server | −0.33 MB, +0.8% time and worse collision odds |
| Rows by slot in a 7/8-full table (no row in the key) | server | 23% less table memory, +0.8M branch misses |
| Compact region table, judged by its upper bound: the region table cut to its first 512 KB, in-process box A/B | board run-d915ef465a1b, run-cb37e117cec4 | at most 0.5 / 0.6 ms to gain, and a compact table adds a dependent load (the 32-id-block row below: parse 16.2 vs 12.8 ms on the server): dropped |
| Batched dedupe (rows of 32 pairs prefetched together), in-process box A/B | board run-6e94feab4883, run-63ccad95bdb6 | −0.4 ms both times, but S 430 → 462 (+32): about −0.23 points against +0.21: dropped |
| Region-table entry prefetched as soon as the region id is known, the pair counted at once (g1, step 46 work) | replica, against step 46 | −0.26% (20 of 31 pairs): within noise |
| Three cursors instead of two (t3, step 46 work) | replica, against step 46 | −0.76% (21 of 31) |
| Four cursors (t4, step 46 work) | replica, against step 46 | +0.20% (9 of 21) |
| Each record pair counted one loop pass later, without the region prefetch (t2n) | replica, against the same with the prefetch (t2p, the next step's design) | +1.22% (7 of 21): the prefetch is what pays |
| Three cursors, counted one pass later (t3p) | replica, against t2p | +4.03% (2 of 31) |
| 48 runs per thread instead of 64 (r48) | replica, against step 46 | −0.71% (12 of 21): a tie |
| 96 runs per thread (r96) | replica, against step 46 | +1.56% (4 of 21) |
| No non-temporal prefetch of the record stream (nopf) | replica, against step 46 | +0.75% (4 of 21): the prefetch of step 19 stays |
| The record-stream prefetch 512 bytes ahead instead of 1 KB (pf512) | replica, against step 46 | +0.16% (10 of 21) |
| Compact region table: a bitmap of listed ids, ranks and a group per listed id, exact (rk) | replica, in-process probe | 86, 93, 91, 90, 96 and 108: 0.4 to 1.4 ms slower per parse pass in five of six readings |
| Timing only, wrong output: the region index masked to 512 KB, or to 256 KB | replica, against step 46 | −1.05% (17 of 21) and −6.69% (20 of 21). A table under 256 KB would be worth about 1.2 ms if it needed no second load; no exact form found |
| Two name partitions per thread (step 43) | board probe | 0.1 ms per pass faster than four, 0.4 ms slower than one (steps 43-45) |
| One-record deferral without the region prefetch (step 48 work) | replica, against step 47 | +2.02% (17 of 101 pairs) |
| Three cursors with one-record deferral (step 48 work) | replica, against step 48's hot path | +2.09% (8 of 51) |
| `add` in batches of 32 pairs with their rows prefetched (step 48 work) | replica, against step 47 | −1.20% (132 of 201), but +18 code lines: about −0.13 points of lines against +0.13 of time, dropped |
| A two-stage `add` that guesses one pair ahead (step 48 work) | replica, against step 47 | +2.51% (4 of 51) |
| Loops aligned to 32 bytes, or not aligned, instead of 64 (step 48 work) | replica, against step 47 | +0.09% and +0.15% (25 and 24 of 51): the flag stays |
| Separators by `pdep` (step 49 work) | replica, against step 48 | +1.10% (31 of 101) |
| The amount's end from the LF as well, with a CR byte load on the chain (step 49 work) | replica, against step 48 | +3.95% (3 of 101) |
| `add` in batches of 32 pairs with row prefetch, on step 49's base | replica | −0.83% (130 of 201) for +18 lines: dropped again |
| No shared buffer pool: a fresh buffer per hand-off, freed after the take (cl-1 work) | replica, against cl-1 | −10 lines, +1.24% (33 of 201); memory slightly lower |
| Every worker spawned in the scope, the main thread only joining (cl-1 work) | replica, against cl-1 | −3 lines, +0.30% (84 of 201) |
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
  29, 33 and 46), not a claim of exactness.
- **Tune on the scoring CPU.** The 4 partitions per thread of step 18 had been the fastest on the
  AMD server. On the box one partition per thread was 0.5 ms faster per parse pass (step 45), and
  the replica confirmed the whole change (step 46: −2.13%).
- **An in-process probe of two code versions is a hint, not a result.** Step 41 read 1.3 ms; the
  replica measured the change end to end at 0.17 ms (step 42).

## What we learned about the box

The box is the board's scoring machine; the test server is a 16-vCPU AMD VM. On Track C:

- **A slow mode, about one run in four.** From 15:00 to 17:16 UTC, 7 of 25 board runs of steps 49 to 54
  took 0.020717 to 0.021534 s, against 0.017095 to 0.018128 s for the rest; on the replica, 4 to 6
  runs in 300 are about 20% slow. Phase marks of slow replica runs show one thread stalled 2.5 to
  5.5 ms early in the run (it starts late, or passes the wait for regions.csv late), and takeovers
  and large inboxes after it: two threads sharing a CPU. Pinning each thread to its own CPU at its
  start made it worse on the replica: 18 slow runs in 300 against 4 (medians 17.31 and 17.25 ms).
  Moving only a thread that finds another of ours on its CPU (step 56) cut the slow replica runs
  from 9 to 2 and from 12 to 6 in 300, and its first board runs took 0.017003 to 0.017339 s.

- **Cuts of uops and branches on the per-record path pay.** The opaque `vpmovmskb` (step 7: 0.0979
  → 0.0553 s with step 6), the per-record fast path (step 15: 506 → 282 instructions per record,
  0.04093 → 0.02677 s), trusting spec 2.2 (step 25: 1042M → 854M instructions, 0.025101 →
  0.022853 s) and the stall cuts of steps 28-30 (server −5%, box −5.1%) all moved the box as they
  moved the server. Step 37 (9.1% fewer instructions, server −3.6%) ran 0.02081 s on the
  box (median of 23 runs), about 4% under step 35. Steps 39 to 46 cut instructions again (1-thread
  777.8M → 645.1M by step 42) and the box's median fell to 0.018846 s by step 46.
- **Cache-footprint cuts pay little.** On the test server, masking the region id to a 16 KB table
  saved 2.3 ms. On the box, the in-process A/B against a table cut to its first 512 KB found only
  0.5 and 0.6 ms (run-d915ef465a1b, run-cb37e117cec4), so a compact table, which adds a dependent
  load, was dropped. The non-temporal prefetch (step 19, 0.025757 → 0.025293 s) is within the
  box's spread; it stays because it costs no lines.
- **Shared random state costs.** One lock-free name table shared by all threads, on top of the SIMD
  parser, ran 0.0539 s against 0.0461 (run-edc1e75d5cee, reverted); dense name ids from a shared
  table took the server from 45 to 67 ms. Per-thread partitions with buffers handed over (steps 18
  and 22) are what stayed.
- **Changes tuned on the test server can be wrong on the box.** Step 26 timed the same on the
  server and 10% slower on the box (0.025257 and 0.02524 s against 0.022853); tables populated
  before the threads start were 20% faster on the server and slower on the box (run-b4f2d95150d7,
  0.106 s); pread was 6 ms faster on the server and no faster on the box (run-4381f7c8201d).
- **One slow CPU doubles a barrier design.** Four runs of unchanged code took about 2x (steps 21-24);
  without barriers the same load costs about 15% on the server (22.4 → 25.8 ms).
- **Fewer, larger tables per thread pay on the box.** One name table per thread instead of four
  (step 46) was faster on the box's CPU (probe: 0.5 ms per pass) and on its replica (−2.13%), though
  four had fit closer to L1 on the AMD server.
- **The board's noise is bursty.** Step 46's two slow runs came in dispatches where one other track
  was slow and another fast (step 46's notes), so marking a dispatch "slow" from the other tracks
  is only a weak filter. The replica is quiet: see "How we measured".

## How we measured

Four tools, from the coarsest to the finest.

1. **The board.** One run per push on the scoring box, an i5-13500.
   - *What shares the box.* Our program runs on P-cores 0-11 (2.5 GHz, turbo off). Meanwhile other
     teams' Docker builds run on E-cores 12-19 of the same chip: a track built with
     `target-cpu=native`, as ours are, is built on the box, two builds at a time
     (`board/README.md`). They share the L3 cache and memory with our run, and they come and go.
   - *The spread this gives.* Step 46's binary ran 0.018414 to 0.020984 s in nine runs, and step
     37's 0.020505 to 0.025598 s in 23. Earlier, single runs of unchanged code took 2x
     (steps 21-24).
   - *Use.* So board runs decide only large changes, in alternating dispatches.
2. **The test server**, a 16-vCPU AMD VM: interleaved runs of two PGO builds of each version, with
   medians, and `perf stat` instruction counts. It misleads on kernel-side changes (step 26,
   populated tables, pread: "What we learned about the box") and on constants (steps 43-46).
3. **Probes on the box** ("Box probes"). Phase probes exit with a phase time, and A/B probes with
   the difference between two modes inside one process. A/B probes of two code versions can carry
   a layout bias, so they are hints (step 42).
4. **The replica**, from Sun 13:20 UTC: a rented server with the box's CPU, set up as the harness
   sets up the box.
   - *CPUs.* P-cores 0-11 at 2.5 GHz with turbo off, E-cores 12-19, transparent huge pages
     `always`. Output to tmpfs, like `/out`.
   - *Timing.* The harness's own `sd-timer` (fork, exec and wait, timed with `CLOCK_MONOTONIC`), so
     T is exec to exit, as on the board.
   - *Builds.* The Dockerfile's `RUN` line in a chroot of the official image, on the E-cores. From
     about 14:25 UTC every build is frozen while a measurement runs, as the final scoring has the
     box to itself.
   - *`ab.sh N old new`.* N pairs, one run of each binary per pair, in alternating order (old new,
     new old, ...), each run under a bench lock. It reports both medians, the median of the paired
     differences and how many pairs the new binary won.
   - *Memory.* `mem.sh` reads the cgroup v2 `memory.peak` of each run; we quote the max of 5, as
     the final scoring takes the largest of five peaks. Every peak includes the kernel's per-CPU
     charge batches: up to 63 pages per CPU, 0 to 3 MB at 12 CPUs, different every run (measured
     on the replica; Track A's PERF.md, "What we learned about the box"). So one binary's M varies
     by about a megabyte from run to run.

   Calibration against the board, on the binaries of 13:05 UTC, 21 runs each:

   | Track and binary | Replica median (min, mean) | Board runs of the same binary |
   |---|---|---|
   | A, Track A's step 48 | 17.17 ms (16.72, 17.59) | 16.836 and 17.041 ms (2 runs) |
   | B, Track B's step 49 | 96.92 ms (96.28, 97.05) | 97.889 to 113.384 ms, median 101.022 (15 runs) |
   | C, step 39 | 19.24 ms (18.98, 19.24) | 19.031 to 21.787 ms, median 19.321 (9 runs) |

   - *Memory.* Over 5 runs (mem.sh): C 9.41-10.78 MB, against the board's 10.10-10.72 MB for
     step 39.
   - *Offset and noise.* Step 39's replica median sits at the fast end of its board runs, as
     expected when the board's noise only adds time. In an A/A test (a binary against itself),
     Track A's binary read +0.66% at 31 pairs and −0.24% at 201, Track B's +0.14% at 15 pairs.
     On a run as short as C's, a 31-pair difference under about 0.7% is read as "the same".
   - *Rule.* A change is kept only if the replica shows it faster than that, or equal with fewer
     lines or less memory; board samples then confirm it.

## How the final number was verified

**The gate.** Since step 25, before every push, on the binary the Dockerfile builds (`docker
build`, then `docker create` and `docker cp`, or the replica's chroot build of the same Dockerfile
line). For step 49 the gate ran on the replica's build, against step 46's outputs and exit
codes:
- full seed 1 at 1, 3, 7 and 12 CPUs, samples 1 to 4 and holdout 99, all equal to the reference;
- the generated seeds, the WORKED fixtures, and the RULES fixtures (abort, 134);
- `objdump | grep -c vpinsrb` = 0;
- the 32 mutants: every one exits 0 or stops with an error status (the gate logged 0, 101 and
  134), never a segfault. Two differ from step 46: the terminator LF replaced by `x` (lfx2, lfx3)
  now aborts with 134, where step 46 wrote an answer. The reference rejects both inputs too.

Steps 50 to 54 passed the same gate on the replica's builds (sha256 7f533d8d563d…,
e434a819c579…, 74d0d0bca777…, 9103dd575699… and 0dd0a9a14529…), step 55 (576fa2eac40b…) with
the two intended changes above, steps 56 and 57 (dd1989cba606… and 6478f47c380b…), and step 58
(2f0c9e28d3c7…), each against the step before: the seed-1 hash d8088b1cd42b… at
1, 3, 7 and 12 CPUs, sample seed 1 and holdout 99 equal, and every mutant's exit code unchanged.

**Robustness runs** (replica, Sun 1 Nov about 17:00 UTC), on the exact Dockerfile build, with data
generated on the replica by the organisers' generator (it reproduces the official sample seed 1
byte for byte). README, "Robust on unseen seeds", says what each column ran. Every run was
byte-exact, with no crash, no nonzero exit and nothing on stderr:

| Version | Fresh full seeds 101-130 | Repeats, and CPU contention | Tiny and sample, uid 65534, up to 20 CPUs | Page-aligned files |
|---|---|---|---|---|
| Step 52 (c-17) | 40/40 runs | 660/660 | 840/840 | 5/5 |

Earlier versions (steps 48 to 50, and step 51's code) passed too. On the 30 fresh seeds step 52
ran 17.61 to 18.09 ms (medians of 7 runs); seed 1, at 17.19 ms, is about 4% faster than a typical
fresh seed. Its memory peak on seeds 101-110 was 10.23 to 11.19 MB (max of 5 runs), and about one
run in 50 to 100 peaked at 11.1 to 12.1 MB, against a median of about 10.0 MB, where the final
scoring's largest of five peaks could land. Step 54 fixed that cause (200 runs: max 10.82 MB) and
passed the gate; steps 53 and 54 change no output path.

Two more checks, run on step 49's code, with programs kept outside the repo:
- *Name collisions.* The program's own `add`, hash and field reader, on full seed 1, sample seeds 1
  to 4 and holdout 99, at every thread count from 1 to 20: all 64-bit name hashes are distinct,
  `add` counts every distinct name once in all 120 configurations, and every pair of names that
  agrees on the partition, key and check bits sits at least 26 buckets farther apart than the
  longest run of full buckets, so no two names can be taken for one there. It also found a doc
  error: at one thread the bucket and check bits overlap by 2, so one thread compares 42 bits
  (odds 1.1e-3), not 44.
- *Outside spec 2.2.* 21 one-record mutants of sample seed 1 and the edge cases of the robustness
  runs, against `reference/c_join.py`: the table in the README ("Error handling"). A moved record
  boundary stops the program. Other breaks of 2.2 that keep the record's LF are answered as
  parsed: a quoted id or region id gives a different output than the reference's, and unquoted
  names and notes give wrong byte counts, with the other columns right. A last record without a
  terminator, which 2.1 allows, stops step 54 with an index out of range.

Step 46's gate, on an exact Dockerfile build:

- full seed 1, sha256 d8088b1cd42b…, at CPUs 0, 0-2, 0-6, 0-11 and 0-15, with three repeats;
- sample seeds 2-4 byte-equal to the reference output; holdout seed 99 byte-equal;
- tiny seeds 11-14 and sample seeds 21-23, and seeds generated fresh for these checkpoints (tiny
  61, 81 and 82, sample 64, 84 and 85, checked with `reference/c_join.py`), at four CPU sets;
- the WORKED fixtures give the expected output, and the RULES fixtures abort with exit 134;
- 32 malformed mutants of a sample, each at two CPU sets: the same exit codes as the code before
  (exit 0 or a tripwire's abort), never a segfault.

Step 39's gate (c-02) listed each check with its result: sample seed 1 9035fed36823…, the samples
at 1 and 12 CPUs, holdout 99 at 1, 5 and 12, and the mutants' exit codes, 38 × 0 and 26 × 134.
Step 39 also checked `objdump | grep -c vpinsrb` = 0.

**The box number.** Board runs of one binary spread about 3%, with outliers up to 2x. So:

- **The replica first** (from step 42): paired runs against the code a step replaces (steps 42,
  46, 47, 48, 49, 50, 51, 52, 53 and 54: −0.90%, −2.13%, −3.54%, −2.15%, −1.36%, +0.11%, −0.45%,
  −0.68%, +0.14% and −0.28%; step 55 +0.33% and −0.10% in two builds; steps 56, 57 and 58 −0.39%,
  +0.16% and −0.11%).
- **Probes** (see "Box probes"): before the replica, in-process A/B probes on the box. A pass is
  run once as a warm-up, then 8 times, ABBA ABBA between the shipped code and the variant, and the
  probe exits with the difference in 0.1 ms units. Both modes see the same box state, so a 0.4 ms
  effect can be read, which no pair of separate dispatches could show. In the four dispatches of
  the phase probes, Track B's unchanged code alone varied from 0.1211 to 0.1312 s.
- **Alternating dispatches** for whole versions: candidates pushed in turn against the code they
  replace, and kept only if the result repeats (step 26 failed twice, step 27 passed twice).
- **Board samples** confirm: step 46's nine runs have a median of 0.018846 s, step 47's three
  0.018023 s, step 48's two clean runs 0.01758 and 0.017572 s, step 49 ran 0.017192 to 0.018128 s
  in six clean runs, step 51 0.017095 to 0.017418 s in four, step 52 0.017135 to 0.017364 s in three clean runs, step 54 0.017101 to
  0.019528 s in three clean runs, step 55 0.020511 and 0.020647 s, with C alone slow both
  times, step 57 0.016978 to 0.017339 s (step 56: 0.017003 s), and the submitted step 58 0.017134 to 0.017376 s in three runs, and 0.018676 and 0.020187 s;
  run-46d6bf16c5bd
  (84.30 points) is the best Track C run.
- **Points** are the board's; M counts on this track, so no point value is computed here.

### Earlier gates

For step 33 (cmem2-1) the set was (the last gate with fresh seeds):
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
