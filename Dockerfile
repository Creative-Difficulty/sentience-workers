# Multi-stage build for the three sentience workers (one image per --target).
#
#   docker build --target fact-extractor -t sentience/fact-extractor .
#
# The build never needs a database: sqlx query macros are checked against the
# committed .sqlx/ offline cache (regenerate with `cargo sqlx prepare --workspace`
# whenever a query or the pinned unidb rev changes).
#
# Optional build input (not required, never ends up in an image layer):
#   --build-context unidb=/path/to/unidb-schema
#       A local git checkout of unidb-schema (must contain .git and the commit
#       pinned in Cargo.lock). When given, cargo fetches the `unidb` git
#       dependency from it instead of github.com (offline / air-gapped builds).

# Base images are pinned by version AND digest; bump both together.
ARG RUST_IMAGE=rust:1.93.1-slim-bookworm@sha256:5b9332190bb3b9ece73b810cd1f1e9f06343b294ce184bcb067f0747d7d333ea
ARG RUNTIME_IMAGE=debian:bookworm-20261027-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3

# Default (empty) stand-in for the optional `unidb` build context.
FROM scratch AS unidb

FROM ${RUST_IMAGE} AS chef
ARG CARGO_CHEF_VERSION=0.1.78
# slim image: add what openssl-sys (native-tls) and cargo's git CLI fetch need.
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev git \
    && rm -rf /var/lib/apt/lists/*
# Wrapper that applies the optional `unidb` build context described above, then execs.
COPY --chmod=0755 <<'EOF' /usr/local/bin/build-env
#!/bin/sh
set -eu
if [ -d /unidb/.git ]; then
    export CARGO_NET_GIT_FETCH_WITH_CLI=true \
        GIT_CONFIG_COUNT=2 \
        GIT_CONFIG_KEY_0=url.file:///unidb/.insteadOf \
        GIT_CONFIG_VALUE_0=https://github.com/Vimothy-s-Vestibule/unidb-schema \
        GIT_CONFIG_KEY_1=safe.directory \
        GIT_CONFIG_VALUE_1='*'
fi
exec "$@"
EOF
RUN cargo install cargo-chef --locked --version "${CARGO_CHEF_VERSION}"
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=bind,from=unidb,target=/unidb \
    build-env cargo chef cook --release --locked --recipe-path recipe.json
COPY . .
# Compile sqlx macros against .sqlx/ instead of a live DATABASE_URL.
ENV SQLX_OFFLINE=true
RUN --mount=type=bind,from=unidb,target=/unidb \
    build-env cargo build --release --locked --workspace --bins \
    && mkdir -p /out \
    && for bin in fact-extractor topic-sorter ingest-vestibule-retriever; do \
        strip --strip-all -o "/out/$bin" "target/release/$bin"; \
    done

FROM ${RUNTIME_IMAGE} AS runtime-base
ARG RUNTIME_IMAGE
# Binaries link only glibc, libgcc_s and OpenSSL 3 (native-tls); see `ldd`.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 sentience \
    && useradd --system --uid 10001 --gid sentience --no-create-home \
        --home-dir /nonexistent --shell /usr/sbin/nologin sentience
ARG VCS_REF=unknown
LABEL org.opencontainers.image.source="https://github.com/Vimothy-s-Vestibule/sentience-workers" \
      org.opencontainers.image.vendor="Vimothy's Vestibule" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.base.name="${RUNTIME_IMAGE}"
USER 10001:10001
# The services write /tmp/ready once started (see compose healthchecks).
HEALTHCHECK --interval=10s --timeout=3s --start-period=60s --retries=5 \
    CMD ["test", "-f", "/tmp/ready"]
ENTRYPOINT ["/usr/local/bin/service"]

FROM runtime-base AS fact-extractor
LABEL org.opencontainers.image.title="sentience-fact-extractor" \
      org.opencontainers.image.description="Extracts per-user facts, skills and activities from Discord messages with an LLM"
COPY --from=builder --chmod=0755 /out/fact-extractor /usr/local/bin/service

FROM runtime-base AS topic-sorter
LABEL org.opencontainers.image.title="sentience-topic-sorter" \
      org.opencontainers.image.description="Groups Discord messages into LLM-labelled topics"
COPY --from=builder --chmod=0755 /out/topic-sorter /usr/local/bin/service

FROM runtime-base AS ingest-vestibule-retriever
LABEL org.opencontainers.image.title="sentience-ingest-vestibule-retriever" \
      org.opencontainers.image.description="Discord bot that ingests server content into unidb"
COPY --from=builder --chmod=0755 /out/ingest-vestibule-retriever /usr/local/bin/service
