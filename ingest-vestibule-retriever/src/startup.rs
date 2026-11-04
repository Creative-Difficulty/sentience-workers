//! Process start-up helpers (environment, tracing filter, readiness file).

use std::path::{Path, PathBuf};

/// Default tracing filter when `RUST_LOG` is not set.
pub fn default_filter_directive() -> String {
    format!("{}=trace,serenity=off", env!("CARGO_CRATE_NAME"))
}

/// The tracing filter directive taken from `RUST_LOG`, or the default.
pub fn env_filter_directive() -> String {
    std::env::var("RUST_LOG").unwrap_or_else(|_| default_filter_directive())
}

/// Loads `.env` (or the given file) into the process environment and returns the tracing
/// filter directive to install.
pub fn load_env_and_filter_directive(dotenv_file: Option<&Path>) -> String {
    let directive = env_filter_directive();
    match dotenv_file {
        Some(p) => dotenvy::from_path(p).ok(),
        None => dotenvy::dotenv().ok().map(|_| ()),
    };
    directive
}

/// File whose existence signals readiness to the container healthcheck.
#[derive(Debug, Clone)]
pub struct ReadyFile(pub PathBuf);

impl ReadyFile {
    pub fn mark(&self) -> std::io::Result<()> {
        std::fs::write(&self.0, "1")
    }
}
