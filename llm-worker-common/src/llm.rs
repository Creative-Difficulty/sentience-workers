//! OpenAI-compatible LLM client with request timeouts.

use std::time::Duration;

use async_openai::{Client, config::OpenAIConfig, types::chat::CreateChatCompletionRequest};
use color_eyre::eyre::eyre;

use crate::failure::Failure;

/// Upper bound for establishing the TCP/TLS connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Build a client whose individual HTTP requests time out after `timeout`.
///
/// Panics only if the TLS backend cannot be initialised, exactly like
/// `async_openai::Client::with_config` (which calls `reqwest::Client::new`).
pub fn build_client(base_url: &str, api_key: &str, timeout: Duration) -> Client<OpenAIConfig> {
    let http = reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .build()
        .expect("failed to initialise the HTTP client (TLS backend)");
    let config = OpenAIConfig::new()
        .with_api_base(base_url)
        .with_api_key(api_key);
    Client::with_config(config).with_http_client(http)
}

/// Send one chat completion request and return the content of the first choice.
///
/// `deadline` bounds the *whole* call. async-openai retries 429/5xx/connection
/// errors internally (and honours `Retry-After`), so the per-request HTTP
/// timeout alone does not bound how long a call can take.
pub async fn complete(
    client: &Client<OpenAIConfig>,
    request: CreateChatCompletionRequest,
    deadline: Duration,
) -> Result<String, Failure> {
    let response = tokio::time::timeout(deadline, client.chat().create(request))
        .await
        .map_err(|_| Failure::systemic(eyre!("LLM call exceeded {deadline:?}")))??;
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| Failure::message(eyre!("LLM response has no choices")))?;
    match choice.message.content {
        Some(content) => Ok(content),
        None => Err(Failure::message(eyre!(
            "no content in LLM response (finish_reason: {:?}, refusal: {:?})",
            choice.finish_reason,
            choice.message.refusal
        ))),
    }
}
