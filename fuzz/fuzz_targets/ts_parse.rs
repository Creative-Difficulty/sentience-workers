//! topic-sorter: raw LLM response -> parse -> plan_assignments.
//! Must never panic; the plan must only contain batch ids, each once, under
//! non-empty canonical topic names.
#![no_main]

use std::collections::HashSet;

use libfuzzer_sys::fuzz_target;
use topic_sorter::response::{normalize_topic, parse_llm_response, plan_assignments};

fuzz_target!(|data: &[u8]| {
    let raw = String::from_utf8_lossy(data);
    let Ok(parsed) = parse_llm_response(&raw) else {
        return;
    };
    // pretend every other mentioned id is part of the batch
    let valid: HashSet<i64> = parsed
        .groups
        .iter()
        .flat_map(|g| g.message_ids.iter().copied())
        .filter(|id| id % 2 == 0)
        .collect();
    let plan = plan_assignments(&parsed.groups, &valid);
    let mut seen = HashSet::new();
    let mut names = HashSet::new();
    for a in &plan {
        assert!(!a.message_ids.is_empty());
        assert_eq!(normalize_topic(&a.topic).as_deref(), Some(a.topic.as_str()));
        assert!(names.insert(a.topic.clone()));
        for id in &a.message_ids {
            assert!(valid.contains(id));
            assert!(seen.insert(*id));
        }
    }
});
