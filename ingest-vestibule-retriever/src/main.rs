use std::env;

use aws_sdk_s3::config::Credentials;
use serenity::all::GatewayIntents;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    setup_tracing()?;

    dotenvy::dotenv()?;
    let env_vars = get_env_vars()?;
    tracing::debug!("loaded environment variables from .env");

    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&env_vars.db_url)
        .await?;
    tracing::debug!("created database connection pool");

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_MESSAGE_REACTIONS;

    let s3_client = setup_s3(&env_vars).await?;

    let handler = ingest_vestibule_retriever::discord_handler::DiscordEventHandler {
        db_pool,
        s3_client,
        s3_bucket: env_vars.s3_bucket_name,
        intro_channel_id: serenity::all::ChannelId::new(env_vars.discord_intro_channel_id),
        guild_id: serenity::all::GuildId::new(env_vars.discord_guild_id),
    };

    let mut client = serenity::Client::builder(&env_vars.discord_token, intents)
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
            tracing_subscriber::fmt::layer(), // .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE),
        ),
        filter,
    );

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

struct EnvVars {
    discord_token: String,
    db_url: String,
    discord_guild_id: u64,
    discord_intro_channel_id: u64,
    s3_url: String,
    s3_access_key_id: String,
    s3_secret_access_key: String,
    s3_bucket_name: String,
}

fn get_env_vars() -> color_eyre::Result<EnvVars> {
    let discord_token = env::var("DISCORD_TOKEN")?;
    let db_url = env::var("DATABASE_URL")?;
    let discord_guild_id = env::var("GUILD_ID")?.parse::<u64>()?;
    let discord_intro_channel_id = env::var("DISCORD_INTRO_CHANNEL_ID")?.parse::<u64>()?;
    let s3_url = env::var("S3_URL")?.parse::<String>()?;

    let s3_access_key_id = env::var("S3_ACCESS_KEY_ID")?.parse::<String>()?;

    let s3_secret_access_key = env::var("S3_SECRET_ACCESS_KEY")?.parse::<String>()?;

    let s3_bucket_name = env::var("S3_BUCKET_NAME")?.parse::<String>()?;

    Ok(EnvVars {
        discord_token,
        db_url,
        discord_guild_id,
        discord_intro_channel_id,
        s3_url,
        s3_access_key_id,
        s3_secret_access_key,
        s3_bucket_name,
    })
}

async fn setup_s3(env_vars: &EnvVars) -> color_eyre::Result<aws_sdk_s3::Client> {
    let s3_creds = Credentials::new(
        &env_vars.s3_access_key_id,
        &env_vars.s3_secret_access_key,
        None,
        None,
        "",
    );

    let config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .endpoint_url(&env_vars.s3_url)
        .credentials_provider(s3_creds)
        // (rustfs does not validate regions)
        .region("yo-mama");

    let sdk_config = config_loader.load().await;

    let s3_config_builder = aws_sdk_s3::config::Builder::from(&sdk_config).force_path_style(true);

    Ok(aws_sdk_s3::Client::from_conf(s3_config_builder.build()))
}
