//! Shared plumbing for the LLM-backed workers (`fact-extractor`, `topic-sorter`).

use tracing_subscriber::layer::SubscriberExt as _;

#[cfg(feature = "test-support")]
pub mod test_support;

/// Install the global tracing subscriber.
///
/// `RUST_LOG` wins when set; otherwise `<crate_name>=trace` is used.
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
