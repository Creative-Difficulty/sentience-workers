//! The polling loop: fetch unclassified messages, ask the LLM, store topics.
//!
//! Guarantees:
//! * **No head-of-line stall.** When a channel batch fails, its messages are
//!   put on a cool-down and skipped by the polling query while other
//!   messages continue; after `MAX_ATTEMPTS` strikes they are recorded in
//!   `message_classification_attempts` (without a topic) and leave the queue.
//! * **Atomic, idempotent writes.** The attempt rows, topics and relations of
//!   one channel batch are written in a single transaction; attempt rows are
//!   claimed first (`ON CONFLICT DO NOTHING RETURNING`) and only claimed
//!   messages get relations, so a crash leaves nothing half-applied and two
//!   workers never both label the same message.
//! * **Bounded prompts.** At most `MAX_PROMPT_TOPICS` existing topic names
//!   (most recently used first) are injected, and message content is
//!   truncated, so the prompt cannot grow with the size of the database.

use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_openai::{Client, config::OpenAIConfig};
use color_eyre::eyre::eyre;
use llm_worker_common::failure::{Failure, FailureKind};
use llm_worker_common::retry::{Backoff, RetryPolicy, RetryTracker, Verdict};
use sqlx::{PgConnection, PgPool};
use unidb::Message;
use uuid::Uuid;

use crate::config::Config;
use crate::prompt::{ContextMessage, build_request, build_system_prompt, build_user_prompt};
use crate::response::{parse_llm_response, plan_assignments};

const CONTEXT_SIZE: i64 = 50;
const IDLE_SLEEP: Duration = Duration::from_secs(5);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(15 * 60);
const BACKOFF_BASE: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);

/// What one [`Worker::process_batch`] call did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchOutcome {
    /// Messages fetched from the queue.
    pub fetched: usize,
    /// Messages recorded in `message_classification_attempts` (classified or given up).
    pub attempted: usize,
    /// Messages whose batch failed and that stay queued (cooling down).
    pub failed: usize,
    /// Messages recorded as attempted after exhausting their retry budget.
    pub given_up: usize,
    /// Set after a systemic failure: pause this long before the next round.
    pub backoff: Option<Duration>,
}

pub struct Worker {
    pool: PgPool,
    client: Client<OpenAIConfig>,
    model: String,
    batch_size: i64,
    max_prompt_topics: i64,
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
            batch_size: config.batch_size,
            max_prompt_topics: config.max_prompt_topics,
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
                Ok(outcome) => match outcome.backoff {
                    Some(d) => Some(d),
                    None if outcome.fetched == 0 || outcome.attempted == 0 => Some(IDLE_SLEEP),
                    None => None,
                },
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

    /// Fetch one batch of unclassified messages and process it. Never sleeps.
    pub async fn process_batch(&self) -> color_eyre::Result<BatchOutcome> {
        let skip = self.retries.lock().unwrap().cooling_down(Instant::now());
        let batch = sqlx::query_as!(
            Message,
            r#"SELECT m.* FROM messages m
               WHERE NOT EXISTS (
                   SELECT 1 FROM message_classification_attempts mca
                   WHERE mca.message_id = m.message_id
               )
                 AND m.content != '[deleted message]'
                 AND m.deleted_at IS NULL
                 AND m.message_id <> ALL($2::bigint[])
               ORDER BY m.channel_id, m.sent_at
               LIMIT $1"#,
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
                "no unclassified messages, waiting..."
            );
            return Ok(outcome);
        }
        tracing::info!(
            count = batch.len(),
            cooling_down = skip.len(),
            "processing batch"
        );

        let mut msgs_by_channel: BTreeMap<i64, Vec<Message>> = BTreeMap::new();
        for message in batch {
            msgs_by_channel
                .entry(message.channel_id)
                .or_default()
                .push(message);
        }

        for (channel_id, msgs) in &msgs_by_channel {
            let ids: Vec<i64> = msgs.iter().map(|m| m.message_id).collect();
            match self.classify_channel(*channel_id, msgs).await {
                Ok(claimed) => {
                    let mut r = self.retries.lock().unwrap();
                    for id in &ids {
                        r.record_success(*id);
                    }
                    drop(r);
                    self.backoff.lock().unwrap().reset();
                    outcome.attempted += claimed;
                }
                Err(failure) => {
                    let verdicts = self.retries.lock().unwrap().record_failures(
                        &ids,
                        failure.kind,
                        Instant::now(),
                    );
                    let give_up: Vec<i64> = ids
                        .iter()
                        .zip(&verdicts)
                        .filter(|(_, v)| matches!(v, Verdict::GiveUp { .. }))
                        .map(|(id, _)| *id)
                        .collect();
                    let strikes = verdicts
                        .iter()
                        .map(|v| match v {
                            Verdict::Retry { strikes, .. } | Verdict::GiveUp { strikes } => {
                                *strikes
                            }
                        })
                        .max()
                        .unwrap_or(0);
                    tracing::error!(
                        channel_id,
                        kind = ?failure.kind,
                        strikes,
                        giving_up = give_up.len(),
                        error = %failure.error,
                        "channel batch failed"
                    );
                    if !give_up.is_empty() {
                        self.give_up(&give_up).await?;
                        outcome.given_up += give_up.len();
                        outcome.attempted += give_up.len();
                    }
                    outcome.failed += ids.len() - give_up.len();
                    if failure.kind == FailureKind::Systemic {
                        outcome.backoff = Some(self.backoff.lock().unwrap().next_delay());
                        break;
                    }
                }
            }
        }
        Ok(outcome)
    }

    async fn give_up(&self, ids: &[i64]) -> color_eyre::Result<()> {
        sqlx::query!(
            r#"INSERT INTO message_classification_attempts (message_id)
               SELECT m FROM UNNEST($1::bigint[]) AS m
               ON CONFLICT (message_id) DO NOTHING"#,
            ids
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Classify one channel's messages; returns how many messages this
    /// worker recorded as attempted.
    #[tracing::instrument(skip_all, fields(channel_id, batch_len = msgs.len()))]
    async fn classify_channel(&self, channel_id: i64, msgs: &[Message]) -> Result<usize, Failure> {
        let pool = &self.pool;
        let first_sent = msgs
            .iter()
            .map(|m| m.sent_at)
            .min()
            .ok_or_else(|| Failure::message(eyre!("Empty batch of messages passed")))?;

        // CONTEXT_SIZE messages before the current batch with their topics (if already labeled)
        let mut context: Vec<ContextMessage> = sqlx::query!(
            r#"SELECT m.message_id, m.content, t.name AS "topic?"
           FROM messages m
           LEFT JOIN topic_message_relation tmr ON tmr.message_id = m.message_id
           LEFT JOIN topic t ON t.id = tmr.topic_id
           WHERE m.channel_id = $1 AND m.sent_at < $2 AND m.deleted_at IS NULL
           ORDER BY m.sent_at DESC
           LIMIT $3"#,
            channel_id,
            first_sent,
            CONTEXT_SIZE
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| ContextMessage {
            message_id: r.message_id,
            content: r.content,
            topic: r.topic,
        })
        .collect();
        // newest-first from the DB; the LLM gets chronological order
        context.reverse();

        // Only the most recently used topic names: the full list grows without
        // bound and would eventually make every request exceed the context window.
        let topics: Vec<String> = sqlx::query_scalar!(
            r#"SELECT t.name AS "name!"
               FROM topic t
               JOIN topic_message_relation r ON r.topic_id = t.id
               JOIN messages m ON m.message_id = r.message_id
               GROUP BY t.name
               ORDER BY max(m.sent_at) DESC, t.name
               LIMIT $1"#,
            self.max_prompt_topics
        )
        .fetch_all(pool)
        .await?;

        let sys_msg = build_system_prompt(&topics, &context);
        let user_msg = build_user_prompt(msgs);
        tracing::debug!(model = %self.model, user_msg_len = user_msg.len(), topics = topics.len(), "sending classify request");
        let request = build_request(&self.model, sys_msg, &user_msg).map_err(Failure::message)?;

        let llm_response =
            llm_worker_common::llm::complete(&self.client, request, self.llm_deadline).await?;
        self.retries.lock().unwrap().record_healthy();
        tracing::debug!(
            content = llm_response.chars().take(50).collect::<String>(),
            "received LLM response"
        );

        let parsed = parse_llm_response(&llm_response).map_err(Failure::message)?;

        // One transaction for the whole channel batch. Claim the attempt rows
        // first: messages another worker already recorded are not relabelled.
        let mut tx = pool.begin().await?;
        let batch_ids: Vec<i64> = msgs.iter().map(|m| m.message_id).collect();
        let claimed: HashSet<i64> = sqlx::query_scalar!(
            r#"INSERT INTO message_classification_attempts (message_id)
               SELECT m FROM UNNEST($1::bigint[]) AS m
               ON CONFLICT (message_id) DO NOTHING
               RETURNING message_id"#,
            &batch_ids
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        if claimed.is_empty() {
            tx.rollback().await?;
            tracing::warn!("batch was already recorded as attempted by another worker; skipping");
            return Ok(0);
        }

        // Prevents LLM-hallucinated message IDs, context ids, duplicates, empty names
        let plan = plan_assignments(&parsed.groups, &claimed);
        let mut relations_inserted = 0u64;
        for assignment in &plan {
            let topic_id = insert_topic(&mut tx, &assignment.topic).await?;
            for &message_id in &assignment.message_ids {
                relations_inserted +=
                    insert_topic_message_relation(&mut tx, topic_id, message_id).await?;
            }
        }
        tx.commit().await?;

        tracing::info!(
            groups = parsed.groups.len(),
            malformed_groups = parsed.malformed,
            topics = plan.len(),
            relations_inserted,
            "applied topic groupings"
        );
        Ok(claimed.len())
    }
}

/// Returns the id of the topic with this name, creating it if needed.
///
/// `topic.name` has no UNIQUE constraint on the current schema, hence the
/// `WHERE NOT EXISTS`; the bare `ON CONFLICT DO NOTHING` keeps this working
/// once a UNIQUE(name) constraint exists (a concurrent insert then makes this
/// statement return no row, which fails the batch and it is retried).
async fn insert_topic(conn: &mut PgConnection, name: &str) -> Result<Uuid, Failure> {
    let id = sqlx::query_scalar!(
        r#"WITH inserted AS (
               INSERT INTO topic (name)
               SELECT $1
               WHERE NOT EXISTS (SELECT 1 FROM topic WHERE name = $1)
               ON CONFLICT DO NOTHING
               RETURNING id
           )
           SELECT id AS "id!" FROM inserted
           UNION ALL
           SELECT id FROM topic WHERE name = $1
           LIMIT 1"#,
        name
    )
    .fetch_one(conn)
    .await?;
    Ok(id)
}

//TODO: make 1 message assignable to more than 1 topic?
/// At most one topic per message; safe with and without a UNIQUE(message_id)
/// constraint on `topic_message_relation`.
async fn insert_topic_message_relation(
    conn: &mut PgConnection,
    topic_id: Uuid,
    message_id: i64,
) -> Result<u64, Failure> {
    Ok(sqlx::query!(
        r#"INSERT INTO topic_message_relation (topic_id, message_id)
           SELECT $1, $2
           WHERE NOT EXISTS (
               SELECT 1 FROM topic_message_relation
               WHERE message_id = $2
           )
           ON CONFLICT DO NOTHING"#,
        topic_id,
        message_id
    )
    .execute(conn)
    .await?
    .rows_affected())
}
