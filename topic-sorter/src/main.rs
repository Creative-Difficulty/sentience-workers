use color_eyre::eyre::WrapErr as _;
use topic_sorter::{config::Config, llm, worker::Worker};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    // .env first, so a RUST_LOG set there is honoured by the subscriber
    let dotenv_problem = llm_worker_common::load_dotenv();
    llm_worker_common::init_tracing(env!("CARGO_CRATE_NAME"))?;
    if let Some(problem) = dotenv_problem {
        tracing::warn!("{problem}");
    }

    // validated before anything else; never report readiness for a bad config
    let config = Config::from_env()?;
    llm_worker_common::clear_ready_file(&config.ready_file).await;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.db_url)
        .await
        .wrap_err("failed to connect to DATABASE_URL")?;

    let client = llm::build_client(&config);

    llm_worker_common::write_ready_file(&config.ready_file).await?;
    Worker::new(pool, client, &config).run().await
}
