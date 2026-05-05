use std::env;

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

    tokio::fs::write("/tmp/ready", "1").await?;
    tracing::debug!("wrote readiness file to /tmp/ready");

    Ok(())
}

fn setup_tracing() -> color_eyre::Result<()> {
    let filter = tracing_subscriber::filter::Targets::new()
        .with_target(env!("CARGO_CRATE_NAME"), tracing::Level::DEBUG);

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
    db_url: String,
}

fn get_env_vars() -> color_eyre::Result<EnvVars> {
    let db_url = env::var("DATABASE_URL")?;

    Ok(EnvVars { db_url })
}
