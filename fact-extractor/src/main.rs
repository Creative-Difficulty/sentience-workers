use std::env;

use tracing_subscriber::layer::SubscriberExt as _;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    setup_tracing()?;

    // TODO check behavior
    dotenvy::dotenv().ok();

    let db_url = env::var("DATABASE_URL")?;

    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;
    tracing::debug!("created database connection pool");

    let existing_ids = sqlx::query_scalar!("SELECT message_id FROM messages")
        .fetch_all(&db_pool)
        .await?;

    tokio::fs::write("/tmp/ready", "1").await?;
    tracing::debug!("wrote readiness file to /tmp/ready");

    // if let Err(e) = client.start().await {
    //     tracing::error!("discord client error: {e}");
    // }

    Ok(())
}

fn setup_tracing() -> color_eyre::Result<()> {
    let default_filter = format!("{}=trace,serenity=off", env!("CARGO_CRATE_NAME"));
    let env_filter = tracing_subscriber::EnvFilter::try_new(
        std::env::var("RUST_LOG").unwrap_or(default_filter),
    )?;

    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(env_filter);

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}
