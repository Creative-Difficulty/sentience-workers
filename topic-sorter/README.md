# topic-sorter

This microservice groups messages from the postgres (unidb) database into topics which are automatically created by an LLM.

## Setup

Create a `.env` file and copy the contents of the `.env.example` file into it and populate it with your own values.

Make sure your `DATABASE_URL` is the full connection string including all credentials (Ends with `/postgres` on supabase). This is the format it should follow: `postgres://user:password@host:port/db-name`.
Also ensure that your instance of Postgres has the pgvector extension installed.
If you don't want to set up Postgres yourself, supabase is a great option which I've been using during development.

The LLM base URL expects an OpenAI compatible API and should end at `https://.../.../.../v1`, for example the correct openrouter `LLM_BASE_URL` is `https://openrouter.ai/api/v1`.

Make sure to use a model and provider that supports structured JSON output. Topic-sorter uses a JSON schema, but describes the output schema to LLM in the system prompt as well (as a fallback).
I have tested `openai/gpt-oss-120b:free`, its error rate is very high and it often hallucinates JSON structures so it is not recommended.
The currently recommended choice is `deepseek/deepseek-v4-flash`, as it both supports structured JSON output (tested on openrouter) and is cheap enough.
You can even self host it (that's part of the reason I selected it) if you have an extremely powerful GPU.

Note that topic-sorter (and all other sentience microservices) consumes a lot of tokens, I have observed about 1 million per hour in total when all microservices are running nonstop.

## Failure handling

Failed batches are retried, but never block the queue: a failing message is put on a cool-down (`RETRY_BASE_DELAY_SECS`, doubling per failure, capped at 15 minutes) and skipped while newer messages are processed.
After `MAX_ATTEMPTS` failures that are attributable to the message itself (malformed LLM JSON, a request the provider rejects, data Postgres refuses) it is recorded as attempted without results and logged at `error` level, so a poison message cannot stall the worker or burn tokens forever.
Provider outages, rate limits and database hiccups pause the worker with an exponential back-off instead and do not use up the message's budget (they only count when the system was healthy right before, so a message that reliably triggers e.g. an HTTP 500 is still given up eventually).
The retry budget lives in memory, so a restart grants every message a fresh budget.

LLM requests time out after `LLM_TIMEOUT_SECS`. Results and the attempt record of a batch are written in one transaction, so a crash never duplicates results.

See `.env.example` for all optional settings. `/tmp/ready` (`READY_FILE`) is written only after the configuration was validated and the database connection succeeded.

## Tests

`cargo test -p topic-sorter` runs unit tests and, when `DATABASE_URL` points to a Postgres server with the pgvector extension, integration tests that create a throw-away database per test (loading the unidb schema at the revision pinned in `Cargo.lock`) and mock the LLM. Without `DATABASE_URL` the integration tests are skipped; set `REQUIRE_DB_TESTS=1` to make that an error.
