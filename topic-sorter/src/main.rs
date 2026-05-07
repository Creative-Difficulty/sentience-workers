use std::env;

use color_eyre::eyre::WrapErr as _;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

mod llm;
mod sorter;

fn env_var(name: &str) -> color_eyre::Result<String> {
    env::var(name).wrap_err_with(|| format!("missing required env var: {name}"))
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let default_filter = format!("{}=trace", env!("CARGO_CRATE_NAME"));
    let filter =
        tracing_subscriber::EnvFilter::try_new(env::var("RUST_LOG").unwrap_or(default_filter))?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(filter)
        .init();

    dotenvy::dotenv().ok();

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&env_var("DATABASE_URL")?)
        .await
        .wrap_err("failed to connect to DATABASE_URL")?;

    tokio::fs::write("/tmp/ready", "1").await?;

    let http = reqwest::Client::new();
    let base_url = env_var("LLM_BASE_URL")?;
    let api_key = env_var("LLM_API_KEY")?;
    let model = env_var("LLM_MODEL")?;

    sorter::run(&pool, &http, &base_url, &api_key, &model).await
}
