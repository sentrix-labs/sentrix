# Stage 1 — Build
FROM rust:1.94-slim AS builder

WORKDIR /app

# Cache dependencies first
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -rf src

# Build actual source
COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release

# Stage 2 — Runtime (minimal)
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/sentrix .

RUN mkdir -p /data

EXPOSE 8545 30303

VOLUME ["/data"]

ENV SENTRIX_DATA_DIR=/data

ENTRYPOINT ["./sentrix"]
CMD ["start"]
