# Sentience

A program that ingests a Discord server's content and uses LLMs to extract per-user facts, skills, and activities, and clusters messages into topics. All state lives in a shared Postgres database called [unidb](https://github.com/Vimothy-s-Vestibule/unidb-schema).

## Project structure

- `ingest-vestibule-retriever` - ingest discord bot
- `fact-extractor` - Extracts facts about users, activities the users did, and emotions from discord messages using an LLM
- `topic-sorter` - Sorts discord messages into topics which are created ad hoc; message "meaning" extraction and topic determination both use an LLM

## Setup and Deployment

All three services are dockerized. The easiest way to run/deploy all of them (including the vestibule-retriever) is to use docker compose (`docker compose up -d`, -d for `detached`, running in the background).

If you prefer not to use docker, ensure you have:

- `rustup` and its dependencies installed to install the rust compiler and toolchain
- A PostgreSQL database available with the pgvector extension installed.
- An S3 bucket available

## Contributing

1. Make sure you have the rust compiler and cargo installed (Best installed using <https://rustup.rs/>)
2. Clone this repository using git (download at <https://git-scm.com>) or GitHub desktop. If you wish to make changes to the unidb database schema, download/clone the [unidb](https://github.com/Vimothy-s-Vestibule/unidb-schema) repository as well/instead.
3. Ensure you have access to a postgres database and an S3 bucket for testing. The S3 bucket is only necessary for development on a service in this repo (sentience-workers), not for making changes to the unidb-schema.
    a) If you are making changes to the unidb-schema, make sure to update the rust models and the manually written (!) sql files in the `schema/` folder, so that all fields match and the SQL tables can be deserialized seamlessly into the rust structs in the `src/models` folder.
4. Make the changes you want to make, then create a new repository on your GitHub account, commit and push the project with your changes to it.
5. Open a pull request on this repository
6. Done! Thank you for contributing! I'll review your changes and get back to you.
