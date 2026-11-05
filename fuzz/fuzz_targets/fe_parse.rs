//! fact-extractor: raw LLM response -> parse -> sanitize -> rows.
//! Must never panic; every sanitized record must satisfy the DB invariants.
#![no_main]

use chrono::{TimeZone as _, Utc};
use fact_extractor::records::{build_evidence, build_fact, parse_llm_response, sanitize};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let raw = String::from_utf8_lossy(data);
    let Ok(parsed) = parse_llm_response(&raw) else {
        return;
    };
    let sent_at = Utc.with_ymd_and_hms(2026, 5, 8, 12, 0, 0).unwrap();
    for record in &parsed.records {
        if let Some(clean) = sanitize(record, sent_at) {
            if let Err(violation) = clean.check_invariants() {
                panic!("invariant violated: {violation}\n{record:?}\n{clean:?}");
            }
            let fact = build_fact(uuid::Uuid::nil(), &clean);
            let ev = build_evidence(fact.id, 1, &clean);
            assert!((0.0..=1.0).contains(&ev.weight));
        }
    }
});
