# topic-sorter

This microservice groups messages from the postgres (unidb) database into topics which are automatically created by an LLM.

## Setup

Create a `.env` file and copy the contents of the `.env.example` file into it and populate it with your own values.

Make sure your `DATABASE_URL` is the full connection string including all credentials (Ends with `/postgres` on supabase).
Also ensure that your instance of Postgres has the pgvector extension installed.
If you don't want to set up Postgres yourself, supabase is a great option which I've been using during development.

The LLM base URL expects an OpenAI compatible API and should end at `https://.../.../.../v1`, for example the correct openrouter `LLM_BASE_URL` is `https://openrouter.ai/api/v1`.

Make sure to use a model and provider that supports structured JSON output. Topic-sorter uses a JSON schema, but describes the output schema to LLM in the system prompt as well (as a fallback).
I have tested `openai/gpt-oss-120b:free`, its error rate is very high and it often halluncinates JSON structures so it is not recommended.
The currently recommended choice is `deepseek/deepseek-v4-flash`, as it both supports structured JSON output (tested on openrouter) and is cheap enough.
You can even self host it (that's part of the reason I selected it) if you have an extremely powerful GPU.

Note that topic-sorter (and all other sentience microservices) consumes a lot of tokens, I have observed about 1 million per hour in total when all microservices are running nonstop.

If topic-sorter runs into ratelimting (or other) errors with your LLM provider, the message(s) that cannot be sucessfully processed (assigned to no group or a group) are automatically attempted again on the next run.
