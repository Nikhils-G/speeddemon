# Track B: Index

**Result.** The submitted code is PERF.md's step 95 (8f57770, S 1174). The best of its nine board
runs is run-4acf92b53275: 0.066011 s at 32.14 MB, 81.94 points. The best Track B run,
run-dec3de0cc771 (0.065617 s at 31.73 MB, 82.01 points, 859× faster than the reference's 56.35 s at
2,200 MB), is of step 94's code (285539d, S 1172), which step 95 changes by one prefetch in the
format; it came from push ae81227, which changed no Track B file. The two runs are 0.6% apart,
within the board's noise, so step 95's gain (−0.36 ms, −0.54%) rests on paired runs on a replica of
the box. On 18 fresh full-scale seeds, step 94's code scores 81.97 points on average (std 0.07; by
the final's middle-3-of-5 rule 81.97, std 0.08, worst 81.72).

Three ideas carry the speed: no index (step 6: 0.355 s at 203 MB → 0.267 s at 38.4 MB), a
perfect hash with one branch-free probe per token (step 31: 0.1338 → 0.1199 s), and BOOL queries
as truth tables evaluated per document (step 49: best runs 0.108491 → 0.097889 s).

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).
The step-by-step history, with a board run for every step that reached the board (all but steps
25 and 27), is in [PERF.md](PERF.md).

## Build and reproduce

**The build** (`Dockerfile`, one `RUN`). The board builds this track on the scoring box with
`-C target-cpu=native` (a `compile_error!` stops a build without AVX2, BMI2 or AES-NI),
`crt-static`, fat LTO, one codegen unit and `panic = "abort"`, and no dependencies. The build is
profile-guided:
1. an instrumented build (`-C profile-generate`, with LLVM's atomic counters, so the training
   run's threads lose no updates);
2. `b_index.py --seed 7 --scale full`, a byte-identical copy of the organisers' generator, makes a
   full-size corpus (there is no /data at build time), and the instrumented binary runs on it once;
3. `llvm-profdata merge`, then the profile-use build. The runtime image holds only the static
   binary.

**Why full scale and seed 7.** Measured on the replica, paired runs (PERF.md, steps 81 and 93, and
"Experiments that did not make it"):
- trained at the sample scale (steps 5 to 80), the build ran 0.55 to 0.67 ms slower than a plain
  one on step 81's code, so steps 81 to 92 built without a profile;
- trained at full scale on seed 7, it ran −0.414 ms (seed 1, 25 of 31 pairs) and −0.383 ms
  (holdout 99, 26 of 31) against step 92's plain build; a second PGO build −0.400 ms (24 of
  31), of the same code with one comment more;
- a profile of the timed input itself was no better: seed 1's profile ran 0.32 to 0.73% slower
  than seed 7's on seed 1 and two fresh seeds, and holdout 99's 0.00 and 0.12% slower on holdout
  99. Seed 7 is not an input we time on, and fitting the profile to the timed data gains nothing;
- seeds 7, 8 and 9 merged ran 0.50 to 0.86% slower: that profile, like seed 1's, stops inlining
  `Scan::end_doc` into `Scan::run`, which every seed-7 build did (7 of 7);
- the training run made three times was 0.21 to 0.23% faster (five builds against four), about
  +0.02 points: under our bar for a change, so not taken.

**PGO builds differ.** The training run's counts depend on thread timing, so every build is its own
binary: two builds of step 94's source differ in 376,583 of 1,493,248 bytes. Two builds of one
source ran 0.419 ms apart (4 of 31 pairs); another pair +0.31% (seed 1) and −0.28% (holdout 99);
over 18 fresh seeds two builds differ by −0.09 ms on average (std 0.22), and neither is faster on
some seeds only. The board's build is another draw. So from step 94 every A/B uses at least
two builds a side, crossed, and the mean decides.

**The speed claim rests on many builds** (replica, paired median new − old, pairs won of 31):

| Claim | Builds compared | Measured |
|---|---|---|
| Step 93: PGO at full scale, against step 92's plain build | 2 PGO builds | seed 1 −0.414 ms (25) and −0.400 ms (24); holdout 99 −0.383 ms (26) |
| Step 94: the queries split in one AVX2 pass | 4 builds of step 93's source × 2 of step 94's: 24 A/Bs on seed 1, holdout 99 and fresh seeds 101, 103, 105 and 107 | mean about −0.36 ms, 23 of 24 below zero; against the two faster old builds alone about −0.21 ms |
| Step 95: the format's prefetch | 2 × 2: 16 A/Bs on seed 1, holdout 99 and fresh seeds 118 and 126 | mean −0.356 ms (−0.54%), 15 of 16 below zero; against the faster old build alone −0.274 ms |
| Points on unseen seeds, step 94's code | 2 PGO builds, 18 fresh full seeds (8201-8210, 9101-9108) | mean 81.97, std 0.068 (T the median of 11 runs); by the final's rule 81.97, std 0.082, worst 81.72, best 82.15; T 65.09 to 66.62 ms; M at most 32.60 MB |

On the board these steps are within noise. Step 92's machine code ran 0.065903 to 0.098793 s in
seven runs; its best, run-2116938ec9fc (0.065903 s, 82.01 points), came from push 631ec7f, which
changed no Track B file. Step 94's best run is 0.43% under it, the direction the replica measured,
and step 95's best is 0.6% over step 94's. One binary's board runs have spread up to 45% in
disturbed dispatches (PERF.md, "Reading the numbers").

**Checking the output.** With Docker and Python 3, from the repository's root (root README,
"Checking a track"):

```
python3 track-b/b_index.py --seed 1 --scale full --out data/b
python3 check.py b --data data/b --expected f86973c210228141bcb6f1f1b10188ad2a9b65f71e7cd7cf4590b38de4e80927
```

The hash is the spec's for seed 1 at full scale. For any other seed, generate it the same way, run
the organisers' `reference/b_index.py` on the same files and compare `results.txt` byte for byte.
Before each push our gate kit did that on the replica, on the exact Dockerfile build. For the
submitted code, two PGO builds passed 14 of 14 checks each (full seed 1 at 1, 3, 7 and 12 CPUs;
holdout 99 and samples 1 to 4 at 12 CPUs and at 1), and the second also the gate script's 144
tiny, fresh and adversarial runs: every output byte-equal to the reference. Step 94's final gate
added 30 fresh full seeds at four CPU counts (120 of 120) and 18 board-like edge cases as root
and as uid 65534 (72 of 72). The rest is under "Why the output is exact" and "Robust on unseen
seeds" below, and in PERF.md, "How the final number was verified".

## Reading the code

Read the files in this order: `main.rs`, `vocab.rs`, `plan.rs`, `scan.rs`, then `pool.rs` and
`mmap.rs`. `main` (`src/main.rs`) is the whole run, top to bottom:
1. `vocab::parse_queries` splits queries.txt on every thread and builds the perfect hash of its
   terms (`Vocab::new`).
2. `slices` cuts docs.txt into at least 8 slices of whole lines.
3. A sample `Scan` per slice counts each term's document frequency.
4. `Plan::new` (`src/plan.rs`) gives each BOOL query a cover and its truth tables (`cover`,
   `doc_entry`), or marks it `always`; it anchors each phrase and numbers the answers.
   `Vocab::finish` writes each term's new id and flags into the table.
5. One `Scan::run` per slice (`src/scan.rs`): `masks` finds the tokens 64 bytes at a time,
   `look_up` probes the table, `end_doc` and `trigger` evaluate each document, and `end_super`
   evaluates every 256 documents.
6. Each slice's first ids are put in line order. `format_part` formats ranges of lines in
   parallel, and the main thread writes results.txt.

| File | What it holds | Its `unsafe`, and why it is sound |
|---|---|---|
| `main.rs` | `main`; `slices` and `line_after` (cuts at line ends), `parse_id`, `format_part` and `push_decimal` | The C `main` symbol (`#![no_main]`). Each `Scan::run` gets a slice that ends inside the text, and `mmap::PAD` (256 zero bytes) follows the text, which covers the scan's loads past `end`. `mmap::release` only on memory that is not read again before the exit |
| `vocab.rs` | The query split (`split_lines`), the perfect hash (`Vocab::new`, `Phf`, `hash`, `place`, `find_pilot`), the token key (`key_at`) and `probe`; long terms (`get_long`) | Intrinsics: AVX2, BMI2 and AES-NI are enabled for the whole build, or it does not compile. Loads of 16 or 32 bytes stay inside the file and its pad. Each thread writes only its own regions of the table until the barrier. `probe`'s `cmov` is inline asm on registers only, after one aligned load of a slot |
| `plan.rs` | Covers (`cover`, `classify`), truth tables (`doc_entry`, `DocEntry`), the term order, the answer numbers (`number`), and `csr`, the counting sort behind every grouping | `csr` is an `unsafe fn`: its closure must give the same pairs on both passes, which each caller's does, since it reads data nothing changes between them; so each slot is written once before the length is set |
| `scan.rs` | The hot loop: `masks`, `Scan::run`, `look_up`, `end_doc`, `trigger` (8 entries at a time), `end_super`, `Answers`, `count_positions`, `left_pack` | Most of the `unsafe` is here. Raw writes go into capacity reserved for the most a loop can write (32 tokens a window; a document's tokens plus 8), then `set_len`. Table offsets come from `Phf::slot`, below the table's size; stamp and row indices from ids at most the term count. `run`'s `# Safety` contract, `end + 16 <= docs.len()`, is met by the pad |
| `pool.rs` | `par`, its workers, `wait` (`umonitor`/`umwait`, or a polling loop) and `ahead` | The job's lifetime is erased to keep it in a static. A worker reads it only after the generation is published and holds the read lock while it runs; `par` returns only when every worker has counted the job done, and an assert allows one call at a time. The `umwait` asm writes no memory |
| `mmap.rs` | The libc calls declared by hand (`mmap`, `madvise`, `prctl`, `mallopt`), `map` (a file plus 256 zero bytes) and `release` | glibc's x86-64 prototypes. The file is mapped over a fresh anonymous reservation, read-only, from a read-only mount. `release` is an `unsafe fn` whose callers never read the pages again |

Every `unsafe` block carries a SAFETY comment, and every `unsafe fn` a `# Safety` section. Bad
input stops with a message (Error handling). `b_index.py` is the generator's copy for the PGO
training; it is not part of the program.

Two comments in `main` give a layout reason for a choice: the data paths joined by `concat`, and
`File::create` given a reference to the path. Both were measured on the plain builds of steps 87
to 89. Under PGO (step 93 on) the profile lays out the code and those offsets vary from build to
build, so that reason is not verified for the shipped binary (Index, item 62).

## The problem

250,000 documents (254 MB) and 50,000 queries known up front:
- 31,000 BOOL queries of 1 to 6 terms, evaluated strictly left to right;
- 19,000 phrases of 2 to 8 terms.

Each answer is the match count, the sum of the matching ids and the ten smallest ids.

## The bottleneck

- **Memory, first.** The obvious design is an inverted index of the query terms. It holds about
  21M (term, document) pairs for the BOOL terms alone, and however it was packed (steps 1 to 5 in
  PERF.md), the run peaked at about 200 MB. The M axis gives full credit only under 44 MB (the 50x
  cap on the reference's 2,200 MB), so at 200 MB memory alone cost about 6.6 points.
- **Then the token path.** Without an index, the work is one pass over 36.3M tokens. 76% of them
  are BOOL query terms, 3% are phrase-only terms and 21% are words no query uses, and each one must
  be identified exactly.
- **Then the BOOL evaluation's random loads.** On the box, the block engine of PERF.md steps 6 to
  48 spent 16 ms setting presence words and 18.5 ms evaluating blocks, against 38 ms for the token
  path (exit-code probes, PERF.md "Box probes since step 47"). Those were loads at random
  addresses. The per-document design below removed the presence words and the block ends.

Where the submitted code's time goes, on the replica of the scoring box (PERF.md, "How we
measured": the same CPU, the board's own timer). These are step 86's phase marks, from a build
that prints the time at each phase boundary (seed 1, medians of 9; PERF.md step 86, "What is
left"). Steps 87 to 89 change only the start-up and the line count, step 90 takes 0.92 ms off
the plan, step 91 keeps `csr`'s running sums in registers and cuts 18 lines, step 92 changes
comments and names only, with step 91's machine code, step 93 builds with PGO trained at full
scale (replica −0.41 and −0.38 ms), and step 94 splits the queries in one AVX2 pass (the parse
−0.42 ms) (PERF.md, steps 90 to 94). The later setup marks, on plain builds (seed 1, medians of 9;
PERF.md step 94): the queries parsed at 3.282 ms on step 92's code and 2.862 on step 94's, the
sample done at 4.347 and 3.921, the plan at 6.862 and 6.504, the vocabulary finished at 7.082 and
6.736. The scan, merge, format and release phases have not been marked again since step 86.

| Phase | Ends at (ms) | What |
|---|---|---|
| Split, perfect hash, sample | 4.3 | the queries split and hashed on every thread; then the frequency sample |
| Plan | 7.9 | covers, truth tables, anchors, answer numbers; the waiting workers finish mapping the corpus at 5.9 |
| Scans | 61.5-62.3 | twelve slices (`Scan::new` 0.46 ms a thread): tokens, lookups, per-document evaluation, superblocks, phrases |
| Merges | 62.9-63.2 | first ids put in line order |
| Format | 64.5 | |
| Releases | 65.6 | pages given back while results.txt is written |

The scan is about 54 ms of 66. The twelve scans end within 0.8 ms of each other here; over 31
runs of step 92's code their ends spread 0.6 to 1.9 ms a run, the last 0.41 to 0.50 ms after
the mean, with no slice last every time. A tail taken over by another thread would cost it a
`Scan::new` (0.27 to 0.48 ms), its line ends and a log loop, so no work stealing was built
(PERF.md, experiments). Those runs were quiet: B's time with one CPU busy, where the run waits
for the slowest thread's one slice, has not been measured (the busy-loop runs below check only
the output). Builds with wrong output, used for timing only, price what is left in step 86's
scan:
- making the answer states hot saves 1.769 ms, mostly through the first-id log;
- skipping the merge's log loop saves 0.436 ms;
- dropping the log's writes during the scan saves a further 1.109 ms.

The phase table, box probes and part-skipping builds of steps 49 and 53 are in PERF.md ("How we
measured", "Box probes since step 47" and "Where the scan's time goes").

## The design

There is no index. The queries are read first, the corpus is scanned once in parallel slices of
whole lines, and every query is answered during that scan, so nothing grows with the corpus. The
board's M for step 94's four runs was 31.38, 31.73, 31.73 and 31.85 MB, for step 95's eight 31.42 to 32.06 MB (step 92's machine code's
seven: 31.31 to 31.81 MB), and the replica's max of 20 runs of its gate build 31.65 MB, under the
44 MB credit; every step's M is in PERF.md's step table.
malloc's mmap threshold is fixed at 1 MB, with transparent huge pages off (`mmap::set_up_memory`):
at 32 MB the scan's first-id logs came from cleared heap and the peak reached 40 MB. Since step
82 freed memory also stays in the heaps (a trim threshold of 1 GB): a trim flushes the TLB of every
CPU the threads ran on, and each phase reuses what the one before freed, for about 1.5 MB more
peak.

1. **Setup, in parallel.** Each phase runs on every thread, and allocates little, because what
   parallel setup mostly waits on is page faults of fresh memory.
   - *One pool.* The eight setup phases, the scan and the formatting all run on one pool of
     threads, made by the first call to `par`. Between calls a worker waits in C0.2 with
     `umonitor`/`umwait` (WAITPKG, checked by CPUID; without it, a polling loop): no call spawns a
     thread, each worker keeps its heap arena, its SMT sibling gets the core, and its CPU stays out
     of the deep idle states that take up to 1 ms to leave. On the replica this ended setup at
     11.10 ms instead of 12.39 ms.
   - *Mapping ahead.* A worker that waits for its next job reads one byte of the corpus per 64 KB
     (`ahead`), and the kernel maps the 16 pages around each read (fault-around), so the scan finds
     its pages mapped. The reads are interleaved over the slices, so every slice gains alike.
     `madvise(MADV_POPULATE_READ)` instead held `mmap_lock` and stalled the main thread's
     allocations; a page fault takes only its VMA's lock (Linux 6.4 and later).
   - *Split.* Each thread splits a range of query lines into compact records. A term is 8 bytes:
     its offset in the file, and its length | op << 30. A key is 16 bytes: its hash, offset and key
     length; the key bytes are reloaded from the file when needed. The vectors are sized for the
     most terms a chunk can hold, so they never grow and never copy, and their untouched pages
     cost nothing. The keys are grouped by hash group (`hash & 63`) with the same counting sort,
     `csr`, that the plan uses.
   - *Perfect hash* of the terms, built on every thread (design 2). The ids are then read back for
     every term. The map of long terms is consulted only for terms over 12 bytes.
   - *Sample.* The start of every slice, 2.5 MB in total, is scanned to estimate each term's
     document frequency; each sampled document's terms are counted once by stamps, as in the scan.
   - *Plan*, a range of query lines per thread:
     - whether each line is an `always` query (design 4), with each term's kind set by an atomic
       OR;
     - the term order (BOOL terms first, so that they index dense per-thread arrays), by a
       counting sort;
     - each part renumbers its own terms and builds its entries: a truth-table entry per (query,
       cover term) (design 3), an `always` entry, or a phrase record;
     - `csr` groups the entries by trigger term, and the same thread numbers their lines (design
       5), while two more workers (a `par(3)` round) group the phrases by anchor and sort the
       `always` entries on one packed key. `csr` counts each key, turns the counts into run ends
       by a fold whose sum stays in a register (reading the sum back from memory cost a
       store-to-load forward per key), and writes each value into its run from the end, last
       value first, straight into a vector's spare capacity: no zero fill and no copy.
2. **Finding and looking up tokens.**
   - *Windows.* Each 64-byte window is classified with AVX2 into a token-byte mask and an LF mask.
     A window starts at a byte that is not inside a token and is scanned up to its last non-token
     byte, so every token it scans ends inside it. Token starts and last bytes are bit operations
     on the mask, and a token's length is the distance between the two. The next window is fetched
     before this one is scanned.
   - *Keys.* A token's key is its first 12 bytes, lowercased and zero-padded in one 16-byte
     register.
   - *Perfect hash.* The query terms of up to 12 bytes (75,735 distinct keys on seed 1) are in a
     perfect hash, so a token is compared with one 16-byte slot, never more:
     - *Hash.* Two AES rounds of the key (`aesenc` twice, the seed XORed into the first round key),
       the low 64 bits kept. After two rounds every output bit depends on every key byte. Attempt
       a's seed is a × 0x9e3779b97f4a7c15, which changes all 8 bytes it is XORed into, so every
       hash bit depends on the seed too. It costs three instructions per token.
     - *Pilots* (PTHash style). A hash h falls in bucket `h & bmask` (16,384 buckets on seed 1,
       about 4.6 terms each), and the bucket's 16-bit pilot places it in slot
       `(h * pilot).rotate_right(6) >> shift`. The builder takes the largest buckets first and gives
       each the first pilot whose slots are all free, so every term gets a slot of its own.
     - *Parallel build.* Every pilot is 1 mod 64, so the low 6 bits of `h * pilot` are those of h
       (its group), and the rotation makes them the slot's top bits. Each of the 64 groups thus owns
       one region of the table, whatever its pilots. Each of the 12 threads sorts, deduplicates and
       places a range of groups in its own regions, with no locks; then a barrier, then each thread
       reads back the ids of its share of the query lines.
     - *Slot.* 16 bytes: the term's 12 key bytes under its 4-byte info word (its id, and the flags
       ANCHOR and TRIG). The table is 2^17 slots (2 MB, a load of 0.58) plus one slot per long term
       and a final MISS slot; empty slots keep a zero key, which no token's key equals, and
       `Vocab::finish` gives them id 0's info word.
     - *Prefetch.* The slot's byte offset is computed and prefetched as soon as the token is found,
       and a document's tokens are all found before any is looked up, so the misses of one
       document overlap. A found token waits in `pending` as 16 bytes: its key, with the slot's
       offset written over the key's top 4 bytes, which the probe does not compare (two plain
       stores; 32 bytes a token until step 78).
     - *Probe.* One aligned 16-byte load, an XOR with the token's key, a `vptest` of the 12 key
       bytes, and a `cmov` that selects the info word (a 4-byte load of the same slot) or MISS.
       There is no branch: whether a token is a query term is too random for the branch predictor.
     - *Re-seed.* If a bucket finds no free pilot among its 1,024 candidates, the thread that
       owns its region places that region's buckets again, that bucket first, up to four times; the
       other threads are not affected. If that fails too, or two different keys of a group have
       the same 64-bit hash, the build runs again with the next seed, and every third seed doubles
       the table. This ends: every third seed halves the load, so the pilot search finds free
       slots, and two different keys share all 64 bits of a hash with probability about 2^-64.
       Over 300 hash seeds on each of 32 datasets (9,600 builds), no build needed a second seed;
       with step 57's build 257 did (2.68%, on 20 of the 32 datasets). Seed 0 needed no second
       placement on any of the 24 datasets of the first sweep (PERF.md, step 58). A second placement costs
       about 0.4 ms on its thread; a second seed cost 3.70 ms in a forced test. The output is the
       same whichever seed succeeds.
   - *Long terms.* Terms of 13 to 32 bytes (0.2% of the tokens) go to a cold hash map. Their slots
     sit after the hash's, so the scan treats both alike.
   - *Presence and lists.* `look_up` probes a document's tokens in one loop that stores only their
     info words, then passes over them 8 at a time. For the 8 tokens together it makes their ids
     (`vpand`), stored over their info words, and their rows' word offsets (`vpminud` for
     `id.min(rows)`, then a shift), and left-packs the trigger occurrences and the anchors'
     positions, 8 lanes per store (`vmovmskps`, a lane table, `vpermd`, as in design 3's
     evaluation). For each token it writes the stamp (design 3) and the row bit (design 4), and
     fetches the stamp's line for writing (`prefetchw`) just before the stamp store: stores commit
     in order, so a stamp store that missed L1 held up every store after it, while the prefetch
     fetches the line out of order. Against step 76: replica −2.1 to −2.8 ms, and store-buffer
     stalls 161M → 50M cycles for the prefetch alone (PERF.md, step 77); the ids and rows 8 at a
     time, −0.3 to −0.5 ms more (step 78).
3. **BOOL queries, per document, from truth tables.**
   - *Covers.* A query is evaluated only in documents that contain one of its *cover* terms: a set
     of terms of which every match contains at least one, chosen to be rare in the sample.
   - *Entries.* For each (query, cover term c), the plan makes one 16-byte entry: the query's other
     distinct terms (at most 5, two bytes each, padded with term 0), its answer (two bytes), and a
     32-bit truth table. Bit a of the table says whether the query matches when c is present and exactly the
     other terms of a's set bits are present, folding AND, OR and NOT strictly left to right. It is
     0 wherever a cover term before c is present: that term's entry counts the document instead.
     The table is built over the k real terms, so its bits from 2^k on repeat the first 2^k, and
     the padding term's presence does not change the bit looked up. Each trigger term's range of
     entries starts at a 16-bit index (`trig_start`); past 2^16 BOOL terms or entries, the plan
     evaluates every BOOL query per superblock instead (step 83); past 2^16 lines the program
     stops (Error handling).
   - *Presence.* Each thread keeps, per term, a one-byte *stamp* of the last document that contains
     it: the document's id mod 127, written for every token. Every 127 documents all stamps are
     reset to a value no document has, so an equal stamp always means this document.
   - *Evaluation.* At a document's end, each trigger term of the document is taken once (its stamp
     gets a DONE bit). An entry's 5 stamp compares give 5 presence bits, and its answer is one bit of
     the truth table. On seed 1 a document has 18.0 fresh trigger terms and 27.2 entries, evaluated
     in 3.84 batches of 8, 59% of them matches (PERF.md step 83; with step 66's ALWAYS 0.30 the
     corpus took 5.51M evaluations); every BOOL query on every document would be 7.75 billion.
   - *Eight at a time.* A first pass stores the entry indices of each fresh trigger term, 8 per
     AVX2 store, into one list (the next store overwrites the spare lanes; the rare range of over 8
     entries is stored on) and prefetches each range's first entry. Each 8 indices are then
     evaluated in registers: the 8 entries come in with 8 aligned 16-byte loads and a transpose
     (4 `vinserti128`, 8 unpacks), so lane j of word k is word k of entry j; 5 gathers fetch their
     terms' stamps; the presence bits are shifted in term by term, and `truth >> presence`. The
     matches' answers are left-packed straight from the entries' words (the 8-bit match mask picks the lane numbers of its set bits
     from `PACK`, a 2 KB table built at compile time, and `vpermd` moves the answers; until step
     80 `pdep` and `pext` made the lane numbers), and only the matches update their answers. No
     branch depends on whether an entry matches, which is a coin flip. Entries are 16 bytes
     (`repr(C, align(16))`), so each is one aligned load (until step 83 they were 32 bytes, and
     until step 85 their words were gathered), and the last batch's spare lanes take the last of 8
     padding entries, which never match. Against step 65's
     range-by-range loop: replica −2.3 to −2.7 ms (−2.8 to −3.3%).
4. **Common queries, per superblock of 256 documents.** Some queries have cover terms expected in
   more than 40% of the 64-document blocks, summed over the terms (`ALWAYS`, 0.40 since step 79,
   0.30 before: 4,342 queries on seed 1, PERF.md step 84). They would be triggered in almost every document. They are evaluated once per 256 documents
   instead, with AVX2 on one 256-bit row per term (one bit per document), and only over their
   real terms. A row's four 64-bit words, one per block, sit side by side: word b of row r at
   `4 * r + b`, so `end_super` reads a term's 256 bits with one 32-byte load. (From step 56 to
   step 77 the words were kept by block, at `b * (rows + 1) + r`, so that the word a token sets
   lay in its block's contiguous array, and `end_super` gathered a term's four words from lines
   72 KB apart; with the presence stores made 8 at a time, side by side is 1.0 to 1.1 ms faster.)
   The queries are sorted by term count, operators and their first term's row, so the operator
   branch predicts. Each token sets its document's bit in its term's row; a token of no row term
   goes to one sink row, chosen without a branch (`vpminud`, 8 tokens at a time).
5. **Answers numbered in evaluation order.** Each thread keeps one u64 of state per query (count
   and id sum) and a log of first ids, 4 bytes each (`answer << 16 | id & 0xFFFF`). Queries are numbered in the order the scan updates them:
   - the entries' answers first, in table order (numbered as the grouping thread places them);
   - then the `always` queries, so that every superblock reads and writes their state as one
     stream of about 35 KB (4,342 queries on seed 1, PERF.md step 84) instead of about 4,300
     scattered cache lines;
   - then the phrases.

   `answer_of` and `line_of` map between query lines and these numbers.
6. **Counts, sums and first ids without a loop over documents.**
   - *Per document:* each matching entry adds `1 << 40 | doc` to its answer's state; the
     first-id log slot is always written and kept only while the answer has fewer than ten.
   - *Per superblock:* the count is a popcount. The sum of the matching ids is
     `base * popcount(w) + sum over k of 2^k * popcount(w & M_k)`, with six fixed masks M_k (M_0 =
     0xAAAA..., the odd positions); for 256 documents at once, nibble lookup tables do the same in
     AVX2, their byte counts summed per 64-bit word by `psadbw` (step 84).
   - *First ids:* from trailing-zero counts, and only while a query has fewer than ten.
7. **Phrases.**
   - *Anchors.* Each phrase is anchored on its rarest sampled term. Its 64-byte record holds its
     first 8 term ids, its answer, the anchor's position and its length.
   - *Fetching.* `look_up` lists the positions of the document's tokens whose info word has the
     ANCHOR flag (design 2); at the document's end, each anchor's first record is prefetched before
     the BOOL evaluation and read after it.
   - *Check.* Every phrase is checked by one masked 32-byte compare against the document's token
     ids, which are still in L1. `look_up` writes 8 ids of `miss` after each document's tokens, so
     the 8 ids from any phrase start exist, and none equals a phrase term (`miss` is the id of every
     token no query uses). A phrase of over 8 terms, which the spec does not have, stops the program
     when the queries are planned.
   - *Counting.* A per-phrase last-document marker counts each document once.
8. **Merging.**
   - *Release.* While the main thread writes the output, giving each chunk back once written, the
     workers give back the corpus, every slice's answer states, line ends and first ids, the hash
     table, and the plan's phrase records and entries, each in shares (`mmap::release`, in the
     last `par` round of `main`; nothing is released during the scans), so the exit has less to
     unmap on one core: since step 81, 10.9 MB is left at the exit instead of 16.3 MB, and the
     exit takes 1.48 ms instead of 1.77 ms. Each slice's first-id log, a 2 MB mapping of its own,
     is given back with `MADV_DONTNEED` once the merge has read it, and its empty mapping left to
     the exit (step 86): twelve `munmap`s at once had queued on `mmap_lock` for 0.53 ms.
   - *First ids, without a sort.* An answer logged its first min(count, 10) ids in id order, so a
     prefix sum of those counts over the lines gives every entry its place in line order, and one
     scatter puts it there, each id rebuilt from its low 16 bits and its slice's first id. The
     slices are in document order, so the logs merge in slice order into the ten smallest ids. The
     first slice's thread publishes which answers it alone fills with ten (`full`); the other
     threads skip those, which hold most of their logged ids (about 100K of 122K each on seed 1).
   - *Formatting.* Output lines are formatted in parallel. Each formatter adds up the threads'
     counts and sums by reading their states in place through `answer_of`; a copy into line order
     cost about 4.5 MB and once put the run over the 44 MB credit (PERF.md, step 26). A thread's ids for
     a line lie between that line's end and the one before (`ends`).
   - *Exit.* The process exits right after writing instead of freeing its tables.

## Techniques

What limits this track is waiting on loads whose address is random, and the uops of the token
path. One random load per corpus token goes into the term table; at each document's end come the
entry ranges, entries and answer states of the queries its terms trigger. Each technique below
names the cost it attacks, what the code does, and why the output stays the same.

| Technique | Bottleneck | What the design does | Why the output is unchanged |
|---|---|---|---|
| No index; queries first | An index of the query terms holds 21M (term, document) pairs: 85 to 200 MB, past the 44 MB credit (steps 1 to 5) | All 50,000 queries are read first and answered during one parallel scan; nothing grows with the corpus | Every query is evaluated on every document that can match it (covers, below), with the spec's set operations |
| Perfect hash, AES-hashed | One random probe per token; the old 4-way buckets compared four keys and spilled into the next bucket | PTHash-style pilots give every term of up to 12 bytes its own 16-byte slot; the hash is two `aesenc` rounds; the probe is one load, an XOR, a `vptest` and a `cmov`. A Bloom filter and a small hot table in front of the term table (step 4) lost once every lookup was prefetched (step 8, test server 139 → 128 ms without them), and a 64 KB one-word filter (step 24) did not move the box (0.1443 against 0.1459 s) for 19 more lines | The probe compares the token's whole key with the slot's; the hash only says where to look. A failed build re-seeds; it never yields a table that answers wrongly |
| Lock-free parallel hash build | Sorting, deduplicating and placing 155,000 term keys takes about 10 ms on one thread of the test server | Pilots are 1 mod 64, so each of 64 hash groups owns a region of the table, and 12 threads place their groups without locks | Groups, bucket order and ids do not depend on thread timing; each thread writes only its regions before the barrier |
| SIMD tokenizer, prefetched lookups | 36.3M tokens to find and look up | AVX2 classifies 64 bytes into token and LF bitmaps; windows end at a non-token byte, so starts and lengths are bit operations; each slot is prefetched when its token is found, and a document's tokens are all found before any is probed, each kept in 16 bytes (the slot's offset over the key's top 4 bytes) | A token is exactly a maximal run of `[A-Za-z0-9]`; the key is its bytes OR 0x20, masked to its length; the probe compares only the 12 key bytes, not the offset |
| Truth tables per document | A BOOL query's left-to-right formula over up to 6 terms, 27.2 entries per document on seed 1 | The plan folds each (query, cover term) into a 32-bit table over the other terms' presence; an evaluation is 5 stamp compares and one bit of the table, 8 entries at a time | Bit a of the table is the spec's left-to-right evaluation with exactly those terms present; the earlier-cover rule makes one entry count each document |
| One-byte stamps | A per-thread record of each term's last document, written for every token | The stamp is the document's id mod 127, reset every 127 documents: 75 KB per thread instead of 303 KB of u32 | An equal stamp can only be this document's: within a window each stamp belongs to one document, and the reset value matches none |
| Presence stores and lists, 8 at a time | Stores commit in order, so a stamp store that misses L1 holds up the stores after it (store buffer full in 168M of 1,620M cycles, about 119M of them from that store); per token, two scalar list stores and 3 uops for its id and row | A second pass over the document's ids makes 8 tokens' ids (`vpand`) and rows (`vpminud`) at once, fetches each stamp's line for writing (`prefetchw`) just before its store, and left-packs the trigger and anchor lists 8 lanes per store (`vmovmskps`, a lane table, `vpermd`) | The same stores and the same lists in the same order; a prefetch changes no data; `vpminud` is `id.min(rows)`, and every reader of the token words masks them with ID; the 8 `miss` ids after the tokens have no flag and go to the sinks |
| Superblock rows side by side | `end_super` built each term's 256 bits from four loads on four lines 72 KB apart (rows kept by block from step 56) | A row's four words side by side (word b of row r at `4 * r + b`): one 32-byte load per term; the presence store's offset is one shift per 8 tokens | The same words with the same bits, only placed row by row; the load puts block b in lane b |
| Cover triggers from a sample | 121M evaluations if every query ran on every 64-document block | From a 2.5 MB sample, each query gets a cover, a set of terms every match contains; it is evaluated only in documents with a cover term; queries with common covers run per 256 documents with AVX2 | A document without a cover term holds no match. The sample only decides where a query is evaluated, never its result |
| Eight entries at a time | Per entry, five scalar stamp loads and a write of the answer state whether it matched or not; a loop per range that mispredicted | Each fresh trigger term's entry indices go 8 per store into one list; each 8 16-byte entries are loaded and transposed, and their terms' stamps gathered (5 AVX2 gathers); the matches' answers are left-packed (a 256-entry lane table, `vpermd`) and only they update their answers | Each lane is the scalar evaluation; the last batch's spare lanes hold a padding entry whose table is 0; prefetches read nothing the program uses |
| Popcount sums and first ids | Count, id sum and ten smallest ids without a loop over documents | Per matching entry, `state += 1 << 40 \| doc`; per superblock, count = popcount and sum = `base * popcount(w) + sum_k 2^k * popcount(w & M_k)`, AVX2 nibble tables and `psadbw` byte sums for 256 documents; first ids by trailing-zero counts while fewer than ten | Integer identities; per-thread sums below 2^39 (checked) |
| Locality by numbering | Hot stamps, rows and answer state scattered over cache lines | Terms renumbered by kind and sampled frequency; answers numbered in the order the scan updates them | Both are bijections (`answer_of`, `line_of`); they change where state lives, not what it holds |
| Phrases on their rarest term | Checking 19,000 phrases per document | Each phrase is anchored on its rarest sampled term, as a 64-byte record checked by one masked 32-byte compare against the document's token ids while they are in L1 | A phrase match contains its anchor, so every match is checked; a document is counted once |
| Setup without page faults | Once setup ran in parallel, its threads mostly waited on page faults of fresh, growing vectors (on the test server the setup spent more CPU time in the kernel than in the program, 34.6 of 58 ms) | Compact pre-sized records, grouping by counting sort, a stable counting sort for the term order from 4-byte keys; freed memory kept in the heaps | Records reload each key from the same file bytes; counting sorts are permutations, which change the order terms are numbered in, never what a term is |
| Measured on the box | Where the box spends its time cannot be profiled there | Exit-code probes, in-process A/B probes and, from Sun 13:20 UTC, a replica of the box (PERF.md, "How we measured") | Measurement only; never pushed as a scored version |
| Build | With PGO, builds of one source ran 2.5 to 6% apart (steps 27, 32), and on step 81's code the PGO build, trained at the sample scale, ran 0.55 to 0.67 ms slower than a plain one | A profile-guided build trained on a full-scale corpus since step 93 (one plain release build in steps 81 to 92): fat LTO, `crt-static`, `target-cpu=native`; `cmov` in `probe` and `vpmovmskb` as inline asm where LLVM would branch or rebuild masks bit by bit. No loop or function alignment flags (PERF.md, steps 38 and 52's experiments) | Flags, the profile and instruction choice only |

What each technique measured, at the steps that brought it (PERF.md has every A/B):

| Technique | Step | Measured |
|---|---|---|
| No index; queries first | 6 | 0.355 s, 203 MB → 0.267 s, 38.4 MB (run-acc60580e004 → run-de33f090eda0) |
| Perfect hash, AES-hashed | 31, 9 | 0.1338 → 0.1199 s, −10.4% (run-ff1d9b72da23 → run-4a28a650e9ac); the `cmov` probe (step 9): test server 132 → 120 ms |
| Lock-free parallel hash build | 31, 58 | built with the hash in step 31; step 58's per-region retry and new seeds: replica +0.07% for the whole step, the same speed |
| SIMD tokenizer, prefetched lookups | 8 | 0.2658 → 0.199 s with the superblocks (run-94bcf1f00dfc → run-1a4ed151ee61); test server: prefetched lookups 151 → 139 ms |
| Truth tables per document | 49 | best runs 0.108491 → 0.097889 s (run-29fa59a9bf1f → run-104cca2cc2ac) |
| One-byte stamps | 52 | replica 95.141 → 91.852 ms (−3.46%, with `malloc_trim` dropped), part of step 52's −7.05% |
| Presence stores and lists, 8 at a time | 77 | replica −2.734 and −2.827 ms; 0.077853 → 0.074086 s (run-b86db6c1a285 → run-567ede8911b8) |
| Superblock rows side by side | 78 | replica −1.05 ms (seed 1), −1.10 ms (holdout 99); run-81452d9a12ae 0.072974 s |
| Cover triggers from a sample | 49, 70, 79 | 7.75 billion → 5.51M evaluations (seed 1, ALWAYS 0.30); a 2.5 MB sample against 1 MB −0.813 and −0.641 ms; ALWAYS 0.40 against 0.30 −0.45 to −0.48 ms |
| Eight entries at a time | 66, 85 | replica −2.8 to −3.3%; 0.085425 → 0.082048 s (run-a0a52b4578ea → run-b16bec586dd7); loads and a transpose instead of gathers (step 85) −0.673 ms |
| Popcount sums and first ids | 84 | `psadbw` byte sums: replica −0.580 ms (seed 1), −0.609 ms (holdout 99); run-707010fe44c5 0.069276 s |
| Locality by numbering | 23 | test server −3.4%, L2 misses −10%; its one board run was disturbed |
| Phrases on their rarest term | 59 | one masked compare per phrase: replica −1.049 ms (30 of 31) |
| Setup without page faults | 15-16, 33, 55, 67-70 | 0.1713 → 0.1619 s (run-058dad4dcdce → run-3682cf4f995c); compact records (step 33): test server setup 16.8 → 11.6 ms; the pool −2.23%; steps 67-70 replica about −3.6 ms |
| Build | 81, 93 | plain against sample-trained PGO: replica 0.55 to 0.67 ms faster (0.86 to 1.00 ms with step 81's tail releases); run-88a51030508d 0.072172 s; PGO trained at full scale against plain (step 93): replica −0.414 ms (seed 1), −0.383 ms (holdout 99) |

### Designs replaced

| Replaced | By | Measured |
|---|---|---|
| An inverted index (steps 1-5) | no index, one scan (step 6) | 0.355 s, 203 MB → 0.267 s, 38.4 MB |
| Bloom filter and hot table (step 4) | one prefetched table (step 8) | test server 139 → 128 ms |
| A 4-way bucket table | the perfect hash (step 31) | 0.1338 → 0.1199 s |
| LLVM's branch in the probe | an inline-asm `cmov` (step 9) | test server 132 → 120 ms |
| The 64-document block engine (steps 6-48) | per-document truth tables (step 49) | 0.108491 → 0.097889 s |
| One thread per P-core (step 14) | two per core | step 14 lost 7%, reverted |
| PGO trained at the sample scale (steps 5-80) | one plain build (step 81) | replica 0.55 to 0.67 ms faster (0.86 to 1.00 ms with step 81's tail releases) |
| One plain build (steps 81-92) | PGO trained at full scale (step 93) | replica −0.414 ms (seed 1), −0.383 ms (holdout 99) |
| A byte loop for the query lines and another for their words | one AVX2 pass over spaces and LFs (step 94) | the parse −0.42 ms; replica −0.316 ms over 10 inputs |
| simdjson-style branch-free extraction | the branchy loop | replica +1.45 to +1.73 ms, won 1 to 3 of 31 |
| Two scans interleaved per thread | one | replica +10.535 ms (+12.32%) |
| A 64 KB one-word filter (step 24) | none | box 0.1443 against 0.1459 s, reverted |

### Index

One line per technique: what, where in the code, and why the output stays exact. The first table
above groups them by bottleneck. The code is in six files: `main.rs` (`main`, the slices, the
results and their formatting), `pool.rs` (`par`, the workers, `wait`, `ahead`), `vocab.rs` (the query split
and the perfect hash: `split_lines`, `Vocab`, `Phf`, `Phf::fill`, `hash`, `probe`), `plan.rs`
(`Plan`, `DocEntry`, `records`, `classify`, `cover`, `csr`) and `scan.rs` (`Scan` with `look_up`,
`end_doc`, `trigger` and `end_super`, `Answers`, `count_positions`), with the system calls in
`mmap.rs`.

1. No index: queries read first, all answered in one scan (`main`). Exact: every query is evaluated on every document that can match it.
2. Memory-mapped files with a 256-byte zero pad (`mmap::map`): full-width loads at any offset. Exact: zero is a separator, never a token byte.
3. Transparent huge pages off, malloc's mmap threshold fixed at 1 MB, and its trim threshold at 1 GB, so freed memory stays in the heaps (`mmap::set_up_memory`): a stable memory peak, and no trim flushes the TLB of every CPU the threads ran on. Exact: no effect on the bytes. Measured: with THP left on, step 49's code ran 74.40 → 73.60 ms on the test server, but `memory.peak` rose from 35.30-38.41 to 45.20-47.24 MB, past the 44 MB credit; THP for the setup tables only was neutral on the block engine (80.72 against 80.35 ms) (PERF.md, experiments).
4. Slices of whole lines, one per thread (`slices`, `line_after`). Exact: every cut follows an LF, so each document is in one slice, and each slice's last id must meet the next slice's first.
5. Query lines split in parallel into pre-sized 8- and 16-byte records (`split_lines`), the spaces and LFs of 32 bytes found by two AVX2 compares, each set bit ending a word (step 94). Exact: each key is reloaded from its own bytes, and every word ends at its own space or LF.
6. Operator from the low two bits of its first byte (the bytes `A`, `N` and `O` end in binary 01, 10 and 11; `split_lines`). Exact: the grammar has only these three words.
7. Stable counting sort `csr` for every grouping (keys by group, entries, anchors), its run ends from a fold whose sum stays in a register, written from each run's end into a vector's spare capacity, last value first. Exact: a permutation, and every slot is written once (the counts add up to the length).
8. PTHash-style perfect hash with per-bucket pilots (`Vocab`, `place`): one slot per term. Exact: the hash says where to look; the probe decides (16).
9. Two AES rounds as the hash, the seed (a × `SEEDS` for attempt a) in the first round key (`hash`): three instructions per token. Exact: a weak or clashing hash only fails the build, which re-seeds (13).
10. Pilots of 1 mod 64, so each of 64 groups owns a table region: a lock-free build on every thread (`Vocab::new`). Exact: no thread writes another's region; the barrier orders the read-back.
11. Largest buckets placed first, first free pilot taken (`Phf::fill`, `find_pilot`). Exact: a fixed order, so the same table on every run at one thread count (another count groups the buckets differently and builds another table, which answers the same).
12. Keys grouped by bucket with `csr`, then each bucket's keys sorted by hash and one kept per hash (`dedup_keys`). Exact: the grouping and the sort only reorder the keys; two different keys with one hash are caught by the read-back (13).
13. A read-back of every term's slot after the barrier (`Vocab::new`): a slot that holds another key means two keys share a hash. A bucket with no free pilot goes first in its thread's next try, up to four tries (`Phf::fill`); a clash or a fourth failure re-seeds, and every third seed doubles the table. Exact: a failed build never answers, and a key that a failed try left in the table matches no token (its own term looks up its final slot).
14. Terms of 13 to 32 bytes in a cold `HashMap`, with their slots after the hash's (`get_long`). Exact: compared byte for byte; tokens over 32 bytes find nothing.
15. 16-byte slot: 12 key bytes under a 4-byte info word (id, ANCHOR, TRIG). Exact: 12 bytes are the whole key of every term the hash holds.
16. Branch-free probe: one aligned load, XOR, `vptest`, inline-asm `cmov` of the info word, loaded on its own (`probe`). Exact: the full key is compared; the hash only says where.
17. Token key from one 16-byte load, OR 0x20 and a sliding `KEEP` mask (`key_at`). Exact: lowercases A to Z, keeps digits, zero padding separates lengths.
18. AVX2 token and LF bitmaps per 64 bytes, signed compares, letters tested on `v | 0x20` (`masks`). Exact: a token byte is exactly `[A-Za-z0-9]`.
19. `vpmovmskb` as inline asm (`movemask`). Exact: the same instruction, without LLVM's bit-by-bit rebuild.
20. Slice end clipped with `bzhi` (`masks`). Exact: bytes past the slice are not tokens here.
21. Windows that end at their last non-token byte, so no token crosses one: token starts `mask & !(mask << 1)` clipped there with `bzhi`, no carry (`Scan::run`); tokens of 63 bytes or more on a cold path (`long_run`). Exact: every token is scanned whole, in one window.
22. Token length from the start and last-byte masks, one `tzcnt` each. Exact: the distance from a token's first byte to its last.
23. Slot's byte offset computed and prefetched when the token is found; a document's tokens all found before any is probed (`pending`, `look_up`): the misses overlap. A found token is 16 bytes, its key with the offset written over the key's top 4 bytes by a second 4-byte store. Exact: a prefetch reads nothing the program uses, and the probe compares only the 12 key bytes.
24. Writes into reserved capacity through raw pointers, no capacity checks (`pending`, `toks`, `trig`, `anchors`; `trigger`'s `evals` and `hits`, as long as the entries). Exact: each loop reserves room for the most it can write (for example `reserve(32)` per 64-byte window, and the document's tokens plus 8 for `look_up`'s lists, whose stores write 8 lanes), and `trigger` writes each real entry at most once plus 8 lanes, as their SAFETY comments argue.
25. One-byte stamp of each term's last document, written for every token, its line fetched for writing (`prefetchw`) just before the store, so that a miss does not hold up the stores after it (`lastdoc`, `STAMPS`, `look_up`). Exact: an equal stamp is always this document's (design 3); a prefetch changes no data.
26. Every trigger occurrence and anchor position kept without a branch, left-packed 8 lanes per store (`look_up`, `left_pack`), and each trigger term taken once per document by its DONE mark (`trigger`). Exact: `left_pack` keeps exactly the lanes whose flag is set, in order, and the 8 `miss` ids after the tokens have no flag; the first occurrence keeps its range, and later ones are written over.
27. Non-row tokens all land on one sink row: the row index is `id.min(rows)`, made for 8 tokens at once by `vpminud` (`look_up`), so there is no branch on the kind. Exact: the sink is no query's row, and `vpminud`'s unsigned minimum is `id.min(rows)`.
28. Cover sets: a set of terms every match contains, cheapest by sampled frequency (`cover`). Exact: a document without a cover term holds no match.
29. Cost model `1 - (1 - df / n)^64`, the share of 64-document blocks that contain a term (`Plan::with`), kept from the block engine of steps 6 to 48. Every cover term of a triggered query has a share under ALWAYS (0.40), so df / n is under 0.8%, where this share is within about 21% of 64 · df / n, the per-document cost: covers rank nearly as they would by document frequency, and ALWAYS was swept on this scale (step 79). Exact: it only chooses the path a query takes.
30. A 2.5 MB sample spread over the slices for document frequencies (`SAMPLE_BYTES`), terms counted once per document by stamps. Exact: it only decides where a query is evaluated.
31. Truth tables per (query, cover term), made once at planning time (`doc_entry`). Exact: bit a is the spec's left-to-right fold of AND, AND NOT and OR with exactly those terms present.
32. The earlier-cover rule inside the table: zero where a cover term before c is present. Exact: each matching document is counted by the entry of its first present cover term, once.
33. Entries padded to 5 terms with term 0 (`DocEntry`, 16 bytes). Exact: the table is built over the k real terms, and its bits from 2^k on repeat the first 2^k, so the padding term's presence lands on the same bit.
34. The triggered entries evaluated 8 at a time (`trigger`): indices stored 8 per vector, 8 aligned loads and a transpose per 8 entries and 5 stamp gathers, the matches' answers left-packed by `left_pack` (lane numbers from `PACK`, then `vpermd`), and only they added; 16-byte entries (`DocEntry`), 16-bit group starts, and 8 padding entries that never match. Exact: each lane is the scalar evaluation, and every index read is a real or padding entry (design 3; "Why the output is exact").
35. Common queries per 256-document superblock, one 256-bit AVX2 row per term, its four words side by side (`wide`, `end_super`, `ALWAYS`). Exact: bit i of a row is "document base + i contains the term", so the word operations are the spec's set operations.
36. Superblock queries sorted by length and operators so the operator branch predicts. Exact: the order of evaluation only.
37. A term's four block words side by side, word b of row r at `4 * r + b`, read by `end_super` in one 32-byte load (`wide`); the presence store's offset is `4 * id.min(rows)`, one shift per 8 tokens. Exact: the same words, one bit per document, and the load puts block b in lane b.
38. Terms renumbered by kind and sampled frequency: hot stamps and rows share cache lines (`term_order`). Exact: a bijection.
39. Answers numbered in the order the scan updates them (`number`, called in `Plan::with`; `answer_of`, `line_of`). Exact: a bijection.
40. Per-document count and sum as one packed add per match, `1 << 40 | doc` (`Answers::add_doc`). Exact: an integer identity.
41. Superblock count by popcount; id sum as `base * popcount + sum_k 2^k * popcount(w & M_k)` (`Answers::add`). Exact: an integer identity.
42. The six masks `M_k` computed as constants by one expression (`digit`). Exact: `M_k` has bit i set exactly when bit k of i is set.
43. AVX2 nibble tables (`pshufb`) for count and position sums of 256 documents, summed per 64-bit word by `psadbw`, with one `vpmuludq` for the base (`count_positions`: one `maddubs` weights each byte's count by its place; `madd` until step 84). Exact: the same integer sums, and no partial sum leaves its lane.
44. First ids written branch-free, 4 bytes each, and kept only while fewer than ten (`add`, `add_doc`). Exact: documents are scanned in order and bits come out in increasing order, so the first ten logged are the ten smallest of the slice; an id is rebuilt from its low 16 bits and its slice's first id, exact while a slice holds at most 2^16 documents: at least 8 slices (`MIN_SLICES`), and `slices` cuts a range of more than 2^16 documents every 2^16 lines; an assert checks it.
45. Count and id sum packed in one `u64` (`SUM_BITS`), so threads' states add as plain integers. Exact: sums below 2^39, checked by an assert.
46. Phrases anchored on their rarest sampled term, as 64-byte records (`Phrase`), checked at document end while the ids are in L1 (`end_doc`). Exact: every match contains its anchor.
47. The anchors' positions listed by `look_up`, and each anchor's first record prefetched before the BOOL evaluation and read after it (`look_up`, `end_doc`). Exact: prefetches read nothing the program uses; the phrases update other answers than the BOOL entries, so the order does not matter.
48. One masked 32-byte compare per phrase, 8 ids from its start (`end_doc`). Exact: `bzhi` keeps exactly the phrase's terms in the compare mask, and the 8 `miss` ids that `look_up` writes after the document's tokens match no term, so a phrase never matches past the document's end.
49. Per-phrase last-document marker, so a document counts once. Exact: a second match in the same document is not counted.
50. Answer states and superblock rows as plain zeroed vectors (`Scan::new`): the heaps hand out memory the setup freed (item 3), which calloc clears with stores, so no page is read before it is written. Writing the zeros by hand (step 13's fix, `written_zeros` until step 90) took the same page faults, about 10,900 on seed 1. Exact: the state starts at zero either way.
51. The corpus, every slice's answer states, line ends and first ids, the hash table and the plan's phrase records given back to the kernel in shares by the workers (the plan's 0.6 MB of entries left to the exit since step 91: about 0.03 ms, under its line's price), and each output chunk by the main thread once written (`mmap::release`, in the last `par` round of `main`); each slice's first-id log by its thread once merged (`MADV_DONTNEED`, its empty mapping left to the exit). Exact: the output is formatted first, and none of them is read again.
52. First-id logs put in line order per thread by a prefix sum of the logged counts and one scatter, no sort (`main`); merged in slice order; the later threads skip the answers whose first slice alone has ten (`full`). Exact: each answer logs its ids in order, and slices are in document order.
53. Output formatted in parallel, states read in place through `answer_of` (`format_part`; no 4.5 MB copy). Exact: each line reads its own states.
54. Exit right after writing, no frees. Exact: results.txt is complete before the exit.
55. Slice edges need no special case. Exact: a document outside a thread's slice has no stamps and no row bits there.
56. A profile-guided build trained at full scale since step 93 (Dockerfile): an instrumented build runs once on a full-scale corpus of generator seed 7 (`b_index.py`), then the profile-use build: −0.414 and −0.383 ms against step 92's plain release build (seed 1 and holdout 99), and a second PGO build −0.400 ms (seed 1). PGO builds of one source are not the same binary (two ran 0.419 ms apart), so the board's build is laid out by its own training run. Steps 81 to 92 built without a profile: on step 81's code the build of steps 5 to 80, trained on a sample-scale run, ran 0.55 to 0.67 ms slower. Items 56 and 57 change speed only: every check runs a build of the Dockerfile.
57. No alignment flags (the replica: `-align-loops=64` +0.24%, `-align-all-functions=6` 0.00%); fat LTO, one codegen unit, `panic = "abort"`, `crt-static`, `target-cpu=native`, `compile_error!` without AVX2, BMI2 or AES.
58. Exit-code and in-process timing probes on the box (PERF.md, "Box probes since step 47"). Measurement only, never a scored version.
59. No dependencies: libc calls declared by hand.
60. One persistent pool of threads behind `par` (`worker`, `JOB`, `GENERATION`, `FINISHED`, `BUSY`): the first call spawns the workers, and each later call publishes a job and waits for every worker to count it done; an assert allows one call at a time. A waiting worker maps the corpus in ahead of the scan, one read per 64 KB (`ahead`). Exact: `par` returns only after every part is done, and no worker uses the job after the call returns (its SAFETY comment).
61. Workers wait in `umwait` on the generation word (`wait`), woken by a write to its cache line or by the kernel's time limit; without WAITPKG, a polling loop. Exact: a wait ends only when the word shows the next job.
62. std's start-up skipped (`#![no_main]`, our own C `main`, as in Track A): no stack-guard lookup through /proc/self/maps, and no signal handlers or alternate signal stacks, in the spawned threads either: 66 fewer system calls at 8 CPUs, about 0.12 ms on the replica (PERF.md, step 89). The data paths are joined by `concat`, not `format!`, which kept the scan's hot functions at their 64-byte offsets in the plain builds of steps 89 to 92; under PGO (step 93) the profile lays out the code, and those offsets vary from build to build. Exact: nothing here recurses deeply or handles a signal; the arguments arrive as before (glibc passes them to std), and a panic still prints its message, then aborts (a panic on the main thread now names it `<unnamed>`).

## Why the output is exact

- **Tokens.** The key keeps exactly the token's bytes (a mask of its length) and ORs 0x20 into
  them, which lowercases A to Z and leaves digits unchanged. Token bytes are never zero, so the zero
  padding makes keys of different lengths different. Longer terms are compared byte for byte; a
  token over 32 bytes cannot be a term (the grammar caps terms at 32 bytes) and finds none.
- **The perfect hash decides where to look, never whether a token matches.**
  - *Lookup.* A token's slot is computed with the same seed, pilots and shift the build used, and
    the probe compares the token's full 12-byte key with the key stored there. The slot's offset,
    which `pending` keeps in the token's top 4 bytes, is outside the 12 bytes compared.
  - *Hits.* A query term therefore finds its own slot, which holds its key. The builder gave every
    distinct key its own slot: a slot is claimed once, and a bucket's keys are placed only when all
    their slots are free.
  - *Misses.* Any other token lands on an empty slot, whose zero key no token has, or on another
    term's slot, whose key differs: MISS either way.
  - *Failures.* A bad hash or seed can only make a placement or the build fail. A failed placement
    is tried again by its thread, and a failed build with another seed. A key that a failed try left
    in the table matches no token: its own term looks up its final slot, and every other token's
    key differs. So a failure cannot change a lookup.
  - *Determinism.* The groups, the bucket order (by size, with a deterministic sort) and the id
    numbering do not depend on thread timing, and each thread writes only its own regions of the
    table before the barrier.
- **BOOL, per document.**
  - *The table.* An entry for (query q, cover term c) lists q's other distinct terms t_0..t_{k-1},
    k ≤ 5. For each a below 2^k, bit a of its table is q's value, folded left to right with the
    spec's AND, NOT and OR, in a document that contains c and exactly the t_i whose bit i is set in
    a. It is 0 if a cover term before c is among them. Unused term slots hold term 0, and the
    table's bits from 2^k on repeat the first 2^k.
  - *The lookup.* At a document's end, bit i of the presence index is "the stamp of t_i is this
    document's". So the looked-up bit is q's value in this document, unless an earlier cover term is
    present. A padded slot's bit, whatever term 0's presence, only moves the lookup between copies
    of the first 2^k bits, so it lands on the same value.
  - *Each document once.* Every match of q contains at least one cover term, and the entry of the
    first one present (in cover order) counts it; every other entry's table is 0 there. A document
    with no cover term holds no match. A trigger term that occurs several times in a document is
    taken once: the first occurrence marks it DONE.
- **Stamps.** A document's stamp is its id mod 127, and all stamps are reset to 0xFF every 127
  documents (the slice's first window starts reset). Within a window each stamp value belongs to
  one document, so a term's stamp equals this document's only if the term occurred in it. 0xFF,
  with the DONE bit masked off, is 127, which no stamp equals.
- **Presence and lists, 8 at a time.** `look_up`'s second pass makes the stores the per-token loop
  made, each token's stamp and row bit, and a prefetch changes no data. Its rows come from
  `vpminud`, the unsigned minimum, which is `id.min(rows)`. It writes each token's id over its info
  word, but packs the lists from the info words loaded before, and `end_doc` masks every token
  word it reads with ID anyway (in the sample, the info words are the ids). `left_pack` keeps exactly
  the lanes whose flag bit is set, in order, so the trigger and anchor lists hold what the scalar
  code kept; spare lanes are overwritten by the next store or lie past the list's end, inside the
  8 extra entries each list reserves. The last 8 ids may include some of the 8 `miss` ids written
  after the tokens: `miss` has no flag, so no list keeps one, and its stamp and row bit go to
  `miss`'s own stamp and the sink row, which no evaluation reads.
- **Covers.** A document without any cover term of a query holds no match of it, so skipping it is
  exact.
- **Superblocks.** Bit i of a row word is "document base + i contains the term", so the word
  operations are the spec's set operations, applied in the query's order. The first term is ORed
  into zero, so a document of another slice, whose bits are 0 in this thread's rows, matches
  nothing here. A row's four words lie side by side, and `end_super`'s 32-byte load puts block b
  in lane b.
- **Eight entries at a time.** The 8 entries are loaded and transposed, so lane j holds entry j's
  words, as the gathers before step 85 did. Each lane computes what one entry's scalar evaluation did: the
  same truth table, read at the bit its 5 presence bits give, from the same stamps under the same
  DONE rule. A stamp is gathered as 4 bytes and only its low 7 bits are kept, so the 3 bytes after
  it and the DONE bit never count, and EMPTY (0xFF) reads as 127, which no stamp equals. Every
  index read is a real entry or a padding one: fresh terms are distinct, so their ranges are
  disjoint and the list holds each real entry at most once; a store's spare lanes are overwritten
  by the next store, and after the last range by the last padding entry, whose table is 0, so it
  never matches. Each answer matches at most once per document (the earlier-cover rule), and the
  matches are added in document order, so the first ids are logged in order. Prefetches read
  nothing the program uses.
- **Sixteen-bit plan indices.** Terms, answers and group starts are 16 bits in the trigger's
  entries and `trig_start`. The spec's inputs need about 53,000 BOOL terms, 50,000 lines and 36,000
  entries (seed 1: 53,444, 50,000 and 36,174). Past 2^16 BOOL terms or entries, `Plan::new` plans
  again with every cover's cost raised over ALWAYS, so every BOOL query is evaluated per
  superblock, which is exact for any query (forced on seed 1: 0.35 s); nothing wraps. Past 2^16
  lines the program stops (below).
- **First ids in 4 bytes.** A log entry is `answer << 16 | id & 0xFFFF`. An id is rebuilt as
  `first + ((entry − first) & 0xFFFF)`, where `first` is its slice's first id: exact whenever
  0 ≤ id − first < 2^16. The scan only ever logs its own slice's documents (other slices' bits are
  0 in its rows and stamps), and an assert after each scan checks that its last id is below
  first + 2^16 (at most 2^16 documents); one after `Plan::new` checks that there are at most 2^16
  answers. `slices` cuts any range of more than 2^16 documents every 2^16 lines, so a slice holds
  at most 2^16 documents whatever the line lengths; at least 8 slices (`MIN_SLICES`) keep a slice
  near 31,000 of the spec's 250,000 documents. More than 2^16 queries stop the program; no input
  gets a wrong id.
- **Phrases past the document's end.** The compare always reads 8 ids from the phrase's start.
  Past the document's last token they are the 8 `miss` ids `look_up` writes there, which match no
  phrase term.
- **The pool.** `par` returns only after every part has stored its result, and an assert allows
  one call at a time (`BUSY`), since its job slot and counters serve one job. A waiting worker's
  reads of the corpus (`ahead`) only map pages in; their values are discarded. The later scans' skip
  of `full` answers waits for the first slice's scan, which runs first on the calling thread, so
  the wait always ends. Every result is the same as with one thread per part (the gate runs 1, 3,
  7 and 12 CPUs).
- **Answer numbers** are a bijection. Every query line is a phrase, an `always` query, or has at
  least one entry (a cover has at least one term), and each line gets a number the first time it
  is met. So `answer_of` and `line_of` are inverse, and every output line reads its own state and
  log entries.
- **The sample** only decides where each query is evaluated (per document or per superblock, and
  which anchor), never its result. Its size (2.5 MB) and ALWAYS are speed choices.
- **Setup records.** A key record holds the offset and length of the term's bytes, and the key is
  loaded again from those bytes whenever it is needed, so the hash, the equal-hash check and the
  stored key are those of the term's own bytes. `csr` groups and the counting sorts are stable
  permutations; they decide the order in which terms and entries are numbered, which only moves
  state around.
- **Sums fit.** Each thread packs its count and its id sum (below N^2 / 2 < 2^39 for N < 2^20,
  which the program checks) into one u64.
- **Checks** before every push, listed in PERF.md ("How the final number was verified"):
  - the seed-1 hash at several thread counts;
  - byte comparison with the reference on sample seeds 2 to 4 and holdout seed 99;
  - 144 tiny, fresh and adversarial runs: token lengths 1 to 70, 1-term phrases, empty answers,
    and the tiny inputs (1,000 queries).

## Robust on unseen seeds

Nothing is fitted to seed 1's data except the ALWAYS threshold (PERF.md, steps 40, 44, 53 and 79,
and the sweeps on step 66's and step 78's code in the experiments):
the plan is made per input, from a 2.5 MB sample of that input's own documents. What could change with the seed, and why it does not change the answer:

| What could change | What the program does |
|---|---|
| Term frequencies | The sample decides each query's path and anchor, never its result (covers, design 3) |
| The query terms | The perfect hash is built per input; a bucket with no free pilot is placed again by its thread, a clash re-seeds, every third seed doubles the table, and the probe always compares the whole key. Over 300 hash seeds on each of 32 datasets (9,600 builds), no build needed a second seed; before step 58, 257 of 9,600 did (PERF.md, step 58) |
| Document count and ids | Checked: ids 0, 1, 2, ... below 2^20, and sums below 2^39 (an assert); any line lengths, since a slice is cut at 2^16 documents |
| Plan sizes | Past 2^16 BOOL terms or entries, every BOOL query is evaluated per superblock: exact for any query (forced on seed 1: 0.35 s). Past 2^16 lines the program stops |
| Token lengths | Terms of 13 to 32 bytes go to the long map; longer tokens cannot be terms |

Evidence, on builds of the Dockerfile:

- **Holdout seed 99**, full scale, byte-equal to the reference at every gate since step 33,
  including the submitted code's (PERF.md, "How the final number was verified").
- **Fresh seeds**, generated for the gates and compared with `reference/b_index.py` at 1, 3 and 8
  CPUs: tiny 801-806 and 911-914, and sample 51 and 52 (step 49); earlier tiny 701-706 and sample
  41-43.
- **Hand-made edge inputs**, in the gate since steps 62 and 63, at 1, 3 and 12 CPUs, equal to the
  reference: only phrases, only `always` queries, one BOOL query, empty documents at the start,
  middle and end and every 256th, all documents empty, one document; and five skewed corpora
  (250,000 documents with 80,000 empty ones first, last or in the middle, no trailing LF, 300,000
  tiny documents), on which step 62 aborted in 9 of 15 runs.
- **Robustness runs on the replica** (Sun 1 Nov, about 17:00 UTC), on the exact Dockerfile build of
  step 59 (steps 60 to 65 change lines, checks, the slicing of skewed corpora and the mapping of
  the corpus, not the output on generated inputs, and passed the gate; step 66 changes the BOOL
  evaluation itself, so its evaluation code, before the 6-term check was added, was run again on
  fresh full-scale seeds 101-110, 14 of 14 runs exact, and in 200 repeats; steps 62 and 63 ran the
  fresh-seed, repeat, tiny/sample and edge suites again, 1,426 runs each, all exact; steps 67 to 75
  change the setup, the page releases at the end and how `look_up` keeps anchor positions, and
  passed the gate, but of seeds 101-130 only 109 ran again, in step 70's A/B, with the same output
  as step 69's; steps 77 to 92 and 94 ran all 30 again, below), with data generated on the replica by the
  organisers' generator (Python 3.11, which
  reproduces the official sample seed 1 and its reference output byte for byte). Every run was
  byte-exact, with no crash, no nonzero exit and no output on stderr:
  - 30 fresh full-scale seeds (101-130) at 12 threads, seeds 101-102 also at 1 and 3 CPUs and
    seeds 103-104 at 2, 7 and 11: 40 of 40 runs;
  - 20 sha256-checked repeats per seed at 12 threads, and runs with a busy loop on 1, 2 or 4 of
    the twelve CPUs (seeds 109, 110 and 113): 660 of 660;
  - tiny and sample seeds 201-210 at 1, 2, 3, 7, 12 and 20 CPUs, each run as root and as uid
    65534 (the board's privilege drop) with read-only data, 3 repeats, and full scale at 12 and
    20 CPUs for both users: 840 of 840;
  - copies of seed 101 whose every file is an exact multiple of 4096 bytes, so that a read past
    the end of a file would fault, at 1, 3, 7, 12 and 20 CPUs: 5 of 5.
- **The submitted code's gate** (step 95, on the replica, Mon 2 Nov): two PGO builds of the
  Dockerfile, each 14 of 14 checks (full seed 1 at 1, 3, 7 and 12 CPUs; holdout 99 and samples 1
  to 4 at 12 CPUs and 1), and the gate script's 144 runs on the second: every output byte-equal to
  the reference. Memory, max of 5 runs, 31.62 and 31.53 MB.
- **Points on unseen seeds** (step 94's code, two PGO builds, 18 fresh full seeds): 81.97 points
  on average by the final's rule (std 0.08, worst 81.72, best 82.15), T 65.09 to 66.62 ms, every
  seed one output on every run, three checked against the organisers' reference. One busy CPU, not
  the seed, is the risk: 124.5 to 144.3 ms on seed 1 with a busy loop on one of the 12 CPUs
  (PERF.md, "How the final number was verified").
- **Step 94's final gate** (on the replica, Mon 2 Nov, one PGO build of the
  Dockerfile, 8ce12e1e190c), every output
  byte-equal to the reference: fresh full seeds 101-130 at 1, 3, 7 and 12 CPUs (120 of 120 runs);
  tiny and sample seeds 201-210 as root and as uid 65534 at 1, 3, 12 and 20 CPUs (160 of 160);
  holdout 99 at nine CPU counts from 1 to 20 (9 of 9); 20 repeats of seed 101 at 12 and at 20 CPUs
  (40 of 40); 18 board-like edge cases at 1 and 12 CPUs as both users (72 of 72); and the gate's
  144 runs and 66 hand-made edge runs. Memory, max of 20 runs, 31.65 MB. Steps 77 to 92 passed
  the same gate, with 30.91 to 32.78 MB, and step 93's build was checked on full seed 1, holdout 99
  and samples 2 to 4 (PERF.md, "How the final number was verified").
- **The perfect hash, forced.** The per-region retry ran end to end from forced start seeds: in
  300 runs, 20 placed a region again, and all were exact.
- **Time and memory on other seeds.** On the 30 fresh seeds step 59 ran 84.73 to 88.18 ms (medians
  of 7 runs; seed 1: 85.47 ms), and its memory peak on seeds 101-110 was 30.11 to 31.63 MB (max of
  5 runs; seed 1: 30.03 MB), under the 44 MB of full credit. Earlier, the block engine of step 46b
  ran three full-scale seeds within 0.5% on the test server (PERF.md).
- **Not covered:** more than 20 threads (the replica has 20 CPUs), and the table-doubling path,
  which now needs seeds 0, 1 and 2 all to fail, each by a clash or by a region that four
  placements could not fill.

## Error handling

Errors are panics with a message, and the build aborts on panic, so the exit status is nonzero:
- a missing file, or a failed mmap;
- a queries.txt that is empty or whose last line has no LF (spec 2.3: every line ends with LF),
  checked at the start of `parse_queries` (step 79). Before, an empty file stopped on an index out
  of bounds in `number_answers`, and a last line without LF lost its last byte with no error: a
  BOOL query the last letter of its last term, a phrase a letter and its closing quote;
- a queries.txt line without terms (an empty line, or `BOOL` and LF), checked in `split_lines`
  (step 94; before, it stopped on a slice index out of bounds);
- a docs.txt line without a TAB, or an id that is not all digits or is over 9 digits;
- document ids that are not 0, 1, 2, ... below 2^20, each before a TAB;
- a slice whose last id does not meet the next slice's first;
- more than 2^16 queries (the 4-byte first-id log's limit; the spec's inputs have 50,000);
- a phrase of over 8 terms (the spec's have 2 to 8);
- a BOOL query of over 6 terms (the spec's have 1 to 6), checked by an assert at the start of
  `cover`, the first function every BOOL line reaches;
- a second call to `par` while one runs (the program calls it from the main thread only).

A line that starts with a separator byte (an empty id, for example) has no id token: its text
joins the previous document. That is caught when it breaks the id sequence (the next line's id
check) or when it is the last line (`slices` parses the last line's id), but not when it is an
extra line inside the file, where the answer is silently wrong.

queries.txt is trusted to follow the spec's grammar (2.3): a malformed line either panics on a
slice bound or is answered as parsed (an unknown operator reads as the one its first byte's low
two bits pick: `XOR` as AND, `BUT` as NOT).

## Dependencies

None: the Rust standard library only, with `mmap`, `madvise`, `prctl` and `mallopt` declared by
hand (`dependency_count = 0`).
- *CPU.* The build needs AVX2, BMI2 and AES-NI (`-C target-cpu=native`; the board builds this
  track on the scoring box).
- *PGO, trained at full scale* (step 93): "Build and reproduce" above. `b_index.py`, the
  generator's copy that makes the training corpus, stays in the folder and is not counted as code.
- *No alignment flags.* B builds without loop or function alignment flags: on the replica they
  were 0 to 0.4% slower.

## With another 24 hours

Measured or modelled on seed 1; none of this was built.

- **The row ORs.** Every token sets its document's bit in its term's row, and 37% of the tokens go
  to one sink row: those writes cost about 10.7 ms of step 53's scan on the replica, and the
  presence stores, stamps and row ORs together, 5.27 ms of step 78's (non-PGO). Rotating sink
  rows, a branch for row terms and sending repeated terms to the sink all lost (PERF.md,
  experiments). It is the largest store cost left, and we have no design for it yet.
- **Setup still ends at 7.9 ms of 65.6** (step 86's phase marks on the replica, median of 9 runs:
  the split, the vocabulary, the sample and the plan). The plan alone was 5.7 ms on step 49.
  Tokenizing needs only the perfect hash, not the plan, so the scan could start as soon as the hash is built, buffering
  each thread's first documents until the plan is published. We estimated 3 to 4 ms; timing-only
  builds on step 88's code put its ceiling at about 0.6 to 0.75 ms (the idle window is about 1.9 ms
  a worker, and the pre-pass slows the plan), for about 65 to 70 lines and 5 MB, so it stays
  unbuilt (PERF.md, experiments). The lever they point at is the plan itself: 3.5 ms on every
  thread's critical path, 1.34 ms of it serial; step 90 took the plan from 3.609 to 2.685 ms, and
  step 91's running sums took about 0.09 ms more off the run (PERF.md, steps 90 and 91). What is
  left of the plan, by step 90's marks per worker (seed 1, medians of 9): `term_order`, 0.39 ms on
  one thread while eleven wait (counts chained on three hot keys, and the faults of a fresh
  300 KB array); the grouping round, about 1.0 ms on two threads (page faults on fresh outputs
  and random placement writes); and 0.15 ms on one thread after it, mostly the phrase numbering
  reading 1.2 MB from another core. `term_order` on every thread, with per-thread counts, is
  priced at about −0.24 ms for about 12 lines; not built.
- **The triggered random accesses.** Two random accesses are left in the per-document evaluation,
  priced by timing-only builds (wrong output) of an earlier version of step 66: sending every
  match's state update to one answer saved 3.13 ms, and reading every entry from entry 0 also
  3.13 ms. Both were tried since: adding the matches one document late, with their state lines
  prefetched, lost 1.095 ms, and a fake with hot states (−1.769 ms) showed that most of that cost
  is the first-id log, not the state misses (PERF.md, steps 83 to 86).
- **The mispredicts that remain** (replica counters on an earlier stage of step 52):
  - the extraction loop's exit, 4.0M (cheap, see PERF.md's failed branch-free extraction);
  - the ANCHOR branch on the probe's info word, 1.1M, which resolves late;
  - the superblock's first-id logging, 0.6M.

  The per-range entry loop's 1.0M went with step 66, and the ANCHOR branch with step 73: our
  first branch-free form (q14an) had measured 1.2 ms slower on step 52, step 73's form −0.455
  and −0.118 ms, and step 77 left-packs the anchors 8 at a time.
- **A denser perfect hash.** The table runs at a load of 0.58 (2 MB). Placing buckets at a load
  near 0.9, as PTHash does with more pilot candidates, would fit it in 1.25 MB of slots and cut
  the misses of the scan's one random load per token. The build would take longer, on twelve
  threads. Untested.
- **A huge page for the term table alone.** The 2 MB table takes the one random load per token.
  THP everywhere was 0.8 ms faster on the test server but over the memory credit; a huge page for
  this table alone, with THP still off for the sparse tables, was not tried.
