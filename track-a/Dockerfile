FROM halloumee/speed-demon-rust:1.99.1-trixie-llvm-tools@sha256:13e4ee7135d8eac6c56476c7d1aee201f36479f4745fd5d0e6b4529f4e13251a AS build
WORKDIR /src
COPY . .
# Profile-guided optimisation. A byte-identical copy of the organisers' generator
# makes training data (there is no /data at build time), an instrumented build runs
# on it, and the final build is laid out by the recorded profile. Build time is not
# scored. target-cpu=native makes the board build this track on the scoring box.
RUN python3 a_ingest.py --seed 7 --scale sample --out /tmp/train > /dev/null \
 && RUSTFLAGS="-C target-cpu=native -C profile-generate=/tmp/pgo" cargo build --release --offline \
 && mkdir /tmp/out && target/release/solution /tmp/train /tmp/out \
 && llvm-profdata merge -o /tmp/pgo.profdata /tmp/pgo \
 && RUSTFLAGS="-C target-cpu=native -C profile-use=/tmp/pgo.profdata" cargo build --release --offline

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/target/release/solution /app/solution
ENTRYPOINT ["/app/solution"]
