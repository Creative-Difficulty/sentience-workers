use std::env;

use async_openai::{Client, config::OpenAIConfig};
use color_eyre::eyre::WrapErr as _;
use tracing_subscriber::layer::SubscriberExt as _;

mod extractor;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    setup_tracing()?;

    // TODO check behavior
    dotenvy::dotenv().ok();

    let env_vars = get_env_vars()?;
    tracing::debug!("loaded environment variables from .env");

    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&env_vars.db_url)
        .await
        .wrap_err("failed to connect to DATABASE_URL")?;
    tracing::debug!("created database connection pool");

    let llm_client = build_llm_client(&env_vars);
    tracing::debug!("built LLM client");

    tokio::fs::write("/tmp/ready", "1").await?;
    tracing::debug!("wrote readiness file to /tmp/ready");

    extractor::run(&db_pool, &llm_client, &env_vars.llm_model).await
}

fn setup_tracing() -> color_eyre::Result<()> {
    let default_filter = format!("{}=trace", env!("CARGO_CRATE_NAME"));
    let env_filter = tracing_subscriber::EnvFilter::try_new(
        std::env::var("RUST_LOG").unwrap_or(default_filter),
    )?;

    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(env_filter);

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

struct EnvVars {
    db_url: String,
    llm_base_url: String,
    llm_api_key: String,
    llm_model: String,
}

fn get_env_vars() -> color_eyre::Result<EnvVars> {
    Ok(EnvVars {
        db_url: env::var("DATABASE_URL")?,
        llm_base_url: env::var("LLM_BASE_URL")?,
        llm_api_key: env::var("LLM_API_KEY")?,
        llm_model: env::var("LLM_MODEL")?,
    })
}

fn build_llm_client(env_vars: &EnvVars) -> Client<OpenAIConfig> {
    let config = OpenAIConfig::new()
        .with_api_base(&env_vars.llm_base_url)
        .with_api_key(&env_vars.llm_api_key);
    Client::with_config(config)
}
