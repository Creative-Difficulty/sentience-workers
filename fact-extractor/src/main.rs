use color_eyre::eyre::WrapErr as _;
use fact_extractor::{config::Config, llm, worker::Worker};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    llm_worker_common::init_tracing(env!("CARGO_CRATE_NAME"))?;

    // TODO check behavior
    dotenvy::dotenv().ok();

    let config = Config::from_env()?;
    tracing::debug!("loaded environment variables from .env");

    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.db_url)
        .await
        .wrap_err("failed to connect to DATABASE_URL")?;
    tracing::debug!("created database connection pool");

    let llm_client = llm::build_client(&config);
    tracing::debug!("built LLM client");

    tokio::fs::write("/tmp/ready", "1").await?;
    tracing::debug!("wrote readiness file to /tmp/ready");
    Worker::new(db_pool, llm_client, &config).run().await
}
