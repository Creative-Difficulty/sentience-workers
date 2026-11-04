//! Shared plumbing for the LLM-backed workers (`fact-extractor`, `topic-sorter`).
//!
//! * [`env`]: typed, validated environment configuration.
//! * [`llm`]: OpenAI-compatible client with timeouts, and one-shot JSON chat calls.
//! * [`failure`]: classification of errors into "this message is bad" vs
//!   "the system is unhealthy".
//! * [`retry`]: per-message retry budget with cool-down, plus a global back-off.
//! * [`text`]: helpers to put untrusted text into prompts and into Postgres.

use std::path::Path;

use tracing_subscriber::layer::SubscriberExt as _;

pub mod env;
pub mod failure;
pub mod llm;
pub mod retry;
pub mod text;

#[cfg(feature = "test-support")]
pub mod test_support;

/// Install the global tracing subscriber.
///
/// `RUST_LOG` wins when set; otherwise `<crate_name>=trace` is used. Call this
/// *after* loading `.env` so a `RUST_LOG` in `.env` is honoured.
pub fn init_tracing(crate_name: &str) -> color_eyre::Result<()> {
    let default_filter = format!("{crate_name}=trace");
    let env_filter = tracing_subscriber::EnvFilter::try_new(
        std::env::var("RUST_LOG").unwrap_or(default_filter),
    )?;

    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(env_filter);

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

/// Load `.env` from the working directory (or a parent). Returns a message
/// to log once tracing is up when the file exists but could not be parsed.
pub fn load_dotenv() -> Option<String> {
    match dotenvy::dotenv() {
        Ok(_) => None,
        Err(e) if e.not_found() => None,
        Err(e) => Some(format!("failed to load .env: {e}")),
    }
}

/// Remove a readiness file left over from a previous run of this container.
pub async fn clear_ready_file(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "could not remove stale readiness file")
        }
    }
}

/// Signal readiness to the container health check.
pub async fn write_ready_file(path: &Path) -> color_eyre::Result<()> {
    tokio::fs::write(path, "1").await?;
    tracing::debug!(path = %path.display(), "wrote readiness file");
    Ok(())
}
