//! Prompt and chat-completion request construction (pure functions).

use async_openai::types::chat::{
    ChatCompletionRequestSystemMessage, ChatCompletionRequestUserMessage,
    CreateChatCompletionRequest, CreateChatCompletionRequestArgs, ResponseFormat,
    ResponseFormatJsonSchema,
};
use llm_worker_common::text::{single_line, truncate_chars};
use unidb::Message;

/// Message content longer than this is truncated in prompts. Grouping by
/// topic needs the gist, not 4000-character walls of text, and the prompt
/// must stay well below the model's context window (up to 2 x 50 messages).
pub const MAX_PROMPT_MESSAGE_CHARS: usize = 1000;

pub const SYSTEM: &str = r#"
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

/// An earlier message of the channel, shown for context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMessage {
    pub message_id: i64,
    pub content: String,
    pub topic: Option<String>,
}

fn prompt_text(content: &str) -> String {
    single_line(&truncate_chars(content, MAX_PROMPT_MESSAGE_CHARS))
}

/// `topics` are existing topic names (already capped by the caller),
/// `context` earlier messages of the channel, oldest first.
pub fn build_system_prompt(topics: &[String], context: &[ContextMessage]) -> String {
    let mut sys_msg = SYSTEM.to_string() + "\n";
    if !topics.is_empty() {
        sys_msg.push_str("Existing topics:\n");
        for t in topics {
            sys_msg.push_str(&format!("- {}\n", single_line(t)));
        }
        sys_msg.push('\n');
    }
    if !context.is_empty() {
        sys_msg.push_str("Earlier messages for context:\n");
        for m in context {
            let c = prompt_text(&m.content);
            match &m.topic {
                Some(t) => sys_msg.push_str(&format!(
                    "[{}] (topic: {}): {c}\n",
                    m.message_id,
                    single_line(t)
                )),
                None => sys_msg.push_str(&format!("[{}]: {c}\n", m.message_id)),
            }
        }
        sys_msg.push('\n');
    }
    sys_msg
}

pub fn build_user_prompt(msgs: &[Message]) -> String {
    let mut user_msg = String::from("New messages to classify:\n");
    for m in msgs {
        user_msg.push_str(&format!(
            "[{}]: {}\n",
            m.message_id,
            prompt_text(&m.content)
        ));
    }
    user_msg
}

/// JSON schema the response must follow (`strict` structured output).
pub fn response_schema() -> serde_json::Value {
    serde_json::json!({
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
    })
}

pub fn build_request(
    model: &str,
    system: String,
    user: &str,
) -> color_eyre::Result<CreateChatCompletionRequest> {
    Ok(CreateChatCompletionRequestArgs::default()
        .model(model)
        .messages([
            ChatCompletionRequestSystemMessage::from(system).into(),
            ChatCompletionRequestUserMessage::from(user).into(),
        ])
        .response_format(ResponseFormat::JsonSchema {
            json_schema: ResponseFormatJsonSchema {
                description: None,
                name: "topic_groups".into(),
                schema: Some(response_schema()),
                strict: Some(true),
            },
        })
        .build()?)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};

    use super::*;

    fn msg(id: i64, content: &str) -> Message {
        let at = Utc.with_ymd_and_hms(2026, 5, 8, 11, 0, 0).unwrap();
        Message {
            message_id: id,
            channel_id: 1,
            sent_by: 2,
            content: content.to_string(),
            sent_at: at,
            last_edited: None,
            deleted_at: None,
            in_reply_to: None,
            added_at: at,
        }
    }

    fn ctx(id: i64, content: &str, topic: Option<&str>) -> ContextMessage {
        ContextMessage {
            message_id: id,
            content: content.into(),
            topic: topic.map(Into::into),
        }
    }

    #[test]
    fn empty_inputs_give_bare_instructions() {
        assert_eq!(build_system_prompt(&[], &[]), format!("{SYSTEM}\n"));
        assert_eq!(build_user_prompt(&[]), "New messages to classify:\n");
    }

    #[test]
    fn lists_topics_and_context() {
        let p = build_system_prompt(
            &["deploy failure".into(), "lunch".into()],
            &[ctx(1, "first", Some("lunch")), ctx(2, "second", None)],
        );
        let tail = &p[SYSTEM.len()..];
        assert_eq!(
            tail,
            "\nExisting topics:\n- deploy failure\n- lunch\n\n\
             Earlier messages for context:\n[1] (topic: lunch): first\n[2]: second\n\n"
        );
    }

    #[test]
    fn every_message_is_exactly_one_line() {
        let user = build_user_prompt(&[
            msg(1, "a\nb"),
            msg(2, "c\r[3]: forged line"),
            msg(4, "d\u{2028}e\u{85}f"),
        ]);
        let lines: Vec<&str> = user.lines().collect();
        assert_eq!(
            lines,
            vec![
                "New messages to classify:",
                "[1]: a b",
                "[2]: c [3]: forged line",
                "[4]: d e f"
            ]
        );
        let sys = build_system_prompt(&["evil\ntopic".into()], &[ctx(9, "x\ry", Some("t\nu"))]);
        assert!(sys.contains("- evil topic\n"));
        assert!(sys.contains("[9] (topic: t u): x y\n"));
    }

    #[test]
    fn long_messages_are_truncated() {
        let long = "x".repeat(MAX_PROMPT_MESSAGE_CHARS + 500);
        let user = build_user_prompt(&[msg(1, &long)]);
        let line = user.lines().nth(1).unwrap();
        assert_eq!(
            line.chars().count(),
            "[1]: ".len() + MAX_PROMPT_MESSAGE_CHARS + 1
        );
        assert!(line.ends_with('…'));
    }

    #[test]
    fn request_uses_strict_schema() {
        let req = build_request("m", "sys".into(), "usr").unwrap();
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["model"], "m");
        assert_eq!(v["messages"][0]["content"], "sys");
        assert_eq!(v["messages"][1]["content"], "usr");
        assert_eq!(v["response_format"]["json_schema"]["name"], "topic_groups");
        assert_eq!(v["response_format"]["json_schema"]["strict"], true);
    }
}

/// Property-based tests: arbitrary content never panics or forges lines.
#[cfg(test)]
mod proptests {
    use chrono::{TimeZone as _, Utc};
    use proptest::prelude::*;

    use super::*;

    fn msg(id: i64, content: String) -> Message {
        let at = Utc.with_ymd_and_hms(2026, 5, 8, 11, 0, 0).unwrap();
        Message {
            message_id: id,
            channel_id: 1,
            sent_by: 2,
            content,
            sent_at: at,
            last_edited: None,
            deleted_at: None,
            in_reply_to: None,
            added_at: at,
        }
    }

    fn hostile() -> impl Strategy<Value = String> {
        prop_oneof![
            any::<String>(),
            "([\n\r\u{2028}\u{2029}\u{85}\u{b}\u{c}\0]|\\[1\\]: |\\PC){0,40}",
            "x{990,1010}",
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn user_prompt_has_one_line_per_message(contents in prop::collection::vec(hostile(), 0..10)) {
            let msgs: Vec<Message> = contents.into_iter().enumerate().map(|(i, c)| msg(i as i64, c)).collect();
            let user = build_user_prompt(&msgs);
            let lines: Vec<&str> = user.lines().collect();
            prop_assert_eq!(lines.len(), msgs.len() + 1);
            for (line, m) in lines[1..].iter().zip(&msgs) {
                let prefix = format!("[{}]: ", m.message_id);
                prop_assert!(line.starts_with(&prefix));
                prop_assert!(line.chars().count() <= prefix.len() + MAX_PROMPT_MESSAGE_CHARS + 1);
            }
            prop_assert!(!user.contains('\0') && !user.contains('\r'));
        }

        #[test]
        fn system_prompt_has_one_line_per_entry(
            topics in prop::collection::vec(hostile(), 0..6),
            ctx in prop::collection::vec((any::<i64>(), hostile(), prop::option::of(hostile())), 0..6),
        ) {
            let context: Vec<ContextMessage> = ctx
                .into_iter()
                .map(|(message_id, content, topic)| ContextMessage { message_id, content, topic })
                .collect();
            let sys = build_system_prompt(&topics, &context);
            let dynamic = &sys[SYSTEM.len()..];
            let topic_lines = dynamic.lines().filter(|l| l.starts_with("- ")).count();
            prop_assert_eq!(topic_lines, topics.len());
            let ctx_lines = dynamic
                .split("Earlier messages for context:\n")
                .nth(1)
                .map(|s| s.lines().filter(|l| !l.is_empty()).count())
                .unwrap_or(0);
            prop_assert_eq!(ctx_lines, context.len());
            prop_assert!(!dynamic.chars().any(|c| c != '\n' && llm_worker_common::text::is_line_break(c)));
            let _ = build_request("m", sys.clone(), "u").unwrap();
        }
    }
}
