//! Environment configuration.

use color_eyre::eyre::eyre;

const DEFAULT_BATCH_SIZE: i64 = 25;

#[derive(Debug, Clone)]
pub struct Config {
    pub db_url: String,
    pub llm_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub discord_server_name: String,
    /// Messages fetched per polling round (`BATCH_SIZE`, default 25).
    pub batch_size: i64,
}

impl Config {
    pub fn from_env() -> color_eyre::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Build the config from an arbitrary key lookup (used by tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> color_eyre::Result<Self> {
        let required =
            |name: &str| get(name).ok_or_else(|| eyre!("missing required env var: {name}"));
        Ok(Config {
            db_url: required("DATABASE_URL")?,
            llm_base_url: required("LLM_BASE_URL")?,
            llm_api_key: required("LLM_API_KEY")?,
            llm_model: required("LLM_MODEL")?,
            discord_server_name: required("DISCORD_SERVER_NAME")?,
            batch_size: match get("BATCH_SIZE") {
                Some(v) => v.parse()?,
                None => DEFAULT_BATCH_SIZE,
            },
        })
    }
}
