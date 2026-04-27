use std::env;

use serenity::all::GatewayIntents;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    setup_tracing()?;

    dotenvy::dotenv()?;
    let (discord_token, db_url, discord_guild_id, discord_intro_channel_id) = get_env_vars()?;

    tracing::debug!("loaded environment variables from .env");

    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;
    tracing::debug!("created database connection pool");

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MEMBERS;

    let handler = ingest_vestibule_retriever::discord_handler::DiscordEventHandler {
        db_pool,
        intro_channel_id: serenity::all::ChannelId::new(discord_intro_channel_id),
        guild_id: serenity::all::GuildId::new(discord_guild_id),
    };

    let mut client = serenity::Client::builder(&discord_token, intents)
        .event_handler(handler)
        .await?;

    tokio::fs::write("/tmp/ready", "1").await?;
    tracing::debug!("wrote readiness file to /tmp/ready");

    if let Err(e) = client.start().await {
        tracing::error!("discord client error: {e}");
    }

    Ok(())
}

fn setup_tracing() -> color_eyre::Result<()> {
    let filter = tracing_subscriber::filter::Targets::new()
        .with_target(env!("CARGO_CRATE_NAME"), tracing::Level::DEBUG)
        .with_target("serenity", tracing::level_filters::LevelFilter::OFF);

    let subscriber = tracing_subscriber::layer::SubscriberExt::with(
        tracing_subscriber::layer::SubscriberExt::with(
            tracing_subscriber::registry(),
            tracing_subscriber::fmt::layer()
                .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE),
        ),
        filter,
    );

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

fn get_env_vars() -> color_eyre::Result<(String, String, u64, u64)> {
    let discord_token = env::var("DISCORD_TOKEN")?;
    let db_url = env::var("DATABASE_URL")?;
    let discord_guild_id = env::var("GUILD_ID")?.parse::<u64>()?;
    let intro_channel_id = env::var("DISCORD_INTRO_CHANNEL_ID")?.parse::<u64>()?;

    Ok((discord_token, db_url, discord_guild_id, intro_channel_id))
}
