//! Parsing and validation of the LLM response, and mapping to database rows.
//!
//! The LLM is untrusted input: even with a strict JSON schema, providers
//! return out-of-range numbers, empty strings, NUL bytes, inverted date
//! ranges, fractional levels or unknown kinds. Nothing coming from the model
//! reaches the database without passing [`sanitize`], whose output always
//! satisfies the invariants checked by [`CleanRecord::check_invariants`].

use chrono::{DateTime, Datelike as _, Utc};
use color_eyre::eyre::eyre;
use llm_worker_common::text::{strip_code_fence, strip_nul};
use serde::{Deserialize, Serialize};
use unidb::{ActivityRecordType, ActivitySource, FactAndActivityEvidence, UserFactAndActivity};
use uuid::Uuid;

/// One record as returned by the model (before validation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedRecord {
    pub kind: ActivityRecordType,
    #[serde(rename = "type")]
    pub type_str: String,
    pub value: String,
    pub confidence: f64,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub ended_at: Option<DateTime<Utc>>,
    /// Integer per the schema; fractional values are accepted and rounded.
    #[serde(default)]
    pub level: Option<f64>,
    #[serde(default)]
    pub is_current: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RawResponse {
    records: Vec<serde_json::Value>,
}

/// Result of parsing the model's answer.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParsedResponse {
    pub records: Vec<ExtractedRecord>,
    /// Entries of `records` that did not match the record shape (dropped).
    pub malformed: usize,
}

/// Parse the raw LLM response content.
///
/// The envelope (`{"records": [...]}`) must be valid, otherwise the whole
/// response is rejected (the caller retries the message). Individual records
/// that do not match the record shape are dropped and counted, so one bad
/// record does not discard the good ones.
pub fn parse_llm_response(raw: &str) -> color_eyre::Result<ParsedResponse> {
    let body = strip_code_fence(raw);
    let envelope: RawResponse = serde_json::from_str(body).map_err(|e| {
        eyre!(
            "failed to parse LLM JSON for fact extraction: {e}\nraw: {}",
            llm_worker_common::text::truncate_chars(raw, 500)
        )
    })?;
    let mut parsed = ParsedResponse::default();
    for value in envelope.records {
        match serde_json::from_value::<ExtractedRecord>(value) {
            Ok(r) => parsed.records.push(r),
            Err(e) => {
                tracing::warn!(error = %e, "dropping malformed record from LLM response");
                parsed.malformed += 1;
            }
        }
    }
    Ok(parsed)
}

/// A record that is safe to store.
#[derive(Debug, Clone, PartialEq)]
pub struct CleanRecord {
    pub kind: ActivityRecordType,
    pub type_str: String,
    pub value: String,
    /// In `[0, 1]`; also used as the evidence weight.
    pub confidence: f32,
    pub reasoning: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    /// `Some(0..=10)` for skills only.
    pub level: Option<i16>,
    pub is_current: Option<bool>,
}

impl CleanRecord {
    /// The invariants the database relies on (and some it should enforce
    /// with CHECK constraints but does not). Returns the first violation.
    pub fn check_invariants(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err(format!("confidence {} outside [0, 1]", self.confidence));
        }
        if self.type_str.trim().is_empty() || self.value.trim().is_empty() {
            return Err("empty type or value".into());
        }
        if [&self.type_str, &self.value, &self.reasoning]
            .iter()
            .any(|s| s.contains('\0'))
        {
            return Err("NUL byte in text".into());
        }
        match (self.kind, self.level) {
            (_, None) => {}
            (ActivityRecordType::Skill, Some(l)) if (0..=10).contains(&l) => {}
            (k, Some(l)) => return Err(format!("level {l} on {k:?}")),
        }
        for d in [self.started_at, self.ended_at].into_iter().flatten() {
            if !year_in_range(d) {
                return Err(format!("date {d} out of range"));
            }
        }
        if let (Some(s), Some(e)) = (self.started_at, self.ended_at)
            && e < s
        {
            return Err(format!("ended_at {e} before started_at {s}"));
        }
        Ok(())
    }
}

/// Dates outside years 1..=9999 are model garbage (and Postgres rejects
/// years before 4713 BC outright).
fn year_in_range(d: DateTime<Utc>) -> bool {
    (1..=9999).contains(&d.year())
}

fn clean_date(d: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    d.filter(|d| year_in_range(*d))
}

fn clean_text(s: &str) -> String {
    strip_nul(s).trim().to_string()
}

/// Validate and normalise one record. Returns `None` when the record must be
/// dropped (empty type/value, non-finite confidence).
///
/// * confidence is clamped to `[0, 1]`
/// * `level` is rounded and clamped to `0..=10`, and only kept for skills
/// * NUL bytes are removed and type/value trimmed
/// * dates outside years 1..=9999 are discarded
/// * activities: a missing `started_at` defaults to the message time (as
///   before); `is_current` is kept as returned (the prompt asks for it)
/// * an `ended_at` before `started_at`: when `started_at` was defaulted it is
///   moved to `ended_at` (a past point event), otherwise the two are swapped
pub fn sanitize(record: &ExtractedRecord, message_sent_at: DateTime<Utc>) -> Option<CleanRecord> {
    let type_str = clean_text(&record.type_str);
    let value = clean_text(&record.value);
    if type_str.is_empty() || value.is_empty() || !record.confidence.is_finite() {
        return None;
    }
    let confidence = record.confidence.clamp(0.0, 1.0) as f32;
    let level = record
        .level
        .filter(|l| l.is_finite())
        .map(|l| l.round().clamp(0.0, 10.0) as i16);
    let started = clean_date(record.started_at);
    let ended = clean_date(record.ended_at);

    let (started_at, ended_at, level, is_current) = match record.kind {
        ActivityRecordType::Activity => {
            let (s, e) = match (started, ended) {
                (None, Some(e)) if e < message_sent_at => (e, Some(e)),
                (None, e) => (message_sent_at, e),
                (Some(s), Some(e)) if e < s => (e, Some(s)),
                (Some(s), e) => (s, e),
            };
            (Some(s), e, None, record.is_current)
        }
        // TODO implement outdated skill detection and setting is_current to false on old skill records
        // or just remove the is_current thing entirely
        ActivityRecordType::Skill => (
            Some(started.unwrap_or(message_sent_at)),
            None,
            level,
            Some(record.is_current.unwrap_or(true)),
        ),
        ActivityRecordType::Fact | ActivityRecordType::Emotion => {
            (None, None, None, Some(record.is_current.unwrap_or(true)))
        }
    };

    Some(CleanRecord {
        kind: record.kind,
        type_str,
        value,
        confidence,
        reasoning: strip_nul(&record.reasoning),
        started_at,
        ended_at,
        level,
        is_current,
    })
}

pub fn build_fact(user_id: Uuid, record: &CleanRecord) -> UserFactAndActivity {
    UserFactAndActivity {
        id: Uuid::new_v4(),
        user_id,
        record_type: record.kind,
        source: ActivitySource::LlmExtraction,
        type_str: record.type_str.clone(),
        value: record.value.clone(),
        confidence: record.confidence,
        level: record.level,
        score_id: None,
        started_at: record.started_at,
        ended_at: record.ended_at,
        is_current: record.is_current,
        created_at: Utc::now(),
        type_value_embedding: None,
    }
}

pub fn build_evidence(
    fact_id: Uuid,
    message_id: i64,
    record: &CleanRecord,
) -> FactAndActivityEvidence {
    FactAndActivityEvidence {
        id: Uuid::new_v4(),
        fact_or_activity_id: fact_id,
        message_id: Some(message_id),
        youtube_comment_id: None,
        external_content_id: None,
        discord_presence_id: None,
        weight: record.confidence,
        reasoning: record.reasoning.clone(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;
    use serde_json::json;

    use super::*;

    fn sent() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 8, 12, 0, 0).unwrap()
    }

    fn rec(kind: ActivityRecordType) -> ExtractedRecord {
        ExtractedRecord {
            kind,
            type_str: "location".into(),
            value: "Berlin".into(),
            confidence: 0.9,
            reasoning: "r".into(),
            started_at: None,
            ended_at: None,
            level: None,
            is_current: None,
        }
    }

    fn day(d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn parses_prompt_example() {
        let raw = r#"{"records":[{"kind":"skill","type":"programming_language","value":"Rust",
            "confidence":0.97,"reasoning":"x","started_at":"2021-01-01T00:00:00Z","ended_at":null,
            "level":7,"is_current":true}]}"#;
        let p = parse_llm_response(raw).unwrap();
        assert_eq!(p.malformed, 0);
        assert_eq!(p.records.len(), 1);
        assert_eq!(p.records[0].level, Some(7.0));
        assert_eq!(p.records[0].kind, ActivityRecordType::Skill);
    }

    #[test]
    fn envelope_errors_are_errors_bad_records_are_dropped() {
        assert!(parse_llm_response("").is_err());
        assert!(parse_llm_response("{").is_err());
        assert!(parse_llm_response("[]").is_err());
        assert!(parse_llm_response(r#"{"records": null}"#).is_err());
        let p = parse_llm_response(
            &json!({"records": [
                {"kind": "opinion", "type": "x", "value": "y", "confidence": 0.9},
                {"kind": "fact", "type": "x", "value": "y", "confidence": "high"},
                42,
                {"kind": "fact", "type": "x", "value": "y", "confidence": 0.9}
            ]})
            .to_string(),
        )
        .unwrap();
        assert_eq!((p.records.len(), p.malformed), (1, 3));
    }

    #[test]
    fn accepts_code_fences() {
        let p = parse_llm_response("```json\n{\"records\": []}\n```").unwrap();
        assert!(p.records.is_empty());
    }

    #[test]
    fn clamps_confidence_and_level() {
        let mut r = rec(ActivityRecordType::Skill);
        r.confidence = 1.7;
        r.level = Some(15.0);
        let c = sanitize(&r, sent()).unwrap();
        assert_eq!((c.confidence, c.level), (1.0, Some(10)));
        r.confidence = -0.2;
        r.level = Some(-3.0);
        let c = sanitize(&r, sent()).unwrap();
        assert_eq!((c.confidence, c.level), (0.0, Some(0)));
        r.level = Some(3.5);
        assert_eq!(sanitize(&r, sent()).unwrap().level, Some(4));
        // non-finite confidence cannot be clamped meaningfully: the record is dropped
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            r.confidence = bad;
            assert!(sanitize(&r, sent()).is_none(), "{bad}");
        }
        r.level = Some(f64::NAN);
        r.confidence = 0.5;
        assert_eq!(sanitize(&r, sent()).unwrap().level, None);
    }

    #[test]
    fn level_only_for_skills() {
        let mut r = rec(ActivityRecordType::Fact);
        r.level = Some(5.0);
        assert_eq!(sanitize(&r, sent()).unwrap().level, None);
        r.kind = ActivityRecordType::Activity;
        assert_eq!(sanitize(&r, sent()).unwrap().level, None);
    }

    #[test]
    fn drops_empty_type_or_value_and_strips_nul() {
        let mut r = rec(ActivityRecordType::Fact);
        r.type_str = "  ".into();
        assert!(sanitize(&r, sent()).is_none());
        let mut r = rec(ActivityRecordType::Fact);
        r.value = "\0".into();
        assert!(sanitize(&r, sent()).is_none());
        let mut r = rec(ActivityRecordType::Fact);
        r.value = " Ber\0lin ".into();
        r.reasoning = "a\0b".into();
        let c = sanitize(&r, sent()).unwrap();
        assert_eq!((c.value.as_str(), c.reasoning.as_str()), ("Berlin", "ab"));
    }

    #[test]
    fn activity_dates_and_is_current() {
        let mut r = rec(ActivityRecordType::Activity);
        r.is_current = Some(false);
        let c = sanitize(&r, sent()).unwrap();
        assert_eq!(
            c.started_at,
            Some(sent()),
            "missing start defaults to message time"
        );
        assert_eq!(c.is_current, Some(false), "is_current kept for activities");

        r.started_at = Some(day(10));
        r.ended_at = Some(day(1));
        let c = sanitize(&r, sent()).unwrap();
        assert_eq!(
            (c.started_at, c.ended_at),
            (Some(day(1)), Some(day(10))),
            "swapped"
        );

        r.started_at = None;
        r.ended_at = Some(day(3));
        let c = sanitize(&r, sent()).unwrap();
        assert_eq!((c.started_at, c.ended_at), (Some(day(3)), Some(day(3))));

        r.started_at = Some(Utc.with_ymd_and_hms(-5000, 1, 1, 0, 0, 0).unwrap());
        r.ended_at = None;
        assert_eq!(sanitize(&r, sent()).unwrap().started_at, Some(sent()));
    }

    #[test]
    fn mapping_per_kind_matches_previous_behaviour() {
        let mut r = rec(ActivityRecordType::Skill);
        r.ended_at = Some(day(1));
        let c = sanitize(&r, sent()).unwrap();
        assert_eq!(
            (c.started_at, c.ended_at, c.is_current),
            (Some(sent()), None, Some(true))
        );
        let mut r = rec(ActivityRecordType::Fact);
        r.started_at = Some(day(1));
        let c = sanitize(&r, sent()).unwrap();
        assert_eq!(
            (c.started_at, c.ended_at, c.is_current),
            (None, None, Some(true))
        );
    }

    #[test]
    fn rows_carry_clean_values() {
        let mut r = rec(ActivityRecordType::Skill);
        r.level = Some(6.0);
        let c = sanitize(&r, sent()).unwrap();
        c.check_invariants().unwrap();
        let user = Uuid::new_v4();
        let fact = build_fact(user, &c);
        let ev = build_evidence(fact.id, 99, &c);
        assert_eq!(fact.user_id, user);
        assert_eq!(fact.level, Some(6));
        assert_eq!(fact.source, ActivitySource::LlmExtraction);
        assert_eq!(ev.fact_or_activity_id, fact.id);
        assert_eq!(ev.message_id, Some(99));
        assert_eq!(ev.weight, fact.confidence);
    }
}
