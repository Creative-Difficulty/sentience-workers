//! Unit tests for the pure Discord -> row mapping functions (no DB needed).

use crate::common::*;
use ingest_vestibule_retriever::mapping::*;
use serde_json::json;
use serenity::all::{ReactionType, StickerItem};

fn kind_message(kind: u8, author: &str, content: &str) -> serenity::all::Message {
    let mut v = message_json(10, CHANNEL, user_json(5, author), content);
    v["type"] = json!(kind);
    message(v)
}

#[test]
fn regular_messages_keep_their_content() {
    for kind in [0u8, 19, 20, 23] {
        let m = kind_message(kind, "alice", "hello");
        assert_eq!(system_message_content(&m), None, "kind {kind}");
        assert_eq!(message_content(&m), "hello");
    }
}

#[test]
fn system_messages_get_synthetic_content() {
    let cases = [
        (7u8, "user alice joined the server"),
        (6, "alice pinned a message"),
        (8, "alice boosted the server"),
        (9, "the server reached Nitro Boost tier 1"),
        (12, "alice followed a news channel"),
        (18, "alice created a thread"),
        (21, "[thread starter message]"),
        (24, "[auto-moderation action]"),
        (44, "alice made a purchase"),
    ];
    for (kind, expected) in cases {
        let m = kind_message(kind, "alice", "");
        assert_eq!(
            system_message_content(&m).as_deref(),
            Some(expected),
            "kind {kind}"
        );
        assert_eq!(message_content(&m), expected);
    }
}

#[test]
fn unknown_message_kinds_keep_content() {
    let m = kind_message(250, "alice", "raw");
    assert_eq!(system_message_content(&m), None);
    assert_eq!(message_content(&m), "raw");
}

#[test]
fn message_row_maps_ids_and_timestamps() {
    let mut v = message_json(42, CHANNEL, user_json(7, "bob"), "hi");
    v["edited_timestamp"] = json!("2024-01-02T03:05:00.000000+00:00");
    v["message_reference"] = json!({"message_id": "41", "channel_id": CHANNEL.to_string()});
    let row = message_row(&message(v));
    assert_eq!(row.message_id, 42);
    assert_eq!(row.channel_id, CHANNEL as i64);
    assert_eq!(row.sent_by, 7);
    assert_eq!(row.content, "hi");
    assert_eq!(row.sent_at.to_rfc3339(), "2024-01-02T03:04:05+00:00");
    assert_eq!(
        row.last_edited.map(|t| t.to_rfc3339()).as_deref(),
        Some("2024-01-02T03:05:00+00:00")
    );
    assert_eq!(row.in_reply_to, Some(41));
}

#[test]
fn user_row_prefers_global_name() {
    let u = user(9, "carol");
    let r = user_row(&u);
    assert_eq!(r.discord_user_id, 9);
    assert_eq!(r.username, "carol");
    assert_eq!(r.display_name, "carol display");
    assert_eq!(r.nickname, "carol display");

    let mut v = user_json(9, "carol");
    v["global_name"] = json!(null);
    let r = user_row(&serde_json::from_value(v).unwrap());
    assert_eq!(r.display_name, "carol");
}

#[test]
fn avatar_spec_only_for_custom_avatars() {
    assert_eq!(avatar_spec(&user(9, "carol")), None);

    let mut v = user_json(9, "carol");
    v["avatar"] = json!("a_0123456789abcdef0123456789abcdef");
    let spec = avatar_spec(&serde_json::from_value(v).unwrap()).unwrap();
    assert_eq!(spec.content_type, "image/gif");
    assert_eq!(
        spec.object_key,
        "discord/avatars/9/a_0123456789abcdef0123456789abcdef"
    );

    let mut v = user_json(9, "carol");
    v["avatar"] = json!("0123456789abcdef0123456789abcdef");
    let spec = avatar_spec(&serde_json::from_value(v).unwrap()).unwrap();
    assert_eq!(spec.content_type, "image/webp");
}

#[test]
fn attachment_spec_keys_by_message_and_attachment() {
    let mut v = message_json(42, CHANNEL, user_json(7, "bob"), "");
    v["attachments"] = json!([{
        "id": "99", "filename": "../../etc/passwd", "size": 3,
        "url": "https://cdn.example/a", "proxy_url": "https://cdn.example/a",
        "content_type": "image/png"
    }]);
    let m = message(v);
    let spec = attachment_spec(&m, &m.attachments[0]);
    assert_eq!(spec.object_key, "discord/attachments/42/99");
    assert_eq!(spec.content_type, "image/png");
    assert_eq!(spec.url, "https://cdn.example/a");
}

#[test]
fn sticker_spec_by_format() {
    let s = |fmt: u8| -> StickerItem {
        serde_json::from_value(json!({"id": "77", "name": "s", "format_type": fmt})).unwrap()
    };
    assert_eq!(
        sticker_spec(&s(1)).unwrap().object_key,
        "discord/stickers/77.png"
    );
    assert_eq!(sticker_spec(&s(2)).unwrap().content_type, "image/png");
    assert_eq!(
        sticker_spec(&s(3)).unwrap().content_type,
        "application/json"
    );
    assert_eq!(
        sticker_spec(&s(4)).unwrap().object_key,
        "discord/stickers/77.gif"
    );
    assert_eq!(sticker_spec(&s(99)), None);
}

#[test]
fn emoji_spec_custom_and_unicode() {
    let custom: ReactionType =
        serde_json::from_value(json!({"id": "555", "name": "pog", "animated": true})).unwrap();
    let spec = emoji_spec(&custom, Some(serenity::all::GuildId::new(GUILD))).unwrap();
    assert_eq!(spec.discord_emoji_id, "555");
    assert_eq!(spec.from_guild.as_deref(), Some(GUILD.to_string().as_str()));
    assert!(spec.is_animated);
    assert_eq!(
        spec.asset.as_ref().unwrap().object_key,
        "discord/emojis/555.webp"
    );
    assert_eq!(emoji_key(&custom).as_deref(), Some("555"));

    let uni = ReactionType::Unicode("👍".into());
    let spec = emoji_spec(&uni, None).unwrap();
    assert_eq!(spec.discord_emoji_id, "👍");
    assert!(spec.asset.is_none());
    assert_eq!(emoji_key(&uni).as_deref(), Some("👍"));
}

#[test]
fn channel_row_maps_kind_and_parent() {
    let row = channel_row(&guild_channel(5, 0, "general", Some(4)));
    assert_eq!(row.channel_id, 5);
    assert_eq!(row.parent_channel_id, Some(4));
    assert_eq!(row.channel_type.to_string(), "text");
    assert_eq!(
        channel_row(&guild_channel(4, 4, "cat", None))
            .channel_type
            .to_string(),
        "category"
    );
    assert_eq!(
        channel_row(&guild_channel(6, 11, "t", Some(5)))
            .channel_type
            .to_string(),
        "public_thread"
    );
    // announcement channels are not a known unidb type -> text fallback
    assert_eq!(
        channel_row(&guild_channel(7, 5, "news", None))
            .channel_type
            .to_string(),
        "text"
    );
}

#[test]
fn edit_content_ignores_system_messages_and_missing_content() {
    use serenity::all::MessageType;
    assert_eq!(
        edit_content(Some(MessageType::Regular), Some("x")).as_deref(),
        Some("x")
    );
    assert_eq!(edit_content(None, Some("x")).as_deref(), Some("x"));
    assert_eq!(edit_content(Some(MessageType::MemberJoin), Some("")), None);
    assert_eq!(edit_content(Some(MessageType::Regular), None), None);
    assert!(is_system_kind(MessageType::PinsAdd));
    assert!(!is_system_kind(MessageType::InlineReply));
}

/// Found by fuzzing (message_mapping): the attachment content type is uploader-controlled and was
/// passed to S3 and `media_assets.content_type` verbatim, including "" and header-breaking values.
#[test]
fn attachment_content_type_is_sanitized() {
    for (ct, expected) in [
        (json!(""), "application/octet-stream"),
        (json!("   "), "application/octet-stream"),
        (
            json!("text/html\r\nx-amz-meta-evil: 1"),
            "application/octet-stream",
        ),
        (json!("image"), "application/octet-stream"),
        (json!("image/png"), "image/png"),
        (
            json!("text/plain; charset=utf-8"),
            "text/plain; charset=utf-8",
        ),
        (json!(null), "application/octet-stream"),
    ] {
        let mut v = message_json(1, CHANNEL, user_json(7, "u7"), "");
        v["attachments"] = json!([{"id": "2", "filename": "f", "size": 1, "url": "https://cdn/f",
            "proxy_url": "https://cdn/f", "content_type": ct}]);
        let m = message(v);
        assert_eq!(
            attachment_spec(&m, &m.attachments[0]).content_type,
            expected,
            "input {ct}"
        );
    }
}

/// Known upstream bug (serenity 0.12.5 `ImageHash::from_str` slices by byte index): a 32-byte
/// non-ASCII avatar hash panics during deserialization instead of erroring. Discord only sends
/// hex hashes, so this is not reachable from real gateway traffic. When this starts failing,
/// serenity fixed it and the filter in fuzz/fuzz_targets/raw_payloads.rs can go.
#[test]
fn serenity_image_hash_panics_on_non_ascii() {
    let r = std::panic::catch_unwind(|| {
        let _: Result<serenity::all::User, _> = serde_json::from_value(
            json!({"id": "1", "username": "u", "discriminator": "0", "avatar": "012345678ǝbcdef0123456789abcdef"}),
        );
    });
    assert!(
        r.is_err(),
        "serenity no longer panics on non-ASCII image hashes"
    );
}

/// Every system message kind gets synthetic content (from mutation testing: deleting any arm
/// went unnoticed).
#[test]
fn every_system_kind_has_synthetic_content() {
    use serenity::all::MessageType as T;
    let with_author = [
        (
            T::GroupRecipientAddition,
            "a added a recipient to the group",
        ),
        (
            T::GroupRecipientRemoval,
            "a removed a recipient from the group",
        ),
        (T::GroupCallCreation, "a started a call"),
        (T::GroupNameUpdate, "a changed the group name"),
        (T::GroupIconUpdate, "a changed the group icon"),
        (T::PinsAdd, "a pinned a message"),
        (T::MemberJoin, "user a joined the server"),
        (T::NitroBoost, "a boosted the server"),
        (T::NitroTier1, "the server reached Nitro Boost tier 1"),
        (T::NitroTier2, "the server reached Nitro Boost tier 2"),
        (T::NitroTier3, "the server reached Nitro Boost tier 3"),
        (T::ChannelFollowAdd, "a followed a news channel"),
        (
            T::GuildDiscoveryDisqualified,
            "the server was disqualified from Discovery",
        ),
        (
            T::GuildDiscoveryRequalified,
            "the server was requalified for Discovery",
        ),
        (
            T::GuildDiscoveryGracePeriodInitialWarning,
            "initial Discovery grace period warning",
        ),
        (
            T::GuildDiscoveryGracePeriodFinalWarning,
            "final Discovery grace period warning",
        ),
        (T::ThreadCreated, "a created a thread"),
        (T::ThreadStarterMessage, "[thread starter message]"),
        (T::GuildInviteReminder, "[guild invite reminder]"),
        (T::AutoModAction, "[auto-moderation action]"),
        (
            T::RoleSubscriptionPurchase,
            "a purchased a role subscription",
        ),
        (T::InteractionPremiumUpsell, "[interaction premium upsell]"),
        (T::StageStart, "[stage started]"),
        (T::StageEnd, "[stage ended]"),
        (T::StageSpeaker, "a is now a stage speaker"),
        (T::StageTopic, "[stage topic changed]"),
        (
            T::GuildApplicationPremiumSubscription,
            "[application premium subscription]",
        ),
        (
            T::GuildIncidentAlertModeEnabled,
            "[incident alert mode enabled]",
        ),
        (
            T::GuildIncidentAlertModeDisabled,
            "[incident alert mode disabled]",
        ),
        (T::GuildIncidentReportRaid, "[incident report: raid]"),
        (
            T::GuildIncidentReportFalseAlarm,
            "[incident report: false alarm]",
        ),
        (T::PurchaseNotification, "a made a purchase"),
    ];
    for (kind, expected) in with_author {
        assert_eq!(
            system_content_for(kind, "a").as_deref(),
            Some(expected),
            "{kind:?}"
        );
        assert!(is_system_kind(kind));
    }
    for kind in [
        T::Regular,
        T::InlineReply,
        T::ChatInputCommand,
        T::ContextMenuCommand,
    ] {
        assert_eq!(system_content_for(kind, "a"), None, "{kind:?}");
    }
}

#[test]
fn plausible_mime_requires_both_tokens() {
    for bad in [
        "/png",
        "image/",
        "ima ge/png",
        "image/p ng",
        "image/png\u{7f}",
        "",
    ] {
        assert!(!is_plausible_mime(bad), "{bad:?}");
    }
    assert!(!is_plausible_mime(&format!("a/{}", "b".repeat(254))));
    for good in [
        "image/png",
        "application/vnd.api+json",
        "text/plain; charset=utf-8",
    ] {
        assert!(is_plausible_mime(good), "{good:?}");
    }
}
