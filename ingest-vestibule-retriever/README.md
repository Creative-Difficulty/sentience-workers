# ingest-vestibule-retriever

This is the discord bot which ingests all data coming from discord into the unidb database.
It is called Sentience and has no user-facing commands and does not send any messages or do anything on the server it is added to.

Its purpose is to index all content which is added to the server both in real time (live event handlers are in the `discord_handler.rs`), and content which was sent/added before the bot was added (in the `historical/` folder).

Note that the bot will, on startup, reindex all messages on the server.
This will look like the bot is adding them to the database again and again, but it is really just checking if they are already in the database against a local cache, which is filled once on every startup.

The re-indexing process should be fairly quick, but it depends on how many messages are in the server (throughput for reindexing is about 10 messages/second).

## Environment varibles

Create a `.env` file and copy the contents of the `.env.example` file into it and populate it with your own values.

The vestibule-retriever needs an S3 bucket (and the corresponding credentials in the `.env` file) for discord media storage.
fill in the credentials you got from your S3 bucket into the fields which start with `S3_`.
I use `rustfs` as a self-hosted S3 implementation, however you can of course opt to use any S3 implementation.

> [!WARNING]
> Note that the S3 region parameter is hardcoded to `yo-mama` in the code, as rustfs does not check the s3 region parameter, only its existance.

Make sure your `DATABASE_URL` is the full connection string including all credentials (Ends with `/postgres` on supabase).
Also ensure that your instance of Postgres has the pgvector extension installed.
If you don't want to set up Postgres yourself, supabase is a great option which I've been using during development.

To get a `DISCORD_TOKEN`, go to <https://discord.com/developers/applications> and create an application, and follow this tutorial on how to get a discord bot token: <https://discordgsm.com/guide/how-to-get-a-discord-bot-token>.

Invite the bot you created in you discord application to your server, again this tutorial is very helpful: <https://www.xda-developers.com/how-to-create-discord-bot/>

Set `GUILD_ID` to the guild (discord-speak for server) id. This is a good tutorial on how to get this value: <https://tokenizedhq.com/discord-server-id/>

Note that you need to keep the ingest-vestibule-retriever process running as long as you want the bot to record messages in real time.
