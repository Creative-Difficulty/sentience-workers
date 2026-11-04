//! The polling loop: fetch unattempted messages, ask the LLM, store facts.

use std::time::Duration;

use async_openai::{Client, config::OpenAIConfig};
use color_eyre::eyre::eyre;
use ormlite::Model as _;
use sqlx::PgPool;
use unidb::{FactAndActivityEvidence, Message};
use uuid::Uuid;

use crate::config::Config;
use crate::prompt::build_extract_llm_request;
use crate::records::{build_fact, parse_llm_response};

const IDLE_SLEEP: Duration = Duration::from_secs(5);
const MIN_CONTENT_LEN: i32 = 10;
const CONTEXT_SIZE: i64 = 10;

/// What one [`Worker::process_batch`] call did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchOutcome {
    /// Messages fetched from the queue.
    pub fetched: usize,
    /// Messages recorded in `fact_extraction_attempts`.
    pub attempted: usize,
}

pub struct Worker {
    pool: PgPool,
    client: Client<OpenAIConfig>,
    model: String,
    server_name: String,
    batch_size: i64,
}

impl Worker {
    pub fn new(pool: PgPool, client: Client<OpenAIConfig>, config: &Config) -> Self {
        Worker {
            pool,
            client,
            model: config.llm_model.clone(),
            server_name: config.discord_server_name.clone(),
            batch_size: config.batch_size,
        }
    }

    #[tracing::instrument(skip_all)]
    pub async fn run(&self) -> color_eyre::Result<()> {
        loop {
            let outcome = self.process_batch().await?;
            if outcome.fetched == 0 || outcome.attempted == 0 {
                tokio::time::sleep(IDLE_SLEEP).await;
            }
        }
    }

    /// Fetch one batch of unattempted messages and process it.
    pub async fn process_batch(&self) -> color_eyre::Result<BatchOutcome> {
        let pool = &self.pool;
        let batch = sqlx::query_as!(
            Message,
            r#"SELECT m.* FROM messages m
           WHERE NOT EXISTS (
               SELECT 1 FROM fact_extraction_attempts fea
               WHERE fea.message_id = m.message_id
           )
             AND m.deleted_at IS NULL
             AND m.content != '[deleted message]'
             AND char_length(m.content) >= $1
           ORDER BY m.sent_at
           LIMIT $2"#,
            MIN_CONTENT_LEN,
            self.batch_size
        )
        .fetch_all(pool)
        .await?;

        if batch.is_empty() {
            tracing::debug!("no unattempted messages, waiting...");
            return Ok(BatchOutcome::default());
        }

        tracing::info!(count = batch.len(), "processing batch");
        let fetched = batch.len();

        let mut attempted = Vec::with_capacity(batch.len());
        for msg in batch {
            match process_message(
                pool,
                &self.client,
                &self.model,
                &msg,
                self.server_name.clone(),
            )
            .await
            {
                Ok(()) => attempted.push(msg.message_id),
                Err(e) => {
                    tracing::error!(message_id = msg.message_id, error = %e, "extraction failed")
                }
            }
        }

        if !attempted.is_empty() {
            sqlx::query!(
                r#"INSERT INTO fact_extraction_attempts (message_id)
           SELECT m FROM UNNEST($1::bigint[]) AS m
           ON CONFLICT (message_id) DO NOTHING"#,
                attempted.as_slice()
            )
            .execute(pool)
            .await?;
        }
        Ok(BatchOutcome {
            fetched,
            attempted: attempted.len(),
        })
    }
}

#[tracing::instrument(skip_all, fields(message_id = msg.message_id))]
async fn process_message(
    pool: &PgPool,
    client: &Client<OpenAIConfig>,
    model: &str,
    msg: &Message,
    server_name: String,
) -> color_eyre::Result<()> {
    let user_id = sqlx::query_scalar!(
        "SELECT vestibule_user_id FROM discord_accounts WHERE discord_user_id = $1",
        msg.sent_by
    )
    .fetch_one(pool)
    .await?;

    let reply_target = fetch_reply_target(pool, msg).await?;
    let mut context = fetch_context(pool, msg).await?;
    if let Some(ref rt) = reply_target {
        context.retain(|c| c.message_id != rt.message_id);
    }
    tracing::debug!(
        context_len = context.len(),
        has_reply_target = reply_target.is_some(),
        "fetched surrounding context"
    );

    let has_attachments = sqlx::query_scalar!(
        r#"SELECT EXISTS(
               SELECT 1 FROM message_attachments
               WHERE message_id = $1 AND deleted_at IS NULL
           ) AS "exists!""#,
        msg.message_id,
    )
    .fetch_one(pool)
    .await?;

    let channel_name = sqlx::query_scalar!(
        "SELECT name FROM discord_channels WHERE channel_id = $1",
        msg.channel_id,
    )
    .fetch_optional(pool)
    .await?;

    let request = build_extract_llm_request(
        model,
        msg,
        &context,
        reply_target.as_ref(),
        has_attachments,
        server_name,
        channel_name,
    )?;
    tracing::debug!(%model, msg_len = msg.content.len(), has_attachments, "sending extract request");

    let raw = client
        .chat()
        .create(request)
        .await?
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| eyre!("no content in LLM response"))?;

    tracing::debug!(
        content = raw.chars().take(50).collect::<String>(),
        "received LLM response"
    );

    let extracted = parse_llm_response(&raw)?;

    tracing::debug!(record_count = extracted.len(), "LLM returned records");

    if extracted.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    for record in &extracted {
        let fact = build_fact(user_id, msg.sent_at, record);
        let evidence = FactAndActivityEvidence {
            id: Uuid::new_v4(),
            fact_or_activity_id: fact.id,
            message_id: Some(msg.message_id),
            youtube_comment_id: None,
            external_content_id: None,
            discord_presence_id: None,
            weight: record.confidence,
            reasoning: record.reasoning.clone(),
        };
        fact.insert(&mut *tx).await?;
        evidence.insert(&mut *tx).await?;
    }
    tx.commit().await?;

    tracing::info!(count = extracted.len(), "inserted records");
    Ok(())
}

async fn fetch_context(pool: &PgPool, focal_msg: &Message) -> color_eyre::Result<Vec<Message>> {
    let mut before = sqlx::query_as!(
        Message,
        r#"SELECT m.* FROM messages m
           WHERE m.channel_id = $1 AND m.sent_at < $2 AND m.message_id != $3 AND m.deleted_at IS NULL
           ORDER BY m.sent_at DESC
           LIMIT $4"#,
        focal_msg.channel_id,
        focal_msg.sent_at,
        focal_msg.message_id,
        CONTEXT_SIZE,
    )
    .fetch_all(pool)
    .await?;
    before.reverse();

    let after = sqlx::query_as!(
        Message,
        r#"SELECT m.* FROM messages m
           WHERE m.channel_id = $1 AND m.sent_at > $2 AND m.message_id != $3 AND m.deleted_at IS NULL
           ORDER BY m.sent_at ASC
           LIMIT $4"#,
        focal_msg.channel_id,
        focal_msg.sent_at,
        focal_msg.message_id,
        CONTEXT_SIZE,
    )
    .fetch_all(pool)
    .await?;

    let mut all = before;
    all.extend(after);
    Ok(all)
}

async fn fetch_reply_target(
    pool: &PgPool,
    focal_msg: &Message,
) -> color_eyre::Result<Option<Message>> {
    let Some(reply_id) = focal_msg.in_reply_to else {
        return Ok(None);
    };
    let target = sqlx::query_as!(
        Message,
        r#"SELECT m.* FROM messages m
           WHERE m.message_id = $1 AND m.deleted_at IS NULL"#,
        reply_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(target)
}
