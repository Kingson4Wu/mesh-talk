# Build the node + relay binaries for Linux containers, to simulate SEPARATE machines on a LAN
# (each container = its own IP, real cross-host UDP discovery — not loopback). Two-stage: compile,
# then a slim runtime image.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p mesh-talk-core --bin mesh-talk-runtime-node \
        --features native,gateway,fast-test-kdf \
 && cargo build --release -p mesh-talk-signal

FROM debian:bookworm-slim
COPY --from=build /src/target/release/mesh-talk-runtime-node /usr/local/bin/runtime-node
COPY --from=build /src/target/release/mesh-talk-signal /usr/local/bin/signal
