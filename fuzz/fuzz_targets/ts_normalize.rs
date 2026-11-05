//! topic-sorter: topic name normalisation is canonical and idempotent.
#![no_main]

use libfuzzer_sys::fuzz_target;
use topic_sorter::response::{MAX_TOPIC_CHARS, normalize_topic};

fuzz_target!(|raw: &str| {
    if let Some(t) = normalize_topic(raw) {
        assert!(!t.is_empty());
        assert!(t.chars().count() <= MAX_TOPIC_CHARS);
        assert!(!t.chars().any(char::is_control));
        assert_eq!(t.trim(), t);
        assert!(!t.contains("  "));
        assert_eq!(normalize_topic(&t).as_deref(), Some(t.as_str()));
    }
});
