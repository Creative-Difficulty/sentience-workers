FROM rust:1.93.1-bookworm AS chef
RUN cargo install cargo-chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --workspace

FROM debian:bookworm-slim AS runtime-base
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

FROM runtime-base AS fact-extractor
COPY --from=builder /app/target/release/fact-extractor /usr/local/bin/service
ENTRYPOINT ["/usr/local/bin/service"]

FROM runtime-base AS topic-sorter
COPY --from=builder /app/target/release/topic-sorter /usr/local/bin/service
ENTRYPOINT ["/usr/local/bin/service"]

FROM runtime-base AS ingest-vestibule-retriever
COPY --from=builder /app/target/release/ingest-vestibule-retriever /usr/local/bin/service
ENTRYPOINT ["/usr/local/bin/service"]
