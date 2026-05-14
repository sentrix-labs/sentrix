# Multi-stage build for the Sentrix validator/node binary.
#
# - Build stage uses rust:1.95-bullseye to match the release.yml SLSA-attested
#   binary baseline (glibc 2.31). The release.yml workflow ships official
#   binaries; this Dockerfile produces the same artifact for users who prefer
#   running via docker.
# - Runtime is debian:bookworm-slim — Sentrix needs ca-certificates for TLS
#   (libp2p Noise handshake doesn't, but reqwest/RPC paths do) and curl for
#   the docker-compose healthcheck.
#
# Built + pushed by .github/workflows/docker-image.yml as
# ghcr.io/sentrix-labs/sentrix:<tag> on every release tag.

ARG RUST_VERSION=1.95
FROM rust:${RUST_VERSION}-bullseye AS builder

# Build deps the workspace needs:
# - libclang-dev + clang: mdbx-sys bindgen
# - protobuf-compiler:    sentrix-grpc + sentrix-grpc-wasm protoc
RUN apt-get update && apt-get install -y --no-install-recommends \
    libclang-dev \
    clang \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the full workspace. Cargo's dependency layer caching is fine without
# the dummy-src dance for our size — full rebuild is ~3 min, layer reuse
# saves seconds, complexity not worth it.
COPY . .

# Build the validator binary specifically — the workspace has 19 crates
# but we only ship `sentrix-node` (bin/sentrix) as the running validator.
# `-p sentrix-node` qualifier is required since the workspace refactor
# (#651-#663) split the binary out of the default-run set.
RUN cargo build --release --bin sentrix -p sentrix-node

# ── Runtime ────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /build/target/release/sentrix /usr/local/bin/sentrix

RUN mkdir -p /data
VOLUME ["/data"]
ENV SENTRIX_DATA_DIR=/data

# 8545 = JSON-RPC, 30303 = libp2p
EXPOSE 8545 30303

ENTRYPOINT ["/usr/local/bin/sentrix"]
CMD ["start"]
