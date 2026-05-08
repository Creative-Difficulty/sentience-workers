use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_openai::{
    Client,
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionRequestSystemMessage, ChatCompletionRequestUserMessage,
        CreateChatCompletionRequestArgs, ResponseFormat, ResponseFormatJsonSchema,
    },
};
use color_eyre::eyre::{OptionExt, eyre};
use serde::Deserialize;
use sqlx::PgPool;
use unidb::Message;
use uuid::Uuid;

const SYSTEM: &str = r#"
Return JSON only. No prose, no code fences.

Group chat messages by topic.

Rules:
- Topic labels: 1–5 words, lowercase, descriptive not restating. E.g. "rust borrow checker", "lunch logistics".
- Reuse the same label for continuations of one topic; don't invent variants.
- Skip greetings, acknowledgments, jokes, one-offs, system messages.
- Only emit a group with 2+ messages. Never emit singletons.
- Each message belongs to at most one group. Group by topic, not adjacency — interleaved threads still group together.
- `message_ids` must use the exact integer IDs from the input. Don't invent, reorder, or omit.

Schema:
{
  "groups": [
    { "topic": "<string, 1-5 words lowercase>", "message_ids": [<int>, <int>, ...] }
  ]
}

If nothing groups, return: { "groups": [] }
No other keys. No null, comments, or trailing commas.

Good example:
Input:
1: "ramen spot near the office?"
2: "morning all"
3: "ichiran on 5th is solid"
4: "deploy failed last night"
5: "migration didn't run"
6: "tonkotsu at menya is better"
7: "thanks!"
8: "rolling back the migration"

Output:
{"groups":[{"topic":"ramen recommendations","message_ids":[1,3,6]},{"topic":"deploy failure","message_ids":[4,5,8]}]}"#;

const BATCH_SIZE: i64 = 50;
const CONTEXT_SIZE: i64 = 50;
const IDLE_SLEEP: Duration = Duration::from_secs(5);

#[tracing::instrument(skip_all)]
pub async fn run(
    pool: &PgPool,
    client: &Client<OpenAIConfig>,
    model: &str,
) -> color_eyre::Result<()> {
    loop {
        let batch = sqlx::query_as!(
            Message,
            r#"SELECT m.* FROM messages m
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

        let mut msgs_by_channel: HashMap<i64, Vec<Message>> = HashMap::new();
        for message in batch {
            msgs_by_channel
                .entry(message.channel_id)
                .or_default()
                .push(message);
        }

        let mut attempted: Vec<i64> = Vec::new();
        for (channel_id, msgs) in &msgs_by_channel {
            match classify_channel(pool, client, model, *channel_id, msgs).await {
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
    client: &Client<OpenAIConfig>,
    model: &str,
    channel_id: i64,
    msgs: &[Message],
) -> color_eyre::Result<()> {
    // fetches CONTEXT_SIZE messages before the current batch with their topics (if already labeled with a topic)
    let mut message_id_content_topic_context_msgs: Vec<(i64, String, Option<String>)> =
        sqlx::query!(
            r#"SELECT m.message_id, m.content, t.name AS "topic?"
           FROM messages m
           LEFT JOIN topic_message_relation tmr ON tmr.message_id = m.message_id
           LEFT JOIN topic t ON t.id = tmr.topic_id
           WHERE m.channel_id = $1 AND m.sent_at < $2 AND m.deleted_at IS NULL
           ORDER BY m.sent_at DESC
           LIMIT $3"#,
            channel_id,
            msgs.iter()
                .map(|m| m.sent_at)
                .min()
                .ok_or_eyre("Empty batch of messages passed")?,
            CONTEXT_SIZE
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| (r.message_id, r.content, r.topic))
        .collect();

    // We asked the db for the last CONTEXT_SIZE before the current batch, so they are new-old, but we want chronoloigical old-new order to make it easier for the LLM to process
    message_id_content_topic_context_msgs.reverse();

    // This section is just adding to the system message (giving the LLM context)
    let topics: Vec<String> = sqlx::query_scalar!("SELECT name FROM topic")
        .fetch_all(pool)
        .await?;

    let mut sys_msg = SYSTEM.to_string() + "\n";
    if !topics.is_empty() {
        sys_msg.push_str("Existing topics:\n");
        for t in &topics {
            sys_msg.push_str(&format!("- {t}\n"));
        }
        sys_msg.push('\n');
    }
    if !message_id_content_topic_context_msgs.is_empty() {
        sys_msg.push_str("Earlier messages for context:\n");
        for (id, content, topic) in &message_id_content_topic_context_msgs {
            let c = content.replace('\n', " ");
            match topic {
                Some(t) => sys_msg.push_str(&format!("[{id}] (topic: {t}): {c}\n")),
                None => sys_msg.push_str(&format!("[{id}]: {c}\n")),
            }
        }
        sys_msg.push('\n');
    }

    let mut user_msg = String::new();
    user_msg.push_str("New messages to classify:\n");
    for m in msgs {
        user_msg.push_str(&format!(
            "[{}]: {}\n",
            m.message_id,
            m.content.replace('\n', " ")
        ));
    }

    tracing::debug!(%model, user_msg_len = user_msg.len(), "sending classify request");

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "groups": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "topic": { "type": "string" },
                        "message_ids": { "type": "array", "items": { "type": "integer" } },
                    },
                    "required": ["topic", "message_ids"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["groups"],
        "additionalProperties": false
    });

    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages([
            ChatCompletionRequestSystemMessage::from(sys_msg).into(),
            ChatCompletionRequestUserMessage::from(user_msg.as_str()).into(),
        ])
        .response_format(ResponseFormat::JsonSchema {
            json_schema: ResponseFormatJsonSchema {
                description: None,
                name: "topic_groups".into(),
                schema: Some(schema),
                strict: Some(true),
            },
        })
        .build()?;

    let llm_response = client
        .chat()
        .create(request)
        .await?
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| eyre!("no content in LLM response"))?;

    tracing::debug!(
        content = llm_response.chars().take(50).collect::<String>(),
        "received LLM response"
    );

    #[derive(Debug, Deserialize)]
    pub struct LLMResponse {
        pub groups: Vec<Group>,
    }

    #[derive(Debug, Deserialize)]
    pub struct Group {
        pub topic: String,
        pub message_ids: Vec<i64>,
    }

    let groups: Vec<Group> = serde_json::from_str::<LLMResponse>(&llm_response)
        .map_err(|e| eyre!("failed to parse LLM JSON for a message group under one topic: {e}\nraw: {llm_response}"))?
        .groups;

    if groups.is_empty() {
        tracing::debug!("LLM returned no groups");
        return Ok(());
    }

    tracing::debug!(group_count = groups.len(), "applying groups");

    // Prevent LLM-hallucinated message IDs
    let valid_ids: HashSet<i64> = msgs.iter().map(|m| m.message_id).collect();

    let mut relations_inserted = 0;
    for group in groups {
        let topic_id = insert_topic(pool, &group.topic).await?;
        for message_id in group.message_ids {
            if !valid_ids.contains(&message_id) {
                tracing::warn!(
                    message_id,
                    topic = %group.topic,
                    "LLM returned unknown message_id, skipping"
                );
                continue;
            }
            insert_topic_message_relation(pool, topic_id, message_id).await?;
            relations_inserted += 1;
        }
    }

    tracing::info!(relations_inserted, "applied topic groupings");
    Ok(())
}

// TODO does this error out on topic already exists or just returns the existing topics id?
async fn insert_topic(pool: &PgPool, name: &str) -> color_eyre::Result<Uuid> {
    let id = sqlx::query_scalar!(
        r#"WITH inserted AS (
               INSERT INTO topic (name)
               SELECT $1
               WHERE NOT EXISTS (SELECT 1 FROM topic WHERE name = $1)
               RETURNING id
           )
           SELECT id AS "id!" FROM inserted
           UNION ALL
           SELECT id FROM topic WHERE name = $1
           LIMIT 1"#,
        name
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

//TODO: make 1 message assignable to more than 1 topic?
async fn insert_topic_message_relation(
    pool: &PgPool,
    topic_id: Uuid,
    message_id: i64,
) -> color_eyre::Result<()> {
    sqlx::query!(
        r#"INSERT INTO topic_message_relation (topic_id, message_id)
           SELECT $1, $2
           WHERE NOT EXISTS (
               SELECT 1 FROM topic_message_relation
               WHERE message_id = $2
           )"#,
        topic_id,
        message_id
    )
    .execute(pool)
    .await?;
    Ok(())
}
