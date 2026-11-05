//! fact-extractor: structured records with arbitrary floats (NaN, inf,
//! subnormals), any representable dates and any strings -> sanitize.
#![no_main]

use arbitrary::Arbitrary;
use chrono::{DateTime, Datelike as _, TimeZone as _, Utc};
use fact_extractor::records::{ExtractedRecord, sanitize};
use libfuzzer_sys::fuzz_target;
use unidb::ActivityRecordType;

#[derive(Debug, Arbitrary)]
struct Input {
    kind: u8,
    type_str: String,
    value: String,
    confidence: f64,
    reasoning: String,
    started_at: Option<(i64, u32)>,
    ended_at: Option<(i64, u32)>,
    level: Option<f64>,
    is_current: Option<bool>,
    sent_at: (i64, u32),
}

fn date((secs, nanos): (i64, u32)) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(secs, nanos % 1_000_000_000).single()
}

fuzz_target!(|input: Input| {
    // The message time comes from the database (a Discord timestamp) and is
    // trusted; sanitize only defaults to it. Model-supplied dates are arbitrary.
    let Some(sent_at) = date(input.sent_at).filter(|d| (1..=9999).contains(&d.year())) else {
        return;
    };
    let record = ExtractedRecord {
        kind: match input.kind % 4 {
            0 => ActivityRecordType::Activity,
            1 => ActivityRecordType::Fact,
            2 => ActivityRecordType::Skill,
            _ => ActivityRecordType::Emotion,
        },
        type_str: input.type_str,
        value: input.value,
        confidence: input.confidence,
        reasoning: input.reasoning,
        started_at: input.started_at.and_then(date),
        ended_at: input.ended_at.and_then(date),
        level: input.level,
        is_current: input.is_current,
    };
    if let Some(clean) = sanitize(&record, sent_at)
        && let Err(violation) = clean.check_invariants()
    {
        panic!("invariant violated: {violation}\n{record:?}\n{clean:?}");
    }
});
