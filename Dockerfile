FROM rust:1.93.1-bookworm AS builder

WORKDIR /app

# Cache dependency layer – copy manifests first
COPY Cargo.toml Cargo.lock ./
COPY ingest-vestibule-retriever/Cargo.toml ingest-vestibule-retriever/Cargo.toml

# Create a dummy main so `cargo build` resolves + caches all deps
RUN mkdir -p ingest-vestibule-retriever/src \
    && echo "fn main() {}" > ingest-vestibule-retriever/src/main.rs \
    && cargo build --release --package ingest-vestibule-retriever \
    && rm -rf ingest-vestibule-retriever/src

# Copy real sources and rebuild (only the crate is recompiled)
COPY ingest-vestibule-retriever/src ingest-vestibule-retriever/src
RUN touch ingest-vestibule-retriever/src/main.rs \
    && cargo build --release --package ingest-vestibule-retriever

# ──-────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/ingest-vestibule-retriever /usr/local/bin/

ENTRYPOINT ["ingest-vestibule-retriever"]
