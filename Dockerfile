# syntax=docker/dockerfile:1.7
#
# Builds the ferra-server binary. Build context is the repo root (the whole
# workspace is needed because Cargo resolves all members at parse time).
#
#   docker build -t ghcr.io/deatherving/ferra:latest -f Dockerfile .

FROM rust:1-slim-bookworm AS builder
WORKDIR /build

RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Workspace manifest + every member's manifest must be present for cargo to
# resolve the workspace, even when only one bin is built.
COPY Cargo.toml Cargo.lock* ./
COPY server/Cargo.toml server/Cargo.toml
COPY agent/Cargo.toml  agent/Cargo.toml
COPY meta/Cargo.toml   meta/Cargo.toml
COPY server/src        server/src
COPY server/migrations server/migrations
COPY agent/src         agent/src
COPY meta/src          meta/src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --bin ferra-server \
 && cp target/release/ferra-server /usr/local/bin/ferra-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -r -u 10001 -m -s /usr/sbin/nologin ferra

COPY --from=builder /usr/local/bin/ferra-server /usr/local/bin/ferra-server

USER ferra
EXPOSE 8080
ENV FERRA_HTTP_ADDR=0.0.0.0:8080
ENTRYPOINT ["/usr/local/bin/ferra-server"]
