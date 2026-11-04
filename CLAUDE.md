# CLAUDE.md — sentience-workers

Full system spec: https://github.com/Vimothy-s-Vestibule/unidb-schema/blob/main/docs/SPEC.md
(lands with the unidb-schema docs PR; until merged, read `docs/SPEC.md` on that branch).

## What this repo is

Cargo workspace (edition 2024) with the three Sentience backend services plus the docker-compose stack:

| Crate | Role | Reads | Writes |
|---|---|---|---|
| `ingest-vestibule-retriever` | serenity 0.12 Discord bot for one guild (`GUILD_ID`); live events + full historical scan on every `ready`; downloads media to S3 (rustfs) | Discord gateway/REST/CDN, id caches from unidb | `discord_channels`, `vestibule_users`, `discord_accounts`, `messages`, `message_edits`, `media_assets`, `message_attachments`, `discord_emojis`, `message_reactions` (+ S3 objects) |
| `fact-extractor` | polls 25 unprocessed messages, one LLM call per message, stores facts/activities/skills | `messages`, `discord_accounts`, `discord_channels`, `message_attachments` | `user_facts_and_activities`, `fact_and_activity_evidence`, `fact_extraction_attempts` |
| `topic-sorter` | polls 50 unclassified messages, one LLM call per channel group, assigns topics | `messages`, `topic`, `topic_message_relation` | `topic`, `topic_message_relation`, `message_classification_attempts` |

Related repos: **unidb-schema** owns the database schema and the `unidb` crate (git dependency, pinned in `Cargo.lock`;
update with `cargo update -p unidb`). **Sentience-dash** reads the same database to display results. Services never
call each other — integration is only through Postgres (ADR 0001); LLM work queues are "attempts" tables (ADR 0002).

## Build / test / lint (verified on main)

The `sqlx::query!` macros need a **live database with the unidb schema at compile time** (there is no `.sqlx/`
offline cache yet; a fix is in flight). Without `DATABASE_URL` the build fails with
"set `DATABASE_URL` to use query macros online".

```sh
# throwaway local DB with the schema (from a unidb-schema checkout)
docker run -d --name unidb-local -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=sentience \
  -p 127.0.0.1:5433:5432 pgvector/pgvector:pg17-trixie
cat ../unidb-schema/schema/*.sql | docker exec -i unidb-local psql -U postgres -d sentience -v ON_ERROR_STOP=1
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5433/sentience

cargo build --workspace                 # OK
cargo test --workspace                  # OK, 0 tests exist
cargo clippy --workspace --all-targets  # clean
cargo fmt --all -- --check              # FAILS on main (handlers/emoji.rs, topic-sorter/src/main.rs) — run cargo fmt
```

If you change SQL in a `query!` macro, the DB you compile against must have the matching schema.

## Run

```sh
cargo run -p fact-extractor   # env from the shell or a .env in the CWD (see <crate>/.env.example)
cp .env.example .env && docker compose --profile app up -d --build   # full stack
docker compose up -d                                                  # only postgres + rustfs (no profile)
```

`docker compose --profile app build` is **currently broken from a clean clone** (Dockerfile builder has no database;
fix in flight). The `postgres` image downloads `schema/*.sql` from GitHub (`UNIDB_REF`, default `main`) at build time and
applies it only on first init of the `pg_data` volume — schema changes later need manual `ALTER`s. Ports: Postgres
`5432` and rustfs console `9001` are published on the host; S3 API is `rustfs:9000` inside the network.

## Environment variables

| Var | Who | Notes |
|---|---|---|
| `DATABASE_URL` | all (runtime + compile time) | compose builds it from `POSTGRES_USER/PASSWORD/DB` |
| `DISCORD_TOKEN`, `GUILD_ID` (u64) | ingest | required |
| `S3_URL`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `S3_BUCKET_NAME` | ingest | required; region hardcoded `yo-mama`, path-style |
| `LLM_BASE_URL` (`…/v1`), `LLM_API_KEY`, `LLM_MODEL` | fact-extractor, topic-sorter | OpenAI-compatible, model must support `json_schema` output |
| `DISCORD_SERVER_NAME` | fact-extractor | required, used in the prompt |
| `RUST_LOG` | all | default `<crate>=trace`; compose `<crate>=debug`. Ignored when set only in `.env` (dotenv loads after tracing init) |
| `RUSTFS_ACCESS_KEY`, `RUSTFS_SECRET_KEY` | compose | **never change after first start** (rustfs refuses to boot) |
| `UNIDB_REF` | compose build arg | unidb-schema branch for the postgres image |

Never commit `.env`; never point a dev build at a production database.

## Conventions

- Discord ids: `id.get() as i64` everywhere; columns are `bigint`. Use Rust `snake_case`; SQL is `snake_case`.
- `ensure_*` helpers (`handlers/`) are "insert if not in the in-memory `IdCache`"; caches are loaded at startup from
  `messages`, `discord_channels`, `discord_accounts`.
- Placeholder for a vanished reply parent: `sent_by = 0` (seeded "Deleted User" account), `content = '[deleted
  message]'`, `sent_at = deleted_at = 1970-01-01T00:00:00Z`. LLM workers skip `'[deleted message]'`.
- System messages get synthetic content (`system_message_content` in `handlers/message.rs`).
- Insert a fact and its evidence in the **same transaction** (deferred trigger `ensure_fact_has_evidence`).
- Attempts tables: insert the message id only after it was processed; no row = will be retried.
- Errors: `color_eyre::Result`, log with `tracing::error!(error = %e, …)`; handlers run in spawned tasks.
- S3 keys: `discord/attachments/{msg}/{att}`, `discord/stickers/{id}.{ext}`, `discord/emojis/{id}.webp`,
  `discord/avatars/{user}/{hash}` (`TODO.md` lists an outdated format).

## Gotchas

- Failed LLM messages are never recorded → a batch that always fails is re-fetched forever (`ORDER BY … LIMIT`): stalls
  the worker and burns tokens. Results and attempts rows are written non-atomically (crash ⇒ duplicates).
- topic-sorter puts **all** topic names into every prompt; it does not filter `deleted_at`.
- No HTTP timeouts (LLM via async-openai, downloads via `reqwest::get`). Two async-openai versions (0.37 topic-sorter,
  0.38 fact-extractor).
- `/tmp/ready` (the compose healthcheck) is written before the service does real work and never removed.
- ingest: stickers are stored only if the message also has attachments; edits call `ensure_message` before reading the
  old content; user creation is non-transactional and racy; reaction add lacks `ON CONFLICT`; deletes are not handled;
  the historical scan ignores threads and buffers whole channels in memory.
- `ingest-vestibule-retriever/Cargo.lock` is stale and ignored — the workspace root `Cargo.lock` is authoritative.
- No tests and no CI exist; add tests next to the code you touch (DB-backed tests need the schema loaded).
