//! Parsing and validation of the LLM's topic groups.
//!
//! The model's answer is untrusted: topic names may be empty, padded,
//! differently cased, contain NUL bytes or newlines; message ids may be
//! invented, belong to context messages, repeat, or appear in several groups.
//! [`plan_assignments`] turns the raw groups into a set of `(topic, ids)`
//! assignments that is safe to write.

use std::collections::{HashMap, HashSet};

use color_eyre::eyre::eyre;
use llm_worker_common::text::{strip_code_fence, truncate_chars};
use serde::{Deserialize, Serialize};

/// Topic names longer than this (in characters) are truncated.
pub const MAX_TOPIC_CHARS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub topic: String,
    pub message_ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct RawResponse {
    groups: Vec<serde_json::Value>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedResponse {
    pub groups: Vec<Group>,
    /// Entries of `groups` that did not match the group shape (dropped).
    pub malformed: usize,
}

/// Parse the raw LLM content. An invalid envelope is an error; individual
/// malformed groups are dropped and counted.
pub fn parse_llm_response(raw: &str) -> color_eyre::Result<ParsedResponse> {
    let envelope: RawResponse = serde_json::from_str(strip_code_fence(raw)).map_err(|e| {
        eyre!(
            "failed to parse LLM JSON for a message group under one topic: {e}\nraw: {}",
            truncate_chars(raw, 500)
        )
    })?;
    let mut parsed = ParsedResponse::default();
    for value in envelope.groups {
        match serde_json::from_value::<Group>(value) {
            Ok(g) => parsed.groups.push(g),
            Err(e) => {
                tracing::warn!(error = %e, "dropping malformed group from LLM response");
                parsed.malformed += 1;
            }
        }
    }
    Ok(parsed)
}

/// Canonical form of a topic name: control characters (incl. NUL and line
/// breaks) become spaces, whitespace runs collapse, lowercase, at most
/// [`MAX_TOPIC_CHARS`] characters. `None` when nothing is left.
///
/// The prompt asks for lowercase labels and for reusing existing ones; this
/// makes "Rust  Borrow Checker" and "rust borrow checker" the same topic.
pub fn normalize_topic(raw: &str) -> Option<String> {
    let spaced: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let lowered = collapsed.to_lowercase();
    let name: String = lowered.chars().take(MAX_TOPIC_CHARS).collect();
    let name = name.trim_end().to_string();
    (!name.is_empty()).then_some(name)
}

/// One topic and the messages to attach to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub topic: String,
    pub message_ids: Vec<i64>,
}

/// Turn raw groups into assignments:
/// * names are normalised; groups with an empty name are dropped;
/// * groups whose names normalise to the same topic are merged;
/// * only ids in `valid_ids` (the batch being classified) are kept —
///   hallucinated ids and context messages are discarded;
/// * every message gets at most one topic (the first group that lists it);
/// * topics left without messages are dropped (no orphan topics).
///
/// Order of topics and ids follows the model's answer.
pub fn plan_assignments(groups: &[Group], valid_ids: &HashSet<i64>) -> Vec<Assignment> {
    let mut out: Vec<Assignment> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut taken: HashSet<i64> = HashSet::new();
    for group in groups {
        let Some(topic) = normalize_topic(&group.topic) else {
            tracing::warn!(topic = ?group.topic, "LLM returned an empty topic name, skipping group");
            continue;
        };
        for &id in &group.message_ids {
            if !valid_ids.contains(&id) {
                tracing::warn!(
                    message_id = id,
                    topic = %topic,
                    "LLM returned unknown message_id, skipping"
                );
                continue;
            }
            if !taken.insert(id) {
                continue;
            }
            let i = *index.entry(topic.clone()).or_insert_with(|| {
                out.push(Assignment {
                    topic: topic.clone(),
                    message_ids: Vec::new(),
                });
                out.len() - 1
            });
            out[i].message_ids.push(id);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(topic: &str, ids: &[i64]) -> Group {
        Group {
            topic: topic.into(),
            message_ids: ids.to_vec(),
        }
    }

    #[test]
    fn parses_prompt_example() {
        let p = parse_llm_response(
            r#"{"groups":[{"topic":"ramen recommendations","message_ids":[1,3,6]},{"topic":"deploy failure","message_ids":[4,5,8]}]}"#,
        )
        .unwrap();
        assert_eq!(p.groups.len(), 2);
        assert_eq!(p.groups[1], g("deploy failure", &[4, 5, 8]));
    }

    #[test]
    fn envelope_errors_and_malformed_groups() {
        for bad in ["", "null", "[]", "{\"groups\": {}}", "{\"groups\": ["] {
            assert!(parse_llm_response(bad).is_err(), "{bad:?}");
        }
        let p = parse_llm_response(
            r#"{"groups":[{"topic":"a","message_ids":[1.5]},{"topic":1,"message_ids":[]},
                {"topic":"b","message_ids":[18446744073709551615]},{"topic":"ok","message_ids":[1]}]}"#,
        )
        .unwrap();
        assert_eq!((p.groups.len(), p.malformed), (1, 3));
        assert!(parse_llm_response("```json\n{\"groups\": []}\n```").is_ok());
    }

    #[test]
    fn normalisation() {
        assert_eq!(
            normalize_topic("  Rust   Borrow\nChecker ").as_deref(),
            Some("rust borrow checker")
        );
        assert_eq!(normalize_topic("null\0byte").as_deref(), Some("null byte"));
        assert_eq!(normalize_topic("tab\there").as_deref(), Some("tab here"));
        assert_eq!(normalize_topic(" \n\t\0 "), None);
        assert_eq!(normalize_topic(""), None);
        let long = "a".repeat(300);
        assert_eq!(
            normalize_topic(&long).unwrap().chars().count(),
            MAX_TOPIC_CHARS
        );
        assert_eq!(normalize_topic("ÄRGER").as_deref(), Some("ärger"));
    }

    #[test]
    fn assignments_filter_merge_and_dedupe() {
        let valid: HashSet<i64> = [1, 2, 3, 4, 5].into();
        let plan = plan_assignments(
            &[
                g("Lunch", &[1, 2, 99, 2]),
                g("   ", &[3]),
                g("lunch", &[4]),
                g("deploy", &[1, 5]),
                g("ghost", &[42, 43]),
            ],
            &valid,
        );
        assert_eq!(
            plan,
            vec![
                Assignment {
                    topic: "lunch".into(),
                    message_ids: vec![1, 2, 4]
                },
                Assignment {
                    topic: "deploy".into(),
                    message_ids: vec![5]
                },
            ]
        );
    }
}
