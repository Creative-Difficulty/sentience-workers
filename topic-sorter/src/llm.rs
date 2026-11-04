//! LLM client construction.

use async_openai::{Client, config::OpenAIConfig};

use crate::config::Config;

/// Client whose HTTP requests time out after `LLM_TIMEOUT_SECS`.
pub fn build_client(config: &Config) -> Client<OpenAIConfig> {
    llm_worker_common::llm::build_client(
        &config.llm_base_url,
        &config.llm_api_key,
        config.llm_timeout,
    )
}
