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

/// Property-based tests: parsing is total, and planned assignments always
/// satisfy the write invariants.
#[cfg(test)]
mod proptests {
    use std::collections::{HashMap, HashSet};

    use proptest::prelude::*;
    use serde_json::{Value, json};

    use super::*;

    fn arb_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            any::<u64>().prop_map(Value::from),
            (-1e300..1e300f64).prop_map(Value::from),
            any::<String>().prop_map(Value::from),
        ];
        leaf.prop_recursive(4, 64, 8, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
                prop::collection::btree_map(
                    prop_oneof![
                        Just("groups".to_string()),
                        Just("topic".to_string()),
                        Just("message_ids".to_string()),
                        "[a-z]{1,8}",
                    ],
                    inner,
                    0..6
                )
                .prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

    fn topic() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => "[a-zA-Z ]{0,12}",
            1 => any::<String>(),
            1 => "[ \t\n\r\0\u{2028}]{0,4}",
            1 => Just("Rust".to_string()),
            1 => Just(" rust ".to_string()),
        ]
    }

    fn groups() -> impl Strategy<Value = Vec<Group>> {
        prop::collection::vec(
            (topic(), prop::collection::vec(-3i64..30, 0..10))
                .prop_map(|(topic, message_ids)| Group { topic, message_ids }),
            0..8,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        #[test]
        fn parse_never_panics_on_strings(raw in any::<String>()) {
            let _ = parse_llm_response(&raw);
        }

        #[test]
        fn parse_never_panics_on_json(v in arb_json()) {
            let _ = parse_llm_response(&v.to_string());
            let _ = parse_llm_response(&json!({ "groups": [v] }).to_string());
        }

        #[test]
        fn parse_accounts_for_every_entry(entries in prop::collection::vec(arb_json(), 0..10)) {
            let n = entries.len();
            let p = parse_llm_response(&json!({ "groups": entries }).to_string()).unwrap();
            prop_assert_eq!(p.groups.len() + p.malformed, n);
        }

        #[test]
        fn valid_groups_round_trip(gs in groups()) {
            let p = parse_llm_response(&json!({ "groups": gs }).to_string()).unwrap();
            prop_assert_eq!(p.malformed, 0);
            prop_assert_eq!(p.groups, gs);
        }

        #[test]
        fn normalized_topics_are_canonical(raw in any::<String>()) {
            if let Some(t) = normalize_topic(&raw) {
                prop_assert!(!t.is_empty());
                prop_assert!(t.chars().count() <= MAX_TOPIC_CHARS);
                prop_assert!(!t.chars().any(char::is_control), "{:?}", t);
                prop_assert_eq!(t.trim(), t.as_str());
                prop_assert!(!t.contains("  "));
                // idempotent: a stored name maps to itself
                prop_assert_eq!(normalize_topic(&t), Some(t.clone()));
            } else {
                prop_assert!(raw.chars().all(|c| c.is_whitespace() || c.is_control()));
            }
        }

        #[test]
        fn assignments_satisfy_write_invariants(
            gs in groups(),
            valid in prop::collection::hash_set(0i64..25, 0..25),
        ) {
            let plan = plan_assignments(&gs, &valid);
            let mut seen = HashSet::new();
            let mut names = HashSet::new();
            for a in &plan {
                prop_assert!(!a.message_ids.is_empty(), "orphan topic {:?}", a.topic);
                prop_assert_eq!(normalize_topic(&a.topic), Some(a.topic.clone()));
                prop_assert!(names.insert(a.topic.clone()), "duplicate topic {:?}", a.topic);
                for id in &a.message_ids {
                    prop_assert!(valid.contains(id), "id {} not in the batch", id);
                    prop_assert!(seen.insert(*id), "id {} assigned twice", id);
                }
            }
            // completeness: every valid id listed under a usable name gets the
            // topic of the first group that lists it
            let mut expected: HashMap<i64, String> = HashMap::new();
            for g in &gs {
                if let Some(t) = normalize_topic(&g.topic) {
                    for id in &g.message_ids {
                        if valid.contains(id) {
                            expected.entry(*id).or_insert_with(|| t.clone());
                        }
                    }
                }
            }
            let actual: HashMap<i64, String> = plan
                .iter()
                .flat_map(|a| a.message_ids.iter().map(move |id| (*id, a.topic.clone())))
                .collect();
            prop_assert_eq!(actual, expected);
        }
    }
}
