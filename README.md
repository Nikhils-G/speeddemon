# Speed Demon 2026: team SALAAR

Team SALAAR's entry for [Speed Demon 2026](https://speeddemon.dev). All four tracks are Rust with no dependencies: the standard library only, with the few libc calls each track needs declared by hand (`dependency_count = 0` in every `.speed-demon.toml`). We tuned tracks A, B and C. Each track's README gives its design and why its output is exact; its PERF.md logs the steps from our first version to the final one with their board runs, the experiments that did not make it, and how the output was verified.

| Track | Best board run | T (s) | M (MB) | S | Points | Commit | Docs |
|---|---|---|---|---|---|---|---|
| A: Ingest | run-0b59d8b4b145 | 0.015546 | 18.04 | 436 | 87.19 | 1a5a6ba | [README](track-a/README.md), [PERF.md](track-a/PERF.md) |
| B: Index | run-96f4490eeb42 | 0.066 | 31.37 | 1175 | 81.94 | 718f0af | [README](track-b/README.md), [PERF.md](track-b/PERF.md) |
| C: Join | run-38f216bf39eb | 0.016995 | 8.17 | 561 | 84.40 | 718f0af | [README](track-c/README.md), [PERF.md](track-c/PERF.md) |
| D: Traverse | run-15c21ec71a29 | 0.19878 | 122.87 | 589 | 64.17 | 2bf221c | [README](track-d/README.md), [PERF.md](track-d/PERF.md) |

Commit is the push the board built for that run. A's and C's best runs are of their final code.

- **A: Ingest.** The log is mapped and parsed by 12 threads, two line-aligned slices side by side per thread, with the ends of a line's method and endpoint found in one 64-byte AVX2 window without a branch; each line becomes a 4-byte record, and hours are runs of lines, so no timestamp is parsed per line and no bound on the disorder is assumed. Pass 2 goes hour by hour, with a counting sort, rank counting and selection for the exact percentiles, and writes each hour's lines in place.
- **B: Index.** There is no index: the queries are read first and the corpus is scanned once, in parallel slices of whole lines, answering every query during the scan, so nothing grows with the corpus. Each token is identified by one branch-free probe of a perfect hash of the query terms, and BOOL queries are folded into truth tables evaluated per document, 8 at a time, with the common ones per 256 documents.
- **C: Join.** Each record is parsed from simdjson-style bitmaps of its quotes and commas (a carry-less multiply gives the inside-quotes mask), with its numbers in one AVX2 pass, and each thread finds its own cuts from the generator's guarantees (spec 2.2). The join is one byte load into a table by region id, prefetched a record ahead, and each thread counts distinct names in its own table, so the threads share no state; only four threads fault memory in, which keeps the peak down.
- **D: Traverse** is built and passes, but we stopped tuning it on October 31: a bidirectional Dijkstra over a CSR graph renumbered in BFS order for locality.

B's best run is of earlier code. The final B code, b21eb1b (S 1157), cuts 18 lines from it at the same speed or better on our replica of the box (the same CPU and the board's timer): −0.157 ms on seed 1 (20 of 31 pairs faster) and −0.073 ms on average over 16 fresh seeds. On the board it has run 0.066969 s (run-a492afc03dad, 81.83 points) and 0.086307 s (run-ac43df513301, 79.08 points, after 1a5a6ba, which changed only comments and names and left the machine code byte-identical). That spread is the board's noise, not the code: the best run's own code also ran 0.077266 s (run-2e8c0a9b6f41) and, on a disturbed box, 0.147443 s (run-77f4c7adf864).

## Checking a track

`check.py` is the organisers' script, unchanged: it builds a track offline, runs it with the board's sandbox settings and prints the output's sha256. It needs Docker and Python 3. Each track folder holds a byte-identical copy of the organisers' generator; seed 1 at `full` scale is the board's dataset, and `sample` is 10% of it.

```
python3.13 track-a/a_ingest.py --seed 1 --scale full --out data/a
python3 check.py a --data data/a --expected <sha256>
```

With `--expected`, a wrong answer fails the check; the expected hashes are linked from speeddemon.dev. `--skip-build` runs the image of the last build. The Dockerfiles of A, C and D also use the generator copy to make their PGO training data (seed 7 at `sample` scale); B builds without PGO, and its Dockerfile says why.

## Licence

MIT, in [LICENSE](LICENSE): copyright 2026 Hackathon Raptors CIC, the organisers, whose `check.py` and generators this repo keeps unchanged, and the contributors of this repository.
