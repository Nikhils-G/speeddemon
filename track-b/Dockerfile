FROM halloumee/speed-demon-rust:1.99.1-trixie-llvm-tools@sha256:13e4ee7135d8eac6c56476c7d1aee201f36479f4745fd5d0e6b4529f4e13251a AS build
WORKDIR /src
COPY . .
# A plain release build, without profile-guided optimisation: on this code the
# profile-guided build ran 0.55-0.67 ms slower (two PGO builds against one plain
# build, on seed 1 and holdout seed 99: 26 to 30 of 31 pairs on the replica). The
# likely cause is its sample-scale training run, which no longer matches the
# hand-tuned scan. Without a profile every build gives the same binary, so the
# board's build cannot differ by layout. target-cpu=native makes the board build
# this track on the scoring box.
RUN RUSTFLAGS="-C target-cpu=native -C target-feature=+crt-static" cargo build --release --offline

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/target/release/solution /app/solution
ENTRYPOINT ["/app/solution"]
