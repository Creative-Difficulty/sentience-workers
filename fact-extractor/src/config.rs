//! Environment configuration (validated at start-up).

use std::path::PathBuf;
use std::time::Duration;

use llm_worker_common::{DEFAULT_READY_FILE, env::Env};

const DEFAULT_BATCH_SIZE: i64 = 25;
const DEFAULT_LLM_TIMEOUT_SECS: u64 = 120;
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_RETRY_BASE_DELAY_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct Config {
    pub db_url: String,
    pub llm_base_url: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub discord_server_name: String,
    /// Messages fetched per polling round (`BATCH_SIZE`, default 25).
    pub batch_size: i64,
    /// Per-request LLM timeout (`LLM_TIMEOUT_SECS`, default 120).
    pub llm_timeout: Duration,
    /// Failures after which a message is recorded as attempted and skipped
    /// (`MAX_ATTEMPTS`, default 3).
    pub max_attempts: u32,
    /// Cool-down after a message's first failure; doubles per failure, capped
    /// at 15 minutes (`RETRY_BASE_DELAY_SECS`, default 30).
    pub retry_base_delay: Duration,
    /// Written once the service is up (`READY_FILE`, default /tmp/ready).
    pub ready_file: PathBuf,
}

impl Config {
    pub fn from_env() -> color_eyre::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Build and validate the config from an arbitrary key lookup (used by tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> color_eyre::Result<Self> {
        let env = Env::new(&get);
        Ok(Config {
            db_url: env.required("DATABASE_URL")?,
            llm_base_url: env.url("LLM_BASE_URL")?,
            llm_api_key: env.required("LLM_API_KEY")?,
            llm_model: env.required("LLM_MODEL")?,
            discord_server_name: env.required("DISCORD_SERVER_NAME")?,
            batch_size: env.int_at_least("BATCH_SIZE", DEFAULT_BATCH_SIZE, 1)?,
            llm_timeout: env.secs("LLM_TIMEOUT_SECS", DEFAULT_LLM_TIMEOUT_SECS, 1)?,
            max_attempts: env.int_at_least("MAX_ATTEMPTS", DEFAULT_MAX_ATTEMPTS, 1)?,
            retry_base_delay: env.secs(
                "RETRY_BASE_DELAY_SECS",
                DEFAULT_RETRY_BASE_DELAY_SECS,
                0,
            )?,
            ready_file: env.path_or("READY_FILE", DEFAULT_READY_FILE),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn cfg(extra: &[(&str, &str)]) -> color_eyre::Result<Config> {
        let mut m: HashMap<&str, &str> = HashMap::from([
            ("DATABASE_URL", "postgres://u:p@h/db"),
            ("LLM_BASE_URL", "https://openrouter.ai/api/v1"),
            ("LLM_API_KEY", "k"),
            ("LLM_MODEL", "m"),
            ("DISCORD_SERVER_NAME", "srv"),
        ]);
        m.extend(extra.iter().copied());
        Config::from_lookup(|k| m.get(k).map(|v| v.to_string()))
    }

    #[test]
    fn defaults() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.batch_size, 25);
        assert_eq!(c.llm_timeout, Duration::from_secs(120));
        assert_eq!(c.max_attempts, 3);
        assert_eq!(c.retry_base_delay, Duration::from_secs(30));
        assert_eq!(c.ready_file, PathBuf::from("/tmp/ready"));
    }

    #[test]
    fn validation() {
        assert!(cfg(&[("LLM_BASE_URL", "not-a-url")]).is_err());
        assert!(cfg(&[("LLM_MODEL", "")]).is_err());
        assert!(cfg(&[("BATCH_SIZE", "0")]).is_err());
        assert!(cfg(&[("MAX_ATTEMPTS", "0")]).is_err());
        assert!(cfg(&[("LLM_TIMEOUT_SECS", "0")]).is_err());
        assert!(cfg(&[("LLM_TIMEOUT_SECS", "ten")]).is_err());
        assert!(cfg(&[("RETRY_BASE_DELAY_SECS", "0")]).is_ok());
    }
}
