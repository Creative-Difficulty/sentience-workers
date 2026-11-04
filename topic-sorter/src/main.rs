use color_eyre::eyre::WrapErr as _;
use topic_sorter::{config::Config, llm, worker::Worker};

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    llm_worker_common::init_tracing(env!("CARGO_CRATE_NAME"))?;

    dotenvy::dotenv().ok();

    let db_url =
        std::env::var("DATABASE_URL").wrap_err("missing required env var: DATABASE_URL")?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .wrap_err("failed to connect to DATABASE_URL")?;

    tokio::fs::write("/tmp/ready", "1").await?;

    let config = Config::from_env()?;
    let client = llm::build_client(&config);

    Worker::new(pool, client, &config).run().await
}
