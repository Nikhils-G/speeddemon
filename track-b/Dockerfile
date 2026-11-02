FROM halloumee/speed-demon-rust:1.99.1-trixie-llvm-tools@sha256:13e4ee7135d8eac6c56476c7d1aee201f36479f4745fd5d0e6b4529f4e13251a AS build
WORKDIR /src
COPY . .
# Profile-guided optimisation trained at full scale. A byte-identical copy of the organisers'
# generator makes a full-size corpus (there is no /data at build time), an instrumented build
# runs on it, and the final build is laid out by the recorded profile. Build time is not scored.
# Trained on the sample scale, PGO ran 0.55-0.67 ms slower than a plain build (PERF.md, step
# 81): that profile no longer matched the hand-tuned scan. Trained at full scale it runs 0.38 to
# 0.41 ms faster than the plain build (replica: seed 1 and holdout 99, 25 and 26 of 31 pairs).
# The profile's counters are atomic, so the training run's threads lose no updates.
# target-cpu=native makes the board build this track on the scoring box.
RUN RUSTFLAGS="-C target-cpu=native -C target-feature=+crt-static -C profile-generate=/tmp/pgo -C llvm-args=-instrprof-atomic-counter-update-all" cargo build --release --offline \
 && python3 b_index.py --seed 7 --scale full --out /tmp/train > /dev/null && mkdir /tmp/out && target/release/solution /tmp/train /tmp/out && llvm-profdata merge -o /tmp/pgo.profdata /tmp/pgo \
 && RUSTFLAGS="-C target-cpu=native -C target-feature=+crt-static -C profile-use=/tmp/pgo.profdata" cargo build --release --offline

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/target/release/solution /app/solution
ENTRYPOINT ["/app/solution"]
