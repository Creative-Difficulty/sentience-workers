use std::time::Duration;

use async_openai::{
    Client,
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionRequestSystemMessage, ChatCompletionRequestUserMessage,
        CreateChatCompletionRequest, CreateChatCompletionRequestArgs, ResponseFormat,
        ResponseFormatJsonSchema,
    },
};
use chrono::{DateTime, Utc};
use color_eyre::eyre::eyre;
use ormlite::Model as _;
use serde::Deserialize;
use sqlx::PgPool;
use unidb::{
    ActivityRecordType, ActivitySource, FactAndActivityEvidence, Message, UserFactAndActivity,
};
use uuid::Uuid;

const SYSTEM: &str = r#"
Return JSON matching the schema below. No prose, no markdown code fences, JSON only.

You extract durable facts, activities, and skills about the AUTHOR of a single chat message, for long-term memory storage.

# Abstention rule (read first)
When in doubt, do NOT emit a record. False positives are worse than false negatives — they corrupt long-term memory. Prefer an empty `records` array over guessing. If a claim is plausible but not clearly supported, drop it.

# Scope
- Extract ONLY information about the message author (first person). Never extract facts about friends, family, coworkers, or third parties mentioned in the message.
- Skip greetings, banter, jokes, opinions about external topics, hypotheticals, questions, and unrelated chatter.
- Preferences ARE extractable as facts ("I prefer Vim", "I'm vegetarian"). Opinions about external things are NOT ("React is overrated").
- Return an empty `records` array if nothing durable is present.

# Record kinds

## fact — stable knowledge about the user
Use for: location, timezone, occupation, employer, real name, languages spoken, dietary preferences, tool/format preferences, demographic info, ongoing health conditions. You should also create your own categories here if the fact does not fit into any of these.
- `started_at`, `ended_at`, `level`, `is_current`: If you can infer these from the message, populate them with the appropriate value, otherwise always null.
- Example `type` values (use snake_case; generate a new `type`value when none of the listed values fit):
  `location`, `timezone`, `occupation`, `employer`, `real_name`, `language_spoken`, `dietary_preference`, `tool_preference`, `format_preference`, `relationship_status`, `health_condition`.

## activity — something the user did, with a timeline
Use for: job changes, trips, events attended, projects completed, life milestones, races run, courses taken.
- `started_at`: when the activity began (or occurred, if a point event). Null if unknown.
- `ended_at`: when it concluded. Null if ongoing or unknown.
- `is_current`: true if still ongoing as of the reference time, false if concluded, null if unknown.
- `level`: always null.
- Canonical `type` values: `job_change`, `travel`, `event_attended`, `project_completed`, `course_taken`, `race_completed`, `relocation`, `life_milestone`.

## skill — a competency the user has
Use for: programming languages, instruments, crafts, sports, spoken languages, professional skills.
- `level`: 0–10 proficiency, REQUIRED (see rubric below).
- `is_current`: true if actively practiced or still known, false if rusty/abandoned, null if unclear.
- `started_at`: when they began learning, if stated. Null otherwise.
- `ended_at`: only if they explicitly stopped practicing.
- Canonical `type` values: `programming_language`, `natural_language`, `instrument`, `craft`, `sport`, `professional_skill`, `software_tool`.

# Date format
All dates use RFC 3339 UTC: `YYYY-MM-DDTHH:MM:SSZ`.
- "March 2024" → `2024-03-01T00:00:00Z`
- "last year" → `<previous year>-01-01T00:00:00Z`
- "yesterday" / "this morning" → resolve against reference time, use `00:00:00Z` if no specific hour.
- "a few years ago", "when I was younger", or anything vaguer than a year → null.
- Never invent precision the message doesn't support.

# Confidence rubric (0.0–1.0)
- 0.95–1.0: Explicit, unambiguous statement ("I live in Berlin", "I'm a nurse").
- 0.75–0.94: Strong implication, single reasonable reading ("Heading home to Berlin after this trip").
- 0.65–0.74: Plausible inference with minor ambiguity.
- Below 0.65: Do not emit. The author may not even mean it.

# Skill level rubric (0–10)
- 1–2: Dabbled, beginner, just started.
- 3–4: Functional, can complete basic tasks.
- 5–6: Competent, works professionally or seriously hobbyist.
- 7–8: Advanced, deep experience, mentors others.
- 9–10: Expert, recognized authority. Reserve for explicit signals (decades of work, professional acclaim).
- Self-claimed levels can be taken at face value but cap INFERRED levels at 6 unless evidence is strong. When unsure, prefer the lower bound.

# Updates and negations
- "I quit my job at Acme" → emit an `activity` of type `job_change` with `ended_at` set and `is_current: true` (because the event of the user leaving their job just happened and has not been overwritten by something else e.g. taking a new job). Do not emit a fact contradicting a previous one; the consumer handles reconciliation.
- "I no longer play guitar" → emit a `skill` with `is_current: false`. Set `level` based on past proficiency if stated, otherwise estimate conservatively.

# The `reasoning` field
Provide a brief (one sentence) paraphrase of the exact span in the message that supports the record. This is for auditing, not chain-of-thought.

# Examples

## Example 1
Reference time: 2026-05-08T12:00:00Z
Message: "Just finished my third marathon in Vienna last Sunday! Living here for 3 years now and still loving it."

Output:
{
  "records": [
    {
      "kind": "activity",
      "type": "race_completed",
      "value": "Vienna marathon (third marathon)",
      "confidence": 0.97,
      "reasoning": "Author states they finished their third marathon in Vienna last Sunday.",
      "started_at": "2026-05-03T00:00:00Z",
      "ended_at": "2026-05-03T00:00:00Z",
      "level": null,
      "is_current": false
    },
    {
      "kind": "fact",
      "type": "location",
      "value": "Vienna",
      "confidence": 0.95,
      "reasoning": "Author says they have been living in Vienna for 3 years.",
      "started_at": null,
      "ended_at": null,
      "level": null,
      "is_current": null
    }
  ]
}

## Example 2
Reference time: 2026-05-08T12:00:00Z
Message: "lol my buddy just got promoted to staff engineer at Google, so jealous"

Output:
{ "records": [] }

(Information is about someone else, not the author. Author's jealousy is an emotion, not durable.)

## Example 3
Reference time: 2026-05-08T12:00:00Z
Message: "Been writing Rust professionally since 2021, mostly backend stuff. Picked up a bit of Zig last month for fun but I'm still really bad at it."

Output:
{
  "records": [
    {
      "kind": "skill",
      "type": "programming_language",
      "value": "Rust",
      "confidence": 0.97,
      "reasoning": "Author has written Rust professionally since 2021.",
      "started_at": "2021-01-01T00:00:00Z",
      "ended_at": null,
      "level": 7,
      "is_current": true
    },
    {
      "kind": "skill",
      "type": "programming_language",
      "value": "Zig",
      "confidence": 0.9,
      "reasoning": "Author picked up Zig last month and self-describes as bad at it.",
      "started_at": "2026-04-01T00:00:00Z",
      "ended_at": null,
      "level": 2,
      "is_current": true
    }
  ]
}

# The current reference time is:"#;

const BATCH_SIZE: i64 = 25;
const IDLE_SLEEP: Duration = Duration::from_secs(5);
const MIN_CONTENT_LEN: i32 = 10;
const CONTEXT_SIZE: i64 = 10;

#[derive(Debug, Deserialize)]
struct ExtractedRecord {
    kind: ActivityRecordType,
    #[serde(rename = "type")]
    type_str: String,
    value: String,
    confidence: f32,
    reasoning: String,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    level: Option<i16>,
    is_current: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct LLMResponse {
    records: Vec<ExtractedRecord>,
}

#[tracing::instrument(skip_all)]
pub async fn run(
    pool: &PgPool,
    client: &Client<OpenAIConfig>,
    model: &str,
    server_name: String,
) -> color_eyre::Result<()> {
    loop {
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
            BATCH_SIZE
        )
        .fetch_all(pool)
        .await?;

        if batch.is_empty() {
            tracing::debug!("no unattempted messages, waiting...");
            tokio::time::sleep(IDLE_SLEEP).await;
            continue;
        }

        tracing::info!(count = batch.len(), "processing batch");

        let mut attempted = Vec::with_capacity(batch.len());
        for msg in batch {
            match process_message(pool, client, model, &msg, server_name.clone()).await {
                Ok(()) => attempted.push(msg.message_id),
                Err(e) => {
                    tracing::error!(message_id = msg.message_id, error = %e, "extraction failed")
                }
            }
        }

        if attempted.is_empty() {
            tokio::time::sleep(IDLE_SLEEP).await;
        } else {
            sqlx::query!(
                r#"INSERT INTO fact_extraction_attempts (message_id)
           SELECT m FROM UNNEST($1::bigint[]) AS m
           ON CONFLICT (message_id) DO NOTHING"#,
                attempted.as_slice()
            )
            .execute(pool)
            .await?;
        }
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

    let request = build_extract_llm_request(
        model,
        msg,
        &context,
        reply_target.as_ref(),
        has_attachments,
        server_name,
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

    let extracted = serde_json::from_str::<LLMResponse>(&raw)
        .map_err(|e| eyre!("failed to parse LLM JSON for fact extraction: {e}\nraw: {raw}"))?
        .records;

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

fn build_system_prompt(
    context: &[Message],
    reply_target: Option<&Message>,
    focal_msg: &Message,
    has_attachments: bool,
    server_name: String,
) -> String {
    let now = Utc::now();
    let mut prompt = format!(
        "{}\nCurrent time: {}\nToday: {}\n\
         \n# Discord server name\n\
         The Discord server you are operating on is named \"{}\". \
         This string may appear in messages or other contexts and should ONLY be interpreted \
         as the name of the Discord server — never as a fact, skill, or activity about the author.\n",
        SYSTEM,
        now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        now.format("%A, %B %-d, %Y"),
        server_name,
    );

    if has_attachments {
        prompt.push_str(
            r#"# Attachments present
             The focal message has one or more attachments (image, video, file, etc.) that you cannot see.
             The author may be captioning, describing, or reacting to the attachment rather than making a first-person claim about themselves or anyone else.
             When something the author says could plausibly be a description of the attachment instead of a durable fact about them, abstain — do NOT emit a record."#,
        );
    }

    if let Some(rt) = reply_target {
        let c = rt.content.replace('\n', " ");
        prompt.push_str(&format!(
            "\n# Replying to\n\
             The focal message is a reply to the message below. The author may be addressing or referencing it directly — \
             facts in the focal message may only make sense in light of it. Do NOT extract facts from the replied-to message itself.\n\
             [{}] sent_by={} at {}: {}\n",
            rt.message_id,
            rt.sent_by,
            rt.sent_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            c,
        ));
    }

    if !context.is_empty() {
        prompt.push_str(&format!(
            "\n# Surrounding messages (context only — do NOT extract from these)\n\
             Nearby messages from the same channel. Use them only to disambiguate references in the focal message. \
             Extract facts ONLY about the focal author (sent_by={}).\n\n",
            focal_msg.sent_by
        ));
        for m in context {
            let c = m.content.replace('\n', " ");
            prompt.push_str(&format!(
                "[{}] sent_by={} at {}: {}\n",
                m.message_id,
                m.sent_by,
                m.sent_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                c,
            ));
        }
    }

    prompt
}

fn build_extract_llm_request(
    model: &str,
    focal_msg: &Message,
    context: &[Message],
    reply_target: Option<&Message>,
    has_attachments: bool,
    server_name: String,
) -> color_eyre::Result<CreateChatCompletionRequest> {
    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages([
            ChatCompletionRequestSystemMessage::from(build_system_prompt(
                context,
                reply_target,
                focal_msg,
                has_attachments,
                server_name
            ))
            .into(),
            ChatCompletionRequestUserMessage::from(focal_msg.content.as_str()).into(),
        ])
        .response_format(ResponseFormat::JsonSchema {
            json_schema: ResponseFormatJsonSchema {
                description: None,
                name: "extracted_records".into(),
                schema: Some(serde_json::json!({
        "type": "object",
        "properties": {
            "records": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["fact", "activity", "skill"]
                        },
                        "type": { "type": "string" },
                        "value": { "type": "string" },
                        "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                        "reasoning": { "type": "string" },
                        "started_at": { "type": ["string", "null"], "format": "date-time" },
                        "ended_at": { "type": ["string", "null"], "format": "date-time" },
                        "level": { "type": ["integer", "null"], "minimum": 0, "maximum": 10 },
                        "is_current": { "type": ["boolean", "null"] }
                    },
                    "required": [
                        "kind", "type", "value", "confidence", "reasoning",
                        "started_at", "ended_at", "level", "is_current"
                    ],
                    "additionalProperties": false
                }
            }
        },
        "required": ["records"],
        "additionalProperties": false
    })),
                strict: Some(true),
            },
        })
        .build()?;
    Ok(request)
}

fn build_fact(
    user_id: Uuid,
    message_sent_at: DateTime<Utc>,
    record: &ExtractedRecord,
) -> UserFactAndActivity {
    let (started_at, ended_at, level, is_current) = match record.kind {
        ActivityRecordType::Activity => (
            record.started_at.or(Some(message_sent_at)),
            record.ended_at,
            None,
            None,
        ),
        // TODO implement outdated skill detection and setting is_current to false on old skill records
        // or just remove the is_current thing entirely
        ActivityRecordType::Skill => (
            record.started_at.or(Some(message_sent_at)),
            None,
            record.level.map(|l| l.clamp(0, 10)),
            Some(record.is_current.unwrap_or(true)),
        ),
        ActivityRecordType::Fact | ActivityRecordType::Emotion => {
            (None, None, None, Some(record.is_current.unwrap_or(true)))
        }
    };

    UserFactAndActivity {
        id: Uuid::new_v4(),
        user_id,
        record_type: record.kind,
        source: ActivitySource::LlmExtraction,
        type_str: record.type_str.clone(),
        value: record.value.clone(),
        confidence: record.confidence,
        level,
        score_id: None,
        started_at,
        ended_at,
        is_current,
        created_at: Utc::now(),
        type_value_embedding: None,
    }
}
