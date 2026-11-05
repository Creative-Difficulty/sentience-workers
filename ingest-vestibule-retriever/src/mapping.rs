//! Pure "Discord model -> DB row" mapping functions.
//!
//! Nothing in this module performs I/O, so every function can be unit tested (and fuzzed)
//! with serenity models deserialized from JSON fixtures.

use chrono::{DateTime, Utc};
use serenity::all::{
    Attachment, ChannelId, GuildChannel, GuildId, Message, MessageId, MessageReferenceKind,
    MessageType, ReactionType, StickerFormatType, StickerItem, User,
};
use unidb::models::{DiscordChannel, enums::DiscordChannelType};

/// Converts a Discord snowflake to the `bigint` used in the DB.
pub fn snowflake(id: u64) -> i64 {
    id as i64
}

/// Returns a synthetic content string for system messages (joins, boosts, pins, ...).
/// Returns `None` for regular messages/replies where the original content should be kept.
pub fn system_message_content(msg: &Message) -> Option<String> {
    system_content_for(msg.kind, &msg.author.name)
}

/// [`system_message_content`] for a message kind and author name.
pub fn system_content_for(kind: MessageType, author: &str) -> Option<String> {
    let s = match kind {
        MessageType::Regular
        | MessageType::InlineReply
        | MessageType::ChatInputCommand
        | MessageType::ContextMenuCommand => return None,
        MessageType::GroupRecipientAddition => format!("{} added a recipient to the group", author),
        MessageType::GroupRecipientRemoval => {
            format!("{} removed a recipient from the group", author)
        }
        MessageType::GroupCallCreation => format!("{} started a call", author),
        MessageType::GroupNameUpdate => format!("{} changed the group name", author),
        MessageType::GroupIconUpdate => format!("{} changed the group icon", author),
        MessageType::PinsAdd => format!("{} pinned a message", author),
        MessageType::MemberJoin => format!("user {} joined the server", author),
        MessageType::NitroBoost => format!("{} boosted the server", author),
        MessageType::NitroTier1 => "the server reached Nitro Boost tier 1".to_string(),
        MessageType::NitroTier2 => "the server reached Nitro Boost tier 2".to_string(),
        MessageType::NitroTier3 => "the server reached Nitro Boost tier 3".to_string(),
        MessageType::ChannelFollowAdd => format!("{} followed a news channel", author),
        MessageType::GuildDiscoveryDisqualified => {
            "the server was disqualified from Discovery".to_string()
        }
        MessageType::GuildDiscoveryRequalified => {
            "the server was requalified for Discovery".to_string()
        }
        MessageType::GuildDiscoveryGracePeriodInitialWarning => {
            "initial Discovery grace period warning".to_string()
        }
        MessageType::GuildDiscoveryGracePeriodFinalWarning => {
            "final Discovery grace period warning".to_string()
        }
        MessageType::ThreadCreated => format!("{} created a thread", author),
        MessageType::ThreadStarterMessage => "[thread starter message]".to_string(),
        MessageType::GuildInviteReminder => "[guild invite reminder]".to_string(),
        MessageType::AutoModAction => "[auto-moderation action]".to_string(),
        MessageType::RoleSubscriptionPurchase => {
            format!("{} purchased a role subscription", author)
        }
        MessageType::InteractionPremiumUpsell => "[interaction premium upsell]".to_string(),
        MessageType::StageStart => "[stage started]".to_string(),
        MessageType::StageEnd => "[stage ended]".to_string(),
        MessageType::StageSpeaker => format!("{} is now a stage speaker", author),
        MessageType::StageTopic => "[stage topic changed]".to_string(),
        MessageType::GuildApplicationPremiumSubscription => {
            "[application premium subscription]".to_string()
        }
        MessageType::GuildIncidentAlertModeEnabled => "[incident alert mode enabled]".to_string(),
        MessageType::GuildIncidentAlertModeDisabled => "[incident alert mode disabled]".to_string(),
        MessageType::GuildIncidentReportRaid => "[incident report: raid]".to_string(),
        MessageType::GuildIncidentReportFalseAlarm => "[incident report: false alarm]".to_string(),
        MessageType::PurchaseNotification => format!("{} made a purchase", author),
        _ => return None,
    };
    Some(s)
}

/// Whether messages of this kind store synthetic (not user-authored) content.
pub fn is_system_kind(kind: MessageType) -> bool {
    system_content_for(kind, "").is_some()
}

/// The new content to record for an edit, or `None` if the update does not change
/// user-authored content (no content in the update, or a system message whose stored content is
/// synthetic).
pub fn edit_content(kind: Option<MessageType>, content: Option<&str>) -> Option<String> {
    if kind.is_some_and(is_system_kind) {
        return None;
    }
    content.map(str::to_owned)
}

/// Content to store for a message: synthetic text for system messages, raw content otherwise.
pub fn message_content(msg: &Message) -> String {
    system_message_content(msg).unwrap_or_else(|| msg.content.clone())
}

/// A row of the `messages` table (minus `added_at`/`deleted_at`, set by the DB layer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub message_id: i64,
    pub channel_id: i64,
    pub sent_by: i64,
    pub content: String,
    pub sent_at: DateTime<Utc>,
    pub last_edited: Option<DateTime<Utc>>,
    pub in_reply_to: Option<i64>,
}

/// The message this message replies to, as `(channel the parent lives in, parent id)`.
///
/// Uses the channel from the reference (a thread starter message references a message in the
/// parent channel). Forwards (`type: 1`) and references into another guild (crossposts from a
/// followed channel) are not replies: their target is not ingestible and must not become a
/// `[deleted message]` placeholder.
pub fn reply_parent(msg: &Message) -> Option<(ChannelId, MessageId)> {
    let r = msg.message_reference.as_ref()?;
    if !matches!(r.kind, MessageReferenceKind::Default) {
        return None;
    }
    if let (Some(ref_guild), Some(msg_guild)) = (r.guild_id, msg.guild_id)
        && ref_guild != msg_guild
    {
        return None;
    }
    Some((r.channel_id, r.message_id?))
}

pub fn message_row(msg: &Message) -> MessageRow {
    MessageRow {
        message_id: snowflake(msg.id.get()),
        channel_id: snowflake(msg.channel_id.get()),
        sent_by: snowflake(msg.author.id.get()),
        content: message_content(msg),
        sent_at: *msg.timestamp,
        last_edited: msg.edited_timestamp.map(|t| *t),
        in_reply_to: reply_parent(msg).map(|(_, id)| snowflake(id.get())),
    }
}

/// Values for the `vestibule_users` + `discord_accounts` rows of a Discord user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    pub discord_user_id: i64,
    pub username: String,
    pub display_name: String,
    pub nickname: String,
}

pub fn user_row(user: &User) -> UserRow {
    let display = user
        .global_name
        .clone()
        .unwrap_or_else(|| user.name.clone());
    UserRow {
        discord_user_id: snowflake(user.id.get()),
        username: user.name.clone(),
        display_name: display.clone(),
        nickname: display,
    }
}

/// Something to download from `url` and store under `object_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetSpec {
    pub url: String,
    pub object_key: String,
    pub content_type: String,
}

pub fn avatar_spec(user: &User) -> Option<AssetSpec> {
    let url = user.avatar_url()?;
    let avatar_hash = user
        .avatar
        .as_ref()
        .map(|h| h.to_string())
        .unwrap_or_else(|| "default".to_string());
    let ext = if url.contains(".gif") { "gif" } else { "webp" };
    Some(AssetSpec {
        object_key: format!("discord/avatars/{}/{}", user.id.get(), avatar_hash),
        url,
        content_type: format!("image/{ext}"),
    })
}

pub fn attachment_object_key(msg: &Message, attachment: &Attachment) -> String {
    format!(
        "discord/attachments/{}/{}",
        msg.id.get(),
        attachment.id.get()
    )
}

pub fn attachment_spec(msg: &Message, attachment: &Attachment) -> AssetSpec {
    AssetSpec {
        url: attachment.url.clone(),
        object_key: attachment_object_key(msg, attachment),
        content_type: attachment
            .content_type
            .as_deref()
            .filter(|ct| is_plausible_mime(ct))
            .unwrap_or("application/octet-stream")
            .to_string(),
    }
}

/// Whether `ct` looks like `type/subtype[; params]` and is safe to use as an S3/HTTP
/// `Content-Type` value. The attachment content type comes from the uploader, so empty or
/// header-breaking values (CR/LF, control characters) must not reach S3 or the DB.
pub fn is_plausible_mime(ct: &str) -> bool {
    let token = |t: &str| {
        !t.is_empty()
            && t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
    };
    let essence = ct.split(';').next().unwrap_or("");
    ct.len() <= 255
        && ct.bytes().all(|b| (0x20..0x7f).contains(&b))
        && essence
            .split_once('/')
            .is_some_and(|(t, sub)| token(t) && token(sub))
}

/// Returns `None` for sticker formats we cannot map to a file type.
pub fn sticker_spec(sticker: &StickerItem) -> Option<AssetSpec> {
    let ext = match sticker.format_type {
        StickerFormatType::Png | StickerFormatType::Apng => "png",
        StickerFormatType::Lottie => "json",
        StickerFormatType::Gif => "gif",
        _ => return None,
    };
    let content_type = if ext == "json" {
        "application/json".to_string()
    } else {
        format!("image/{ext}")
    };
    Some(AssetSpec {
        url: format!(
            "https://cdn.discordapp.com/stickers/{}.{}",
            sticker.id.get(),
            ext
        ),
        object_key: format!("discord/stickers/{}.{}", sticker.id.get(), ext),
        content_type,
    })
}

/// The `discord_emojis.discord_emoji_id` key of a reaction type: the custom emoji snowflake, or
/// the unicode emoji string itself.
pub fn emoji_key(reaction: &ReactionType) -> Option<String> {
    match reaction {
        ReactionType::Custom { id, .. } => Some(id.get().to_string()),
        ReactionType::Unicode(s) => Some(s.clone()),
        _ => None,
    }
}

/// Values for a `discord_emojis` row; `asset` is the image to download for custom emojis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiSpec {
    pub discord_emoji_id: String,
    pub from_guild: Option<String>,
    pub display_name: Option<String>,
    pub is_animated: bool,
    pub emoji_url: Option<String>,
    pub asset: Option<AssetSpec>,
}

pub fn emoji_spec(reaction: &ReactionType, guild_id: Option<GuildId>) -> Option<EmojiSpec> {
    match reaction {
        ReactionType::Custom { animated, id, name } => {
            // All emojis are served in webp, this makes life way easier
            let url = if *animated {
                format!(
                    "https://cdn.discordapp.com/emojis/{}.webp?animated=true",
                    id.get()
                )
            } else {
                format!("https://cdn.discordapp.com/emojis/{}.webp", id.get())
            };
            Some(EmojiSpec {
                discord_emoji_id: id.get().to_string(),
                from_guild: guild_id.map(|g| g.get().to_string()),
                display_name: name.clone(),
                is_animated: *animated,
                emoji_url: Some(url.clone()),
                asset: Some(AssetSpec {
                    url,
                    object_key: format!("discord/emojis/{}.webp", id.get()),
                    content_type: "image/webp".to_string(),
                }),
            })
        }
        ReactionType::Unicode(s) => Some(EmojiSpec {
            discord_emoji_id: s.clone(),
            from_guild: None,
            display_name: None,
            is_animated: false,
            emoji_url: None,
            asset: None,
        }),
        _ => None,
    }
}

/// Maps a guild channel to its DB row. Unknown channel kinds fall back to `text`.
pub fn channel_row(ch: &GuildChannel) -> DiscordChannel {
    let channel_id = snowflake(ch.id.get());
    let channel_type = DiscordChannelType::try_from(ch.kind).unwrap_or_else(|_| {
        tracing::warn!(
            channel_id,
            "Discord channel kind (\"{}\") is not a known channel type, returning text type as fallback.",
            ch.kind.name()
        );
        DiscordChannelType::Text
    });
    DiscordChannel {
        channel_id,
        name: ch.name.clone(),
        channel_type,
        parent_channel_id: ch.parent_id.map(|id| snowflake(id.get())),
    }
}
