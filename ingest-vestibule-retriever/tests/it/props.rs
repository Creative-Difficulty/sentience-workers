//! Property tests of the pure mapping layer (no DB, no network).
use std::collections::HashSet;

use ingest_vestibule_retriever::db::stale_username;
use ingest_vestibule_retriever::mapping::*;
use ingest_vestibule_retriever::media::MediaLimits;
use proptest::prelude::*;
use serde_json::{Value, json};
use serenity::all::{GuildChannel, Message, MessageType, ReactionType, StickerItem, User};

const MAX_SNOWFLAKE: u64 = i64::MAX as u64;

fn id() -> impl Strategy<Value = u64> {
    1..=MAX_SNOWFLAKE
}

/// Discord usernames: 2-32 chars of lowercase letters, digits, `_` and `.`.
fn username() -> impl Strategy<Value = String> {
    "[a-z0-9_.]{2,32}"
}

fn user_json(id: u64, name: &str, global: Option<&str>) -> Value {
    json!({"id": id.to_string(), "username": name, "global_name": global,
           "discriminator": "0", "avatar": null, "bot": false})
}

#[derive(Debug, Clone)]
struct Ref {
    kind: u8,
    message_id: Option<u64>,
    channel_id: u64,
    guild_id: Option<u64>,
}

fn reference() -> impl Strategy<Value = Option<Ref>> {
    proptest::option::of(
        (
            prop_oneof![Just(0u8), Just(1u8), 2..=255u8],
            proptest::option::of(id()),
            id(),
            proptest::option::of(prop_oneof![Just(42u64), id()]),
        )
            .prop_map(|(kind, message_id, channel_id, guild_id)| Ref {
                kind,
                message_id,
                channel_id,
                guild_id,
            }),
    )
}

#[derive(Debug, Clone)]
struct Msg {
    id: u64,
    channel: u64,
    guild: Option<u64>,
    author: u64,
    author_name: String,
    content: String,
    kind: u8,
    reference: Option<Ref>,
}

fn msg() -> impl Strategy<Value = Msg> {
    (
        id(),
        id(),
        proptest::option::of(prop_oneof![Just(42u64), id()]),
        id(),
        username(),
        any::<String>(),
        any::<u8>(),
        reference(),
    )
        .prop_map(
            |(id, channel, guild, author, author_name, content, kind, reference)| Msg {
                id,
                channel,
                guild,
                author,
                author_name,
                content,
                kind,
                reference,
            },
        )
}

fn build(m: &Msg) -> Message {
    let mut v = json!({
        "id": m.id.to_string(), "channel_id": m.channel.to_string(),
        "guild_id": m.guild.map(|g| g.to_string()),
        "author": user_json(m.author, &m.author_name, None),
        "content": m.content, "timestamp": "2024-01-02T03:04:05.000000+00:00",
        "edited_timestamp": null, "tts": false, "mention_everyone": false, "mentions": [],
        "mention_roles": [], "attachments": [], "embeds": [], "pinned": false, "type": m.kind,
    });
    if let Some(r) = &m.reference {
        v["message_reference"] = json!({
            "type": r.kind, "message_id": r.message_id.map(|i| i.to_string()),
            "channel_id": r.channel_id.to_string(), "guild_id": r.guild_id.map(|g| g.to_string()),
        });
    }
    serde_json::from_value(v).expect("generated message must deserialize")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn message_row_maps_ids_and_content(m in msg()) {
        let message = build(&m);
        let row = message_row(&message);
        prop_assert_eq!(row.message_id, m.id as i64);
        prop_assert_eq!(row.channel_id, m.channel as i64);
        prop_assert_eq!(row.sent_by, m.author as i64);
        let kind = MessageType::from(m.kind);
        if is_system_kind(kind) {
            prop_assert!(!row.content.is_empty());
            prop_assert_eq!(Some(row.content.clone()), system_content_for(kind, &m.author_name));
        } else {
            prop_assert_eq!(&row.content, &m.content);
        }
    }

    /// A reply link exists exactly for default references with a message id that do not point
    /// into another guild, and always names the referenced channel.
    #[test]
    fn reply_parent_only_for_same_guild_default_references(m in msg()) {
        let message = build(&m);
        let parent = reply_parent(&message);
        let expected = m.reference.as_ref().and_then(|r| {
            let other_guild = matches!((r.guild_id, m.guild), (Some(a), Some(b)) if a != b);
            (r.kind == 0 && !other_guild).then_some(r.message_id).flatten().map(|mid| (r.channel_id, mid))
        });
        prop_assert_eq!(parent.map(|(c, i)| (c.get(), i.get())), expected);
        prop_assert_eq!(message_row(&message).in_reply_to, expected.map(|(_, i)| i as i64));
    }

    #[test]
    fn edit_content_never_overwrites_system_content(kind in any::<u8>(), content in proptest::option::of(any::<String>())) {
        let kind = MessageType::from(kind);
        let out = edit_content(Some(kind), content.as_deref());
        if is_system_kind(kind) {
            prop_assert_eq!(out, None);
        } else {
            prop_assert_eq!(out, content.clone());
        }
        // unknown kind (partial update event without `type`) behaves like a user message
        prop_assert_eq!(edit_content(None, content.as_deref()), content);
    }

    #[test]
    fn snowflake_preserves_order_and_sign(a in id(), b in id()) {
        prop_assert!(snowflake(a) > 0);
        prop_assert_eq!(a.cmp(&b), snowflake(a).cmp(&snowflake(b)));
    }

    #[test]
    fn attachment_keys_are_unique_and_safe(pairs in proptest::collection::vec((id(), id()), 1..20)) {
        let mut keys = HashSet::new();
        let distinct: HashSet<_> = pairs.iter().cloned().collect();
        for (mid, aid) in &distinct {
            let msg: Message = serde_json::from_value(json!({
                "id": mid.to_string(), "channel_id": "1", "author": user_json(1, "aa", None),
                "content": "", "timestamp": "2024-01-02T03:04:05.000000+00:00",
                "edited_timestamp": null, "tts": false, "mention_everyone": false, "mentions": [],
                "mention_roles": [], "attachments": [{"id": aid.to_string(), "filename": "../../x",
                "size": 1, "url": "https://cdn/x", "proxy_url": "https://cdn/x"}], "embeds": [],
                "pinned": false, "type": 0
            })).unwrap();
            let spec = attachment_spec(&msg, &msg.attachments[0]);
            prop_assert!(spec.object_key.starts_with("discord/attachments/"));
            prop_assert!(!spec.object_key.contains(".."), "filename leaked into key: {}", spec.object_key);
            prop_assert_eq!(&spec.content_type, "application/octet-stream");
            keys.insert(spec.object_key);
        }
        prop_assert_eq!(keys.len(), distinct.len());
    }

    #[test]
    fn sticker_spec_matches_format(sid in id(), format in any::<u8>()) {
        let s: StickerItem = serde_json::from_value(json!({"id": sid.to_string(), "name": "s", "format_type": format})).unwrap();
        let spec = sticker_spec(&s);
        prop_assert_eq!(spec.is_some(), (1..=4).contains(&format));
        if let Some(spec) = spec {
            let ext = spec.object_key.rsplit('.').next().unwrap().to_string();
            prop_assert_eq!(spec.object_key.clone(), format!("discord/stickers/{sid}.{ext}"));
            let suffix = format!("/{sid}.{ext}");
            prop_assert!(spec.url.ends_with(&suffix));
            let expected_ct = if ext == "json" { "application/json".to_string() } else { format!("image/{ext}") };
            prop_assert_eq!(spec.content_type, expected_ct);
        }
    }

    #[test]
    fn emoji_key_matches_spec(custom in any::<bool>(), eid in id(), name in "\\PC{1,8}", animated in any::<bool>(), guild in proptest::option::of(id())) {
        let r: ReactionType = if custom {
            serde_json::from_value(json!({"id": eid.to_string(), "name": name, "animated": animated})).unwrap()
        } else {
            ReactionType::Unicode(name.clone())
        };
        let spec = emoji_spec(&r, guild.map(serenity::all::GuildId::new)).unwrap();
        prop_assert_eq!(Some(spec.discord_emoji_id.clone()), emoji_key(&r));
        if custom {
            prop_assert_eq!(spec.discord_emoji_id, eid.to_string());
            prop_assert_eq!(spec.from_guild, guild.map(|g| g.to_string()));
            prop_assert_eq!(spec.asset.unwrap().object_key, format!("discord/emojis/{eid}.webp"));
            prop_assert_eq!(spec.is_animated, animated);
        } else {
            prop_assert_eq!(spec.discord_emoji_id, name);
            prop_assert!(spec.asset.is_none() && spec.from_guild.is_none());
        }
    }

    #[test]
    fn user_row_display_name_falls_back_to_username(uid in id(), name in username(), global in proptest::option::of("\\PC{1,32}")) {
        let u: User = serde_json::from_value(user_json(uid, &name, global.as_deref())).unwrap();
        let row = user_row(&u);
        prop_assert_eq!(row.discord_user_id, uid as i64);
        prop_assert_eq!(&row.username, &name);
        prop_assert_eq!(&row.display_name, global.as_ref().unwrap_or(&name));
        prop_assert_eq!(row.nickname, row.display_name);
    }

    /// The renamed username of a stale account can never collide with a real username, and
    /// distinct accounts never get the same stale name.
    #[test]
    fn stale_usernames_never_collide(a in username(), b in username(), ia in id(), ib in id()) {
        let sa = stale_username(&a, ia as i64);
        prop_assert!(sa.contains('#'));
        prop_assert!(sa != b);
        if (a.clone(), ia) != (b.clone(), ib) {
            prop_assert_ne!(sa, stale_username(&b, ib as i64));
        }
    }

    #[test]
    fn channel_row_never_panics_and_keeps_parent(cid in id(), kind in any::<u8>(), parent in proptest::option::of(id())) {
        let ch: GuildChannel = serde_json::from_value(json!({
            "id": cid.to_string(), "type": kind, "guild_id": "42", "name": "c",
            "parent_id": parent.map(|p| p.to_string()), "position": 0,
            "permission_overwrites": [], "nsfw": false
        })).unwrap();
        let row = channel_row(&ch);
        prop_assert_eq!(row.channel_id, cid as i64);
        prop_assert_eq!(row.parent_channel_id, parent.map(|p| p as i64));
    }

    #[test]
    fn media_limits_parse_any_u64(n in any::<u64>(), secs in any::<u64>(), pad in " {0,3}") {
        let l = MediaLimits::from_lookup(|k| match k {
            "MEDIA_MAX_BYTES" => Some(format!("{pad}{n}{pad}")),
            "MEDIA_DOWNLOAD_TIMEOUT_SECS" => Some(secs.to_string()),
            _ => None,
        }).unwrap();
        prop_assert_eq!(l.max_bytes, n);
        prop_assert_eq!(l.timeout, std::time::Duration::from_secs(secs));
        prop_assert_eq!(l.connect_timeout, MediaLimits::default().connect_timeout);
    }

    #[test]
    fn media_limits_reject_non_numbers(s in "[^0-9 ]{1,10}|-[0-9]{1,5}") {
        prop_assert!(MediaLimits::from_lookup(|k| (k == "MEDIA_MAX_BYTES").then(|| s.clone())).is_err());
    }
}
