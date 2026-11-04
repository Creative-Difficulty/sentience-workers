//! System prompt and chat-completion request construction.

use async_openai::types::chat::{
    ChatCompletionRequestSystemMessage, ChatCompletionRequestUserMessage,
    CreateChatCompletionRequest, CreateChatCompletionRequestArgs, ResponseFormat,
    ResponseFormatJsonSchema,
};
use chrono::Utc;
use unidb::Message;

pub const SYSTEM: &str = r#"
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

pub fn build_system_prompt(
    context: &[Message],
    reply_target: Option<&Message>,
    focal_msg: &Message,
    has_attachments: bool,
    server_name: String,
    channel_name: Option<String>,
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

    if let Some(ref name) = channel_name {
        prompt.push_str(&format!(
            "\n# Discord channel name\n\
             The focal message was sent in the Discord channel named \"{}\". \
             This string may appear in messages or other contexts and should ONLY be interpreted \
             as the name of the Discord channel the message was sent in — never as a fact, skill, \
             or activity about the author.\n",
            name,
        ));
    } else {
        tracing::error!(
            msg_id = focal_msg.message_id,
            "Channel that the focal message was sent in is not in the database"
        );
    }

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

pub fn build_extract_llm_request(
    model: &str,
    focal_msg: &Message,
    context: &[Message],
    reply_target: Option<&Message>,
    has_attachments: bool,
    server_name: String,
    channel_name: Option<String>,
) -> color_eyre::Result<CreateChatCompletionRequest> {
    let request = CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages([
            ChatCompletionRequestSystemMessage::from(build_system_prompt(
                context,
                reply_target,
                focal_msg,
                has_attachments,
                server_name,
                channel_name,
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
