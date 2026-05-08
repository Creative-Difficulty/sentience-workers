# Sentience

A program that ingests a Discord server's content and uses LLMs to extract per-user facts, skills, and activities, and clusters messages into topics. All state lives in a shared Postgres database called [unidb](https://github.com/Vimothy-s-Vestibule/unidb-schema).

Anyone with an active discord server can use it to get insights into and cool visualization of what their users are talking about and what is happening on the server in general.

## Project structure

Each microservice has its own README describing the service in greater detail, check them out!

- `ingest-vestibule-retriever` - ingest discord bot
- `fact-extractor` - Extracts facts about users, activities the users did, and emotions from discord messages using an LLM
- `topic-sorter` - Sorts discord messages into topics which are created ad hoc; message "meaning" extraction and topic determination both use an LLM

## Deployment

All three services are dockerized. The easiest way to run/deploy all of them is to use docker compose.

Before starting the stack, copy `.env.example` to `.env` and fill in the values. In particular, pick a `RUSTFS_ACCESS_KEY` and `RUSTFS_SECRET_KEY`, these are the S3 access-key / secret-key pairs that rustfs uses for both its admin console (at <http://localhost:9001>) and as the S3 credentials the apps use to read/write objects. You don't need to generate anything in the rustfs console first; whatever values you put in `.env` *are* the credentials.

> [!IMPORTANT]
> Pick these values once and don't change them after first start. RustFS encrypts its IAM config with the initial secret key and refuses to start with different credentials later (`crypto: decrypt failed`). Rotating them requires wiping the `rustfs_data` docker volume, which deletes all stored objects.

Then run `docker compose --profile app up -d` (`-d` for `detached`, running in the background).

### Non-Docker Installation/Deployment

In the case you do not wish to do so, you can run each service individually (but you have to compile them yourself, see the `##Contributing` section for instructions on how to setup your computer to compile the project, or contact me with your operating system and CPU architecture and I will provide you with binaries if are not familiar with compiling yourself).

If you prefer not to use docker, ensure you have:

- `rustup` and its dependencies installed to install the rust compiler and toolchain
- A PostgreSQL database available with the pgvector extension installed.
- An S3 bucket available

Steps:

1. Ensure you have a PostgreSQL database setup with the [unidb schema](https://github.com/Vimothy-s-Vestibule/unidb-schema) loaded onto it and the pgvector extension installed and its functions available
2. Ensure you have an S3 bucket with the proper credentials and permissions to write to it
3. Put each compiled binary into its own folder and pass the required environment variables to it, either by prepending them to the run command like this: `FOO=BAR DOG=CAT ./ingest_vestibule_retriever`, or by creating a `.env` file in the same directory as the binary and writing them inside if it, just like in the `.env.example` files of each service.
4. Run the binaries!

## Contributing

1. Make sure you have the rust compiler and cargo installed (Best installed using <https://rustup.rs/>)
2. Clone this repository using git (download at <https://git-scm.com>) or GitHub desktop. If you wish to make changes to the unidb database schema, download/clone the [unidb](https://github.com/Vimothy-s-Vestibule/unidb-schema) repository as well/instead.
3. Ensure you have access to a postgres database and an S3 bucket for testing. The S3 bucket is only necessary for development on a service in this repo (sentience-workers), not for making changes to the unidb-schema.
    a) If you are making changes to the unidb-schema, make sure to update the rust models and the manually written (!) sql files in the `schema/` folder, so that all fields match and the SQL tables can be deserialized seamlessly into the rust structs in the `src/models` folder.
4. Make the changes you want to make, then create a new repository on your GitHub account, commit and push the project with your changes to it.
5. Open a pull request on this repository
6. Done! Thank you for contributing! I'll review your changes and get back to you.
