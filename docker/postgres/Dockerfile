FROM pgvector/pgvector:pg17-trixie

ARG UNIDB_REF=main

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Pull schema/ from unidb-schema, concatenate every *.sql in lexicographic
# (whole-filename) order, and drop the result into Postgres's first-init dir.
RUN curl -fsSL "https://codeload.github.com/Vimothy-s-Vestibule/unidb-schema/tar.gz/refs/heads/${UNIDB_REF}" \
        | tar xz -C /tmp \
    && cat /tmp/unidb-schema-*/schema/*.sql > /docker-entrypoint-initdb.d/unidb-schema.sql \
    && rm -rf /tmp/unidb-schema-*
