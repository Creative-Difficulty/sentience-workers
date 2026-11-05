//! Structure-aware fuzzing of the Discord -> DB mapping layer: the fuzzer drives the fields of a
//! gateway message (ids, kind, reference, attachments, stickers, reactions, author), the JSON is
//! deserialized with serenity exactly like a gateway payload, and every mapping invariant the DB
//! layer relies on is checked.
#![no_main]

use std::collections::HashSet;

use arbitrary::Arbitrary;
use ingest_vestibule_retriever::db::stale_username;
use ingest_vestibule_retriever::mapping::*;
use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};
use serenity::all::{GuildId, Message, MessageType, MessageUpdateEvent};

#[derive(Arbitrary, Debug)]
struct Reference {
    kind: u8,
    message_id: Option<u64>,
    channel_id: u64,
    guild_id: Option<u64>,
}

#[derive(Arbitrary, Debug)]
struct Input {
    id: u64,
    channel: u64,
    guild: Option<u64>,
    author: u64,
    author_name: String,
    global_name: Option<String>,
    avatar: Option<(bool, [u8; 16])>,
    content: String,
    kind: u8,
    edited: bool,
    reference: Option<Reference>,
    attachments: Vec<(u64, String, Option<String>)>,
    stickers: Vec<(u64, u8)>,
    reactions: Vec<(Option<u64>, String, bool)>,
    update_content: Option<String>,
    update_kind: Option<u8>,
}

fn nz(v: u64) -> u64 {
    v.max(1)
}

fn build(i: &Input) -> Value {
    let avatar = i.avatar.map(|(animated, bytes)| {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        if animated { format!("a_{hex}") } else { hex }
    });
    let mut v = json!({
        "id": nz(i.id).to_string(),
        "channel_id": nz(i.channel).to_string(),
        "guild_id": i.guild.map(|g| nz(g).to_string()),
        "author": {"id": nz(i.author).to_string(), "username": i.author_name,
                   "global_name": i.global_name, "discriminator": "0", "avatar": avatar, "bot": false},
        "content": i.content,
        "timestamp": "2024-01-02T03:04:05.000000+00:00",
        "edited_timestamp": if i.edited { json!("2024-01-02T04:04:05.000000+00:00") } else { Value::Null },
        "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [],
        "embeds": [], "pinned": false, "type": i.kind,
        "attachments": i.attachments.iter().take(8).map(|(id, name, ct)| json!({
            "id": nz(*id).to_string(), "filename": name, "size": 1,
            "url": format!("https://cdn.discordapp.com/attachments/{}", nz(*id)),
            "proxy_url": "https://media.discordapp.net/x", "content_type": ct,
        })).collect::<Vec<_>>(),
        "sticker_items": i.stickers.iter().take(4).map(|(id, f)| json!({
            "id": nz(*id).to_string(), "name": "s", "format_type": f,
        })).collect::<Vec<_>>(),
    });
    if let Some(r) = &i.reference {
        v["message_reference"] = json!({
            "type": r.kind, "message_id": r.message_id.map(|m| nz(m).to_string()),
            "channel_id": nz(r.channel_id).to_string(), "guild_id": r.guild_id.map(|g| nz(g).to_string()),
        });
    }
    v
}

fuzz_target!(|i: Input| {
    let Ok(msg) = serde_json::from_value::<Message>(build(&i)) else {
        return;
    };

    // --- message row -------------------------------------------------------------------------
    let row = message_row(&msg);
    assert_eq!(row.message_id, nz(i.id) as i64);
    assert_eq!(row.sent_by, nz(i.author) as i64);
    let kind = MessageType::from(i.kind);
    if is_system_kind(kind) {
        assert_eq!(Some(row.content.clone()), system_content_for(kind, &msg.author.name));
        assert!(!row.content.is_empty());
    } else {
        assert_eq!(row.content, msg.content);
    }
    assert_eq!(row.last_edited.is_some(), i.edited);

    // --- reply parent ------------------------------------------------------------------------
    let parent = reply_parent(&msg);
    assert_eq!(row.in_reply_to, parent.map(|(_, id)| id.get() as i64));
    if let Some((ch, id)) = parent {
        let r = i.reference.as_ref().expect("reply without reference");
        assert_eq!(r.kind, 0, "forward/unknown reference treated as reply");
        assert_eq!(ch.get(), nz(r.channel_id));
        assert_eq!(Some(id.get()), r.message_id.map(nz));
        if let (Some(a), Some(b)) = (r.guild_id, i.guild) {
            assert_eq!(nz(a), nz(b), "cross-guild reference treated as reply");
        }
    }

    // --- media specs -------------------------------------------------------------------------
    let mut keys = HashSet::new();
    for a in &msg.attachments {
        let spec = attachment_spec(&msg, a);
        assert_eq!(
            spec.object_key,
            format!("discord/attachments/{}/{}", msg.id.get(), a.id.get())
        );
        assert!(is_plausible_mime(&spec.content_type), "unsafe content type {:?}", spec.content_type);
        match a.content_type.as_deref() {
            Some(ct) if is_plausible_mime(ct) => assert_eq!(spec.content_type, ct),
            _ => assert_eq!(spec.content_type, "application/octet-stream"),
        }
        keys.insert((a.id, spec.object_key));
    }
    let ids: HashSet<_> = msg.attachments.iter().map(|a| a.id).collect();
    assert_eq!(keys.len(), ids.len(), "attachment object keys collide");

    for s in &msg.sticker_items {
        if let Some(spec) = sticker_spec(s) {
            assert!(spec.object_key.starts_with("discord/stickers/"));
            let ext = spec.object_key.rsplit('.').next().unwrap();
            assert!(spec.url.ends_with(ext));
            assert!(spec.content_type.ends_with(if ext == "json" { "json" } else { ext }));
        }
    }

    if let Some(spec) = avatar_spec(&msg.author) {
        let hash = msg.author.avatar.as_ref().unwrap().to_string();
        assert_eq!(spec.object_key, format!("discord/avatars/{}/{}", msg.author.id.get(), hash));
        assert_eq!(spec.content_type == "image/gif", hash.starts_with("a_"));
    }

    // --- user row ----------------------------------------------------------------------------
    let u = user_row(&msg.author);
    assert_eq!(u.username, msg.author.name);
    assert_eq!(&u.display_name, msg.author.global_name.as_ref().unwrap_or(&msg.author.name));
    let stale = stale_username(&u.username, u.discord_user_id);
    assert!(stale.contains('#') && stale != u.username);

    // --- reactions ---------------------------------------------------------------------------
    for (custom, name, animated) in i.reactions.iter().take(4) {
        let emoji = match custom {
            Some(id) => json!({"id": nz(*id).to_string(), "name": name, "animated": animated}),
            None => json!({"id": null, "name": name}),
        };
        let Ok(r) = serde_json::from_value::<serenity::all::ReactionType>(emoji) else {
            continue;
        };
        let key = emoji_key(&r);
        let spec = emoji_spec(&r, msg.guild_id.or(i.guild.map(|g| GuildId::new(nz(g)))));
        assert_eq!(key, spec.as_ref().map(|s| s.discord_emoji_id.clone()));
        if let Some(s) = spec {
            assert_eq!(s.asset.is_some(), custom.is_some());
        }
    }

    // --- MESSAGE_UPDATE ----------------------------------------------------------------------
    let ev = json!({
        "id": nz(i.id).to_string(), "channel_id": nz(i.channel).to_string(),
        "content": i.update_content, "type": i.update_kind,
        "edited_timestamp": "2024-01-02T05:00:00.000000+00:00",
    });
    if let Ok(ev) = serde_json::from_value::<MessageUpdateEvent>(ev) {
        let out = edit_content(ev.kind, ev.content.as_deref());
        if ev.kind.is_some_and(is_system_kind) {
            assert_eq!(out, None);
        } else {
            assert_eq!(out, ev.content);
        }
    }
});
