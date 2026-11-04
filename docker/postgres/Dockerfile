# Postgres + pgvector with the unidb schema baked into the first-init dir.
#
# The schema comes from the `unidb` stage below, which by default fetches
# unidb-schema at a pinned commit (UNIDB_REF, default = the rev pinned in the
# workers' Cargo.lock, so the DB schema matches the compiled sqlx/ormlite
# models). Override it either way:
#   docker build --build-arg UNIDB_REF=<40-char sha> docker/postgres
#   docker build --build-context unidb=../unidb-schema docker/postgres   # local checkout, no network
#
# NOTE: initdb.d scripts only run when the data volume is empty. Changing the
# schema later requires a migration or wiping the pg_data volume.

ARG PG_IMAGE=pgvector/pgvector:0.8.7-pg17-trixie@sha256:01215e09bd4bd582d20230e025b6c971deeeffee49ceef1aae167e839cb62a7e
ARG UNIDB_REF=a7a7c55458aef4f1bef899c4527af3559a925033

FROM scratch AS unidb
ARG UNIDB_REF
ADD https://github.com/Vimothy-s-Vestibule/unidb-schema.git#${UNIDB_REF} /

FROM ${PG_IMAGE}
ARG UNIDB_REF
LABEL org.opencontainers.image.title="sentience-postgres" \
      org.opencontainers.image.description="pgvector Postgres with the unidb schema applied on first init" \
      org.opencontainers.image.source="https://github.com/Vimothy-s-Vestibule/sentience-workers" \
      dev.sentience.unidb.ref="${UNIDB_REF}"
# Concatenate every schema/*.sql in lexicographic (whole-filename) order
# (`awk 1` guarantees a newline between files).
# A glob that matches nothing would silently produce an empty schema, so fail.
RUN --mount=type=bind,from=unidb,target=/unidb \
    set -eu; \
    ls /unidb/schema/*.sql >/dev/null; \
    awk 1 /unidb/schema/*.sql > /docker-entrypoint-initdb.d/10-unidb-schema.sql; \
    test -s /docker-entrypoint-initdb.d/10-unidb-schema.sql
