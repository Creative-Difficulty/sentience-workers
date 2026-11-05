//! Raw-bytes fuzzing: arbitrary gateway-like JSON for every payload type the ingester maps.
//! Whatever serenity accepts must map without panicking.
#![no_main]

use ingest_vestibule_retriever::mapping::*;
use ingest_vestibule_retriever::media::MediaLimits;
use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use serenity::all::{GuildChannel, Message, MessageUpdateEvent, Reaction, User};

/// serenity 0.12.5 `ImageHash::from_str` panics on 32/34-byte non-ASCII strings (byte slicing;
/// pinned by `serenity_image_hash_panics_on_non_ascii` in tests/it/mapping.rs). Discord never
/// sends such hashes, so inputs containing one are skipped to keep fuzzing our own code.
fn has_non_ascii_hash_like(v: &Value) -> bool {
    match v {
        Value::String(s) => (s.len() == 32 || s.len() == 34) && !s.is_ascii(),
        Value::Array(a) => a.iter().any(has_non_ascii_hash_like),
        Value::Object(o) => o.values().any(has_non_ascii_hash_like),
        _ => false,
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(value) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    if has_non_ascii_hash_like(&value) {
        return;
    }
    let data = &serde_json::to_vec(&value).unwrap()[..];
    if let Ok(msg) = serde_json::from_slice::<Message>(data) {
        let _ = message_row(&msg);
        let _ = reply_parent(&msg);
        let _ = avatar_spec(&msg.author);
        for a in &msg.attachments {
            let _ = attachment_spec(&msg, a);
        }
        for s in &msg.sticker_items {
            let _ = sticker_spec(s);
        }
        for r in &msg.reactions {
            let _ = emoji_spec(&r.reaction_type, msg.guild_id);
        }
    }
    if let Ok(ev) = serde_json::from_slice::<MessageUpdateEvent>(data) {
        let _ = edit_content(ev.kind, ev.content.as_deref());
    }
    if let Ok(r) = serde_json::from_slice::<Reaction>(data) {
        let _ = emoji_spec(&r.emoji, r.guild_id);
        let _ = emoji_key(&r.emoji);
    }
    if let Ok(u) = serde_json::from_slice::<User>(data) {
        let _ = user_row(&u);
        let _ = avatar_spec(&u);
    }
    if let Ok(ch) = serde_json::from_slice::<GuildChannel>(data) {
        let _ = channel_row(&ch);
    }
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = MediaLimits::from_lookup(|_| Some(s.to_string()));
    }
});
