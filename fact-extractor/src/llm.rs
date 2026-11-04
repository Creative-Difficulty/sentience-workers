//! LLM client construction.

use async_openai::{Client, config::OpenAIConfig};

use crate::config::Config;

pub fn build_client(config: &Config) -> Client<OpenAIConfig> {
    let llm_client_config = OpenAIConfig::new()
        .with_api_base(&config.llm_base_url)
        .with_api_key(&config.llm_api_key);
    Client::with_config(llm_client_config)
}
