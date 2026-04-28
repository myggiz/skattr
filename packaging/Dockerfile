# syntax=docker/dockerfile:1.6

# Build stage: pinned to the workspace toolchain.
FROM rust:1-slim-bookworm AS build
WORKDIR /work
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*
COPY rust-toolchain.toml ./
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY deny.toml ./deny.toml
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/work/target \
    cargo build --release -p skattr-mailbox && \
    cp /work/target/release/skattr-mailbox /usr/local/bin/skattr-mailbox

# Runtime: distroless cc, non-root.
FROM gcr.io/distroless/cc-debian12:nonroot
LABEL org.opencontainers.image.source="https://github.com/myggiz/skattr"
LABEL org.opencontainers.image.licenses="AGPL-3.0-or-later"
COPY --from=build /usr/local/bin/skattr-mailbox /usr/local/bin/skattr-mailbox
USER nonroot:nonroot
VOLUME ["/var/lib/skattr-mailbox"]
ENTRYPOINT ["/usr/local/bin/skattr-mailbox", "--config", "/etc/skattr-mailbox/mailbox.toml"]
