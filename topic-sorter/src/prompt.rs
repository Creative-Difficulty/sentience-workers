//! System prompt for topic grouping.

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
