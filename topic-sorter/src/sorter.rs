use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use color_eyre::eyre::OptionExt;
use sqlx::PgPool;
use uuid::Uuid;

use crate::llm;

const BATCH_SIZE: i64 = 50;
const CONTEXT_SIZE: i64 = 100;
const IDLE_SLEEP: Duration = Duration::from_secs(5);

struct Row {
    message_id: i64,
    channel_id: i64,
    content: String,
    sent_at: DateTime<Utc>,
}

#[tracing::instrument(skip_all)]
pub async fn run(
    pool: &PgPool,
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> color_eyre::Result<()> {
    loop {
        let batch = sqlx::query_as!(
            Row,
            r#"SELECT m.message_id, m.channel_id, m.content, m.sent_at
               FROM messages m
               WHERE NOT EXISTS (
                   SELECT 1 FROM message_classification_attempts mca
                   WHERE mca.message_id = m.message_id
               )
                 AND m.content != '[deleted message]'
               ORDER BY m.channel_id, m.sent_at
               LIMIT $1"#,
            BATCH_SIZE
        )
        .fetch_all(pool)
        .await?;

        if batch.is_empty() {
            tracing::debug!("no unclassified messages, waiting...");
            tokio::time::sleep(IDLE_SLEEP).await;
            continue;
        }

        tracing::info!(count = batch.len(), "processing batch");

        let mut msgs_by_channel: HashMap<i64, Vec<Row>> = HashMap::new();
        for message in batch {
            msgs_by_channel
                .entry(message.channel_id)
                .or_default()
                .push(message);
        }

        let mut attempted: Vec<i64> = Vec::new();
        for (channel_id, msgs) in &msgs_by_channel {
            match classify_channel(pool, http, base_url, api_key, model, *channel_id, msgs).await {
                Ok(()) => attempted.extend(msgs.iter().map(|m| m.message_id)),
                Err(e) => tracing::error!(channel_id, error = %e, "channel batch failed"),
            }
        }

        if attempted.is_empty() {
            tokio::time::sleep(IDLE_SLEEP).await;
        } else {
            sqlx::query!(
                r#"INSERT INTO message_classification_attempts (message_id)
                   SELECT m FROM UNNEST($1::bigint[]) AS m
                   ON CONFLICT (message_id) DO NOTHING"#,
                &attempted
            )
            .execute(pool)
            .await?;
        }
    }
}

#[tracing::instrument(skip_all, fields(channel_id, batch_len = msgs.len()))]
async fn classify_channel(
    pool: &PgPool,
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    channel_id: i64,
    msgs: &[Row],
) -> color_eyre::Result<()> {
    let oldest = msgs
        .iter()
        .map(|m| m.sent_at)
        .min()
        .ok_or_eyre("Empty batch of messages passed")?;

    let mut context: Vec<(i64, String, Option<String>)> = sqlx::query!(
        r#"SELECT m.message_id, m.content, t.name AS "topic?"
           FROM messages m
           LEFT JOIN topic_message_relation tmr ON tmr.message_id = m.message_id
           LEFT JOIN topic t ON t.id = tmr.topic_id
           WHERE m.channel_id = $1 AND m.sent_at < $2 AND m.deleted_at IS NULL
           ORDER BY m.sent_at DESC
           LIMIT $3"#,
        channel_id,
        oldest,
        CONTEXT_SIZE
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.message_id, r.content, r.topic))
    .collect();

    context.reverse();

    let topics: Vec<String> = sqlx::query_scalar!("SELECT name FROM topic")
        .fetch_all(pool)
        .await?;
    let new: Vec<(i64, String)> = msgs
        .iter()
        .map(|m| (m.message_id, m.content.clone()))
        .collect();

    let groups = llm::classify(http, base_url, api_key, model, &context, &new, &topics).await?;
    tracing::debug!(group_count = groups.len(), "LLM returned groups");
    if groups.is_empty() {
        tracing::debug!("no groups returned, skipping DB writes");
        return Ok(());
    }

    let names: Vec<String> = groups.iter().map(|(t, _)| t.clone()).collect();
    tracing::debug!(?names, "ensuring topics exist");

    let topic_map: HashMap<String, Uuid> = sqlx::query!(
        r#"WITH new_topics AS (
               INSERT INTO topic (name)
               SELECT DISTINCT n FROM UNNEST($1::text[]) AS n
               WHERE NOT EXISTS (SELECT 1 FROM topic WHERE name = n)
               RETURNING id, name
           )
           SELECT id AS "id!", name AS "name!" FROM new_topics
           UNION ALL
           SELECT id, name FROM topic WHERE name = ANY($1)"#,
        &names
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.name, r.id))
    .collect();

    tracing::debug!(topic_map_len = topic_map.len(), "topic map built");

    let mut tids: Vec<Uuid> = Vec::new();
    let mut mids: Vec<i64> = Vec::new();
    for (topic, ids) in &groups {
        match topic_map.get(topic.as_str()) {
            Some(&tid) => {
                tracing::debug!(%topic, msg_count = ids.len(), "queuing assignments");
                for &id in ids {
                    tids.push(tid);
                    mids.push(id);
                }
            }
            None => tracing::warn!(%topic, "topic not found in map after upsert"),
        }
    }

    if tids.is_empty() {
        tracing::debug!("no assignments to write");
        return Ok(());
    }

    tracing::debug!(pair_count = tids.len(), "inserting topic_message_relation rows");
    let result = sqlx::query!(
        r#"INSERT INTO topic_message_relation (topic_id, message_id)
           SELECT t.topic_id, t.message_id
           FROM UNNEST($1::uuid[], $2::bigint[]) AS t(topic_id, message_id)
           WHERE NOT EXISTS (
               SELECT 1 FROM topic_message_relation r
               WHERE r.message_id = t.message_id
           )"#,
        &tids,
        &mids
    )
    .execute(pool)
    .await?;

    tracing::info!(rows_affected = result.rows_affected(), "inserted topic_message_relations");
    Ok(())
}
