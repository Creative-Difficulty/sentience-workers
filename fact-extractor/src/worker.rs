//! The polling loop: fetch unattempted messages, ask the LLM, store facts.
//!
//! Guarantees:
//! * **No head-of-line stall.** A failing message is put on a cool-down and
//!   skipped by the polling query while other messages continue; after
//!   `MAX_ATTEMPTS` strikes it is recorded in `fact_extraction_attempts` (with
//!   no facts) and leaves the queue. See [`llm_worker_common::retry`].
//! * **Idempotent writes.** The attempt row is inserted in the same
//!   transaction as the facts and evidence of that message, first, with
//!   `ON CONFLICT DO NOTHING`: a crash can never leave facts without an
//!   attempt row (which would duplicate them on restart), and a message that
//!   another worker already stored is rolled back instead of duplicated.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_openai::{Client, config::OpenAIConfig};
use chrono::Utc;
use color_eyre::eyre::eyre;
use llm_worker_common::failure::{Failure, FailureKind};
use llm_worker_common::retry::{Backoff, RetryPolicy, RetryTracker, Verdict};
use ormlite::Model as _;
use sqlx::PgPool;
use unidb::Message;

use crate::config::Config;
use crate::prompt::{PromptInput, build_extract_llm_request};
use crate::records::{build_evidence, build_fact, parse_llm_response, sanitize};

const IDLE_SLEEP: Duration = Duration::from_secs(5);
const MIN_CONTENT_LEN: i32 = 10;
const CONTEXT_SIZE: i64 = 10;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(15 * 60);
const BACKOFF_BASE: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);

/// What one [`Worker::process_batch`] call did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchOutcome {
    /// Messages fetched from the queue.
    pub fetched: usize,
    /// Messages recorded in `fact_extraction_attempts` (stored or given up).
    pub attempted: usize,
    /// Messages that failed and stay queued (cooling down).
    pub failed: usize,
    /// Messages recorded as attempted after exhausting their retry budget.
    pub given_up: usize,
    /// Set after a systemic failure: pause this long before the next round.
    pub backoff: Option<Duration>,
}

/// How one message ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stored {
    Records(usize),
    /// Another worker recorded the attempt first; our work was rolled back.
    AlreadyAttempted,
}

/// How long to wait before the next polling round: the back-off after a
/// systemic failure, the idle sleep when the round made no progress (nothing
/// fetched, or everything fetched failed and is cooling down), otherwise none.
fn pause_after(outcome: &BatchOutcome) -> Option<Duration> {
    match outcome.backoff {
        Some(d) => Some(d),
        None if outcome.fetched == 0 || outcome.attempted == 0 => Some(IDLE_SLEEP),
        None => None,
    }
}

pub struct Worker {
    pool: PgPool,
    client: Client<OpenAIConfig>,
    model: String,
    server_name: String,
    batch_size: i64,
    llm_deadline: Duration,
    retries: Mutex<RetryTracker>,
    backoff: Mutex<Backoff>,
}

impl Worker {
    pub fn new(pool: PgPool, client: Client<OpenAIConfig>, config: &Config) -> Self {
        Worker {
            pool,
            client,
            model: config.llm_model.clone(),
            server_name: config.discord_server_name.clone(),
            batch_size: config.batch_size,
            // bounds async-openai's internal 429/5xx retries as well
            llm_deadline: config.llm_timeout * 2,
            retries: Mutex::new(RetryTracker::new(RetryPolicy {
                max_attempts: config.max_attempts,
                base_delay: config.retry_base_delay,
                max_delay: MAX_RETRY_DELAY,
            })),
            backoff: Mutex::new(Backoff::new(BACKOFF_BASE, BACKOFF_MAX)),
        }
    }

    #[tracing::instrument(skip_all)]
    pub async fn run(&self) -> color_eyre::Result<()> {
        loop {
            let pause = match self.process_batch().await {
                Ok(outcome) => pause_after(&outcome),
                Err(e) => {
                    let d = self.backoff.lock().unwrap().next_delay();
                    tracing::error!(error = %e, pause = ?d, "polling round failed");
                    Some(d)
                }
            };
            if let Some(d) = pause {
                tokio::time::sleep(d).await;
            }
        }
    }

    /// Fetch one batch of unattempted messages and process it. Never sleeps.
    pub async fn process_batch(&self) -> color_eyre::Result<BatchOutcome> {
        let skip = self.retries.lock().unwrap().cooling_down(Instant::now());
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
             AND m.message_id <> ALL($3::bigint[])
           ORDER BY m.sent_at
           LIMIT $2"#,
            MIN_CONTENT_LEN,
            self.batch_size,
            &skip,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut outcome = BatchOutcome {
            fetched: batch.len(),
            ..Default::default()
        };
        if batch.is_empty() {
            tracing::debug!(
                cooling_down = skip.len(),
                "no unattempted messages, waiting..."
            );
            return Ok(outcome);
        }
        tracing::info!(
            count = batch.len(),
            cooling_down = skip.len(),
            "processing batch"
        );

        for msg in &batch {
            match self.process_message(msg).await {
                Ok(stored) => {
                    self.retries.lock().unwrap().record_success(msg.message_id);
                    self.backoff.lock().unwrap().reset();
                    outcome.attempted += 1;
                    tracing::debug!(message_id = msg.message_id, ?stored, "message done");
                }
                Err(failure) => {
                    let verdict = self.retries.lock().unwrap().record_failure(
                        msg.message_id,
                        failure.kind,
                        Instant::now(),
                    );
                    match verdict {
                        Verdict::Retry { strikes, .. } => {
                            outcome.failed += 1;
                            tracing::error!(
                                message_id = msg.message_id,
                                kind = ?failure.kind,
                                strikes,
                                error = %failure.error,
                                "extraction failed, will retry later"
                            );
                        }
                        Verdict::GiveUp { strikes } => {
                            tracing::error!(
                                message_id = msg.message_id,
                                kind = ?failure.kind,
                                strikes,
                                error = %failure.error,
                                "extraction failed permanently, recording message as attempted without facts"
                            );
                            self.give_up(msg.message_id).await?;
                            outcome.given_up += 1;
                            outcome.attempted += 1;
                        }
                    }
                    if failure.kind == FailureKind::Systemic {
                        // the provider/DB is unhealthy: don't hammer it with
                        // the rest of the batch
                        outcome.backoff = Some(self.backoff.lock().unwrap().next_delay());
                        break;
                    }
                }
            }
        }
        Ok(outcome)
    }

    async fn give_up(&self, message_id: i64) -> color_eyre::Result<()> {
        sqlx::query!(
            "INSERT INTO fact_extraction_attempts (message_id) VALUES ($1)
             ON CONFLICT (message_id) DO NOTHING",
            message_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(message_id = msg.message_id))]
    async fn process_message(&self, msg: &Message) -> Result<Stored, Failure> {
        let pool = &self.pool;
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
        if channel_name.is_none() {
            tracing::error!(
                msg_id = msg.message_id,
                "Channel that the focal message was sent in is not in the database"
            );
        }

        let request = build_extract_llm_request(
            &self.model,
            &PromptInput {
                focal: msg,
                context: &context,
                reply_target: reply_target.as_ref(),
                has_attachments,
                server_name: &self.server_name,
                channel_name: channel_name.as_deref(),
                now: Utc::now(),
            },
        )
        .map_err(Failure::message)?;
        tracing::debug!(model = %self.model, msg_len = msg.content.len(), has_attachments, "sending extract request");

        let raw =
            llm_worker_common::llm::complete(&self.client, request, self.llm_deadline).await?;
        // the provider answered: whatever happens next is not an outage
        self.retries.lock().unwrap().record_healthy();
        tracing::debug!(
            content = raw.chars().take(50).collect::<String>(),
            "received LLM response"
        );

        let parsed = parse_llm_response(&raw).map_err(Failure::message)?;
        let records: Vec<_> = parsed
            .records
            .iter()
            .filter_map(|r| {
                let clean = sanitize(r, msg.sent_at);
                if clean.is_none() {
                    tracing::warn!(record = ?r, "dropping invalid record from LLM response");
                }
                clean
            })
            .collect();
        tracing::debug!(
            returned = parsed.records.len() + parsed.malformed,
            kept = records.len(),
            "LLM returned records"
        );

        // One transaction per message: attempt row first, then the facts.
        let mut tx = pool.begin().await?;
        let claimed = sqlx::query!(
            "INSERT INTO fact_extraction_attempts (message_id) VALUES ($1)
             ON CONFLICT (message_id) DO NOTHING",
            msg.message_id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if claimed == 0 {
            tx.rollback().await?;
            tracing::warn!("message was already recorded as attempted by another worker; skipping");
            return Ok(Stored::AlreadyAttempted);
        }
        for record in &records {
            let fact = build_fact(user_id, record);
            let evidence = build_evidence(fact.id, msg.message_id, record);
            fact.insert(&mut *tx).await.map_err(ormlite_failure)?;
            evidence.insert(&mut *tx).await.map_err(ormlite_failure)?;
        }
        tx.commit().await?;

        tracing::info!(count = records.len(), "inserted records");
        Ok(Stored::Records(records.len()))
    }
}

fn ormlite_failure(e: ormlite::Error) -> Failure {
    match e {
        ormlite::Error::SqlxError(e) => e.into(),
        other => Failure::message(eyre!(other)),
    }
}

async fn fetch_context(pool: &PgPool, focal_msg: &Message) -> Result<Vec<Message>, sqlx::Error> {
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
) -> Result<Option<Message>, sqlx::Error> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing() {
        let o = |fetched, attempted, backoff| BatchOutcome {
            fetched,
            attempted,
            backoff,
            ..Default::default()
        };
        let b = Some(Duration::from_secs(40));
        assert_eq!(pause_after(&o(0, 0, None)), Some(IDLE_SLEEP));
        assert_eq!(pause_after(&o(3, 0, None)), Some(IDLE_SLEEP));
        assert_eq!(pause_after(&o(3, 1, None)), None);
        assert_eq!(pause_after(&o(3, 1, b)), b);
        assert_eq!(pause_after(&o(0, 0, b)), b);
    }

    #[test]
    fn delay_caps() {
        assert_eq!(IDLE_SLEEP, Duration::from_secs(5));
        assert_eq!(MAX_RETRY_DELAY, Duration::from_secs(15 * 60));
        assert_eq!(BACKOFF_BASE, Duration::from_secs(5));
        assert_eq!(BACKOFF_MAX, Duration::from_secs(5 * 60));
    }
}
