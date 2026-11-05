use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use aws_sdk_s3::config::Credentials;
use ingest_vestibule_retriever::media::{HttpFetcher, MediaLimits, S3Store};
use ingest_vestibule_retriever::startup::{self, ReadyFile};
use serenity::all::GatewayIntents;
use tracing_subscriber::layer::SubscriberExt as _;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let directive = startup::load_env_and_filter_directive(None);
    setup_tracing(&directive)?;

    let env_vars = get_env_vars()?;
    tracing::debug!("loaded environment variables");

    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&env_vars.db_url)
        .await?;
    tracing::debug!("created database connection pool");

    let (message_id_cache, channel_id_cache, user_id_cache) =
        ingest_vestibule_retriever::load_caches(&db_pool).await?;

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_MESSAGE_REACTIONS;

    let s3_client = setup_s3(&env_vars).await?;

    let media_limits = MediaLimits::from_env()?;
    tracing::debug!(?media_limits, "media download limits");
    let fetcher = HttpFetcher::with_limits(&media_limits)?;

    let handler = ingest_vestibule_retriever::discord_handler::DiscordEventHandler {
        db_pool,
        fetcher: Arc::new(fetcher),
        store: Arc::new(S3Store {
            client: s3_client,
            bucket: env_vars.s3_bucket_name.clone(),
        }),
        guild_id: serenity::all::GuildId::new(env_vars.discord_guild_id),
        message_id_cache,
        channel_id_cache,
        user_id_cache,
    };

    let mut client = serenity::Client::builder(&env_vars.discord_token, intents)
        .event_handler(handler)
        .await?;

    ReadyFile(PathBuf::from("/tmp/ready")).mark()?;
    tracing::debug!("wrote readiness file to /tmp/ready");

    if let Err(e) = client.start().await {
        tracing::error!("discord client error: {e}");
    }

    Ok(())
}

fn setup_tracing(directive: &str) -> color_eyre::Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_new(directive)?;

    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(env_filter);

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

struct EnvVars {
    discord_token: String,
    db_url: String,
    discord_guild_id: u64,
    s3_url: String,
    s3_access_key_id: String,
    s3_secret_access_key: String,
    s3_bucket_name: String,
}

fn get_env_vars() -> color_eyre::Result<EnvVars> {
    Ok(EnvVars {
        discord_token: env::var("DISCORD_TOKEN")?,
        db_url: env::var("DATABASE_URL")?,
        discord_guild_id: env::var("GUILD_ID")?.parse::<u64>()?,
        s3_url: env::var("S3_URL")?,
        s3_access_key_id: env::var("S3_ACCESS_KEY_ID")?,
        s3_secret_access_key: env::var("S3_SECRET_ACCESS_KEY")?,
        s3_bucket_name: env::var("S3_BUCKET_NAME")?,
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
