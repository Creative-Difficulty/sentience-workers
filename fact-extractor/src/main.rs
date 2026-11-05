use color_eyre::eyre::WrapErr as _;
use fact_extractor::{config::Config, llm, worker::Worker};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    // .env first, so a RUST_LOG set there is honoured by the subscriber
    let dotenv_problem = llm_worker_common::load_dotenv();
    llm_worker_common::init_tracing(env!("CARGO_CRATE_NAME"))?;
    if let Some(problem) = dotenv_problem {
        tracing::warn!("{problem}");
    }

    // A previous run of this container may have left the readiness file:
    // remove it first, so an invalid config never looks healthy.
    llm_worker_common::clear_ready_file(&llm_worker_common::ready_file_from_env()).await;
    // validated before anything else; never report readiness for a bad config
    let config = Config::from_env()?;
    tracing::debug!("loaded and validated configuration");

    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.db_url)
        .await
        .wrap_err("failed to connect to DATABASE_URL")?;
    tracing::debug!("created database connection pool");

    let llm_client = llm::build_client(&config);
    tracing::debug!(timeout = ?config.llm_timeout, "built LLM client");

    llm_worker_common::write_ready_file(&config.ready_file).await?;
    Worker::new(db_pool, llm_client, &config).run().await
}
