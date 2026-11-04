//! Parsing of the LLM response and mapping of records to database rows.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use unidb::{ActivityRecordType, ActivitySource, UserFactAndActivity};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ExtractedRecord {
    pub kind: ActivityRecordType,
    #[serde(rename = "type")]
    pub type_str: String,
    pub value: String,
    pub confidence: f32,
    pub reasoning: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub level: Option<i16>,
    pub is_current: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct LLMResponse {
    pub records: Vec<ExtractedRecord>,
}

/// Parse the raw LLM response content into records.
pub fn parse_llm_response(raw: &str) -> color_eyre::Result<Vec<ExtractedRecord>> {
    Ok(serde_json::from_str::<LLMResponse>(raw)
        .map_err(|e| {
            color_eyre::eyre::eyre!("failed to parse LLM JSON for fact extraction: {e}\nraw: {raw}")
        })?
        .records)
}

pub fn build_fact(
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
