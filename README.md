# Speed Demon 2026: team SALAAR

Team SALAAR's entry for [Speed Demon 2026](https://speeddemon.dev). All four tracks are Rust with no dependencies: the standard library only, with the few libc calls each track needs declared by hand (`dependency_count = 0` in every `.speed-demon.toml`). We tuned tracks A, B and C. Each track's README gives its design and why its output is exact; its PERF.md logs the steps from our first version to the final one with their board runs, the experiments that did not make it, and how the output was verified.

| Track | Best board run | T (s) | M (MB) | S | Points | Commit | Docs |
|---|---|---|---|---|---|---|---|
| A: Ingest | run-0b59d8b4b145 | 0.015546 | 18.04 | 436 | 87.19 | 1a5a6ba | [README](track-a/README.md), [PERF.md](track-a/PERF.md) |
| B: Index | run-dec3de0cc771 | 0.065617 | 31.73 | 1172 | 82.01 | ae81227 | [README](track-b/README.md), [PERF.md](track-b/PERF.md) |
| C: Join | run-38f216bf39eb | 0.016995 | 8.17 | 561 | 84.40 | 718f0af | [README](track-c/README.md), [PERF.md](track-c/PERF.md) |
| D: Traverse | run-15c21ec71a29 | 0.19878 | 122.87 | 589 | 64.17 | 2bf221c | [README](track-d/README.md), [PERF.md](track-d/PERF.md) |

Commit is the push the board built for that run, not always the commit of the code it ran. A's
best run is of step 77's code (008c49b, S 436), six lines shorter than the final A code (below);
B's is of step 94's code (285539d, S 1172); C's of step 68's code (9514121, S 561).

- **A: Ingest.** The log is mapped and parsed by 12 threads, two line-aligned slices side by side per thread, with the ends of a line's method and endpoint found in one 64-byte AVX2 window without a branch; each line becomes a 4-byte record, and hours are runs of lines, so no timestamp is parsed per line and no bound on the disorder is assumed. Pass 2 goes hour by hour, with a counting sort, rank counting and selection for the exact percentiles, and writes each hour's lines in place.
- **B: Index.** There is no index: the queries are read first and the corpus is scanned once, in parallel slices of whole lines, answering every query during the scan, so nothing grows with the corpus. Each token is identified by one branch-free probe of a perfect hash of the query terms, and BOOL queries are folded into truth tables evaluated per document, 8 at a time, with the common ones per 256 documents.
- **C: Join.** Each record is parsed from simdjson-style bitmaps of its quotes and commas (a carry-less multiply gives the inside-quotes mask), with its numbers in one AVX2 pass, and each thread finds its own cuts from the generator's guarantees (spec 2.2). The join is one byte load into a table by region id, prefetched a record ahead, and each thread counts distinct names in its own table, so the threads share no state; only four threads fault memory in, which keeps the peak down.
- **D: Traverse** is built and passes, but we stopped tuning it on October 31: a bidirectional Dijkstra over a CSR graph renumbered in BFS order for locality.

A's final code, 5fc484e (S 442), is step 80: step 79's with one more assert, so that a spilled id
in a thread's last hour also stops the program (noise on our replica of the box; no board run
yet). Step 79, bf4f03c (S 441), is step 78's with the inline endpoint lookup also checking the
slot after the home slot, −0.70% on 20 fresh seeds and about −0.5% on seed 1 on the replica. Its
four board runs, 0.015925 to 0.018303 s (86.78 points at best), do not show that gain: three came
in dispatches where another track ran slow too. Step 78, 631ec7f (S 437),
is step 77's with one assert per hour: a malformed latency that
spilled into an endpoint id that was never handed out made step 77's build write past a buffer
and exit 139 (SIGSEGV) on two logs built for it, and it now stops with exit 134 and a message. On
our replica of the box it runs at step 77's speed, −0.05% over four pairings of two builds a side
(noise), with equal memory. Its best board run is 0.015777 s (run-1a7f895d269e, 86.95 points);
its newest, run-1d4590b7ab6a, ran 0.015809 s (86.94 points), and five of its eight runs came in
slow dispatches (track-a/PERF.md, step 78).

B's best run is of step 94's code (285539d, S 1172): step 92's with the queries split in one AVX2 pass, built with PGO trained at full scale. It ran 0.065617 s (run-dec3de0cc771, 82.01 points), 0.43% under step 92's best run (run-2116938ec9fc, 0.065903 s), the direction our replica of the box measured but inside the board's spread. The final B code, 8f57770 (S 1174), is step 95: step 94's with the format fetching the threads' states four lines ahead, −0.36 ms (−0.54%) on the replica over 16 paired A/Bs. Its three board runs took 0.087889 s (a slow run), 0.07793 s and 0.066258 s (run-fb530d36f965, 81.90 points), 1.0% over the best run, inside the board's spread. On 18 fresh full seeds step 94's code scores 81.97 points on average (std 0.08, worst 81.72).

C's best run is of step 68's code (9514121, S 561); the board built it from 718f0af, a later push. The final C code, 905584e (S 556), is step 73: step 71's (ab83410, S 550: step 68 with the inbox bound halved and 11 cold lines fewer, at step 68's speed and memory on the replica) with the name table's key slots and `KEEP` aligned so that no hot load straddles two cache lines (step 72, 943254b, replica −0.41%) and a group's four sums added in one 256-bit add (step 73, replica −0.90% against step 72). Step 71's board runs include the fastest C time, 0.016946 s (run-a0a62bc64ea7, 10.91 MB, 83.24 points); step 72's one run, 0.020345 s (run-255af69db8ef), came with Track A slow too, and step 73's one run, 0.021108 s (run-a173e945b66c, 82.07 points), was 24% over step 68's best in a dispatch where Track B also ran 19% over its best (0.07793 s). On 20 fresh full seeds step 70's code scores 84.02 points on average (std 0.19, worst 83.49).

## Checking a track

`check.py` is the organisers' script, unchanged: it builds a track offline, runs it with the board's sandbox settings and prints the output's sha256. It needs Docker and Python 3. Each track folder holds a byte-identical copy of the organisers' generator; seed 1 at `full` scale is the board's dataset, and `sample` is 10% of it.

```
python3.13 track-a/a_ingest.py --seed 1 --scale full --out data/a
python3 check.py a --data data/a --expected 495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3
```

With `--expected`, a wrong answer fails the check. The hash above is Track A's seed 1 at `full` scale, from the organisers' spec (`specs/a-ingest.md`); every track's expected hashes are linked from speeddemon.dev. `--skip-build` runs the image of the last build. The Dockerfiles of A, C and D also use the generator copy to make their PGO training data (seed 7 at `sample` scale); B's makes a full-scale corpus (seed 7) for its PGO training, and its Dockerfile says why.

## Licence

MIT, in [LICENSE](LICENSE): copyright 2026 Hackathon Raptors CIC, the organisers, whose `check.py` and generators this repo keeps unchanged, and the contributors of this repository.
