//! Database operations. Every function takes a `&PgPool` (or runs its own transaction on it) and
//! no Discord/HTTP types, so they can be integration tested against a real Postgres.

use chrono::{DateTime, TimeZone, Utc};
use ormlite::Model as _;
use sqlx::PgPool;
use unidb::models::{DiscordChannel, DiscordEmoji, MediaAsset, MessageAttachment, MessageReaction};
use uuid::Uuid;

use crate::mapping::{EmojiSpec, MessageRow, UserRow};

/// Placeholder author for messages we could not fetch (seeded by unidb).
pub const DELETED_USER_ID: i64 = 0;

// ---------------------------------------------------------------------------------------------
// channels
// ---------------------------------------------------------------------------------------------

pub async fn insert_channel(pool: &PgPool, ch: &DiscordChannel) -> color_eyre::Result<()> {
    ch.clone().insert(pool).await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// messages
// ---------------------------------------------------------------------------------------------

/// Inserts a message row. Returns `true` when a new row was written, `false` if it existed.
pub async fn insert_message(pool: &PgPool, row: &MessageRow) -> color_eyre::Result<bool> {
    let res = sqlx::query!(
        "INSERT INTO messages (message_id, channel_id, sent_by, content, sent_at, last_edited, deleted_at, in_reply_to, added_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (message_id) DO NOTHING",
        row.message_id,
        row.channel_id,
        row.sent_by,
        row.content,
        row.sent_at,
        row.last_edited,
        Option::<DateTime<Utc>>::None,
        row.in_reply_to,
        Utc::now()
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 1)
}

/// Inserts a "[deleted message]" placeholder for a reply parent that no longer exists.
pub async fn insert_placeholder_message(
    pool: &PgPool,
    message_id: i64,
    channel_id: i64,
) -> color_eyre::Result<()> {
    let epoch = Utc.timestamp_opt(0, 0).unwrap();
    sqlx::query!(
        "INSERT INTO messages (message_id, channel_id, sent_by, content, sent_at, last_edited, deleted_at, in_reply_to, added_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (message_id) DO NOTHING",
        message_id,
        channel_id,
        DELETED_USER_ID,
        "[deleted message]",
        epoch,
        Option::<DateTime<Utc>>::None,
        epoch,
        Option::<i64>::None,
        Utc::now()
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn message_content(pool: &PgPool, message_id: i64) -> color_eyre::Result<Option<String>> {
    Ok(sqlx::query_scalar!(
        "SELECT content FROM messages WHERE message_id = $1",
        message_id
    )
    .fetch_optional(pool)
    .await?)
}

/// Result of [`record_edit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    /// The message is not in the DB.
    NotFound,
    /// The stored content already equals the new content (e.g. embed-only update, duplicate
    /// event): nothing written.
    Unchanged,
    /// The stored row was edited at or after `edited_at`: the event is stale, nothing written.
    Stale,
    /// Old content appended to `message_edits`, message updated.
    Recorded,
}

/// Records an edit atomically: locks the message row, appends the *currently stored* content to
/// `message_edits` and updates the message, unless the content is unchanged or the event is
/// older than the stored edit.
pub async fn record_edit(
    pool: &PgPool,
    message_id: i64,
    new_content: &str,
    edited_at: DateTime<Utc>,
) -> color_eyre::Result<EditOutcome> {
    let mut tx = pool.begin().await?;
    let Some(current) = sqlx::query!(
        "SELECT content, last_edited FROM messages WHERE message_id = $1 FOR UPDATE",
        message_id
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(EditOutcome::NotFound);
    };

    if current.content == new_content {
        return Ok(EditOutcome::Unchanged);
    }
    if current.last_edited.is_some_and(|last| edited_at <= last) {
        return Ok(EditOutcome::Stale);
    }

    sqlx::query!(
        "INSERT INTO message_edits (id, message_id, old_content, edited_at) VALUES ($1, $2, $3, $4)",
        Uuid::new_v4(),
        message_id,
        current.content,
        edited_at
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE messages SET content = $1, last_edited = $2 WHERE message_id = $3",
        new_content,
        edited_at,
        message_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(EditOutcome::Recorded)
}

pub async fn mark_message_deleted(pool: &PgPool, message_id: i64) -> color_eyre::Result<()> {
    sqlx::query!(
        "UPDATE messages SET deleted_at = NOW() WHERE message_id = $1",
        message_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// users
// ---------------------------------------------------------------------------------------------

/// Result of [`ensure_user_rows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureUser {
    /// New `vestibule_users` + `discord_accounts` rows were inserted.
    Created,
    /// The account already existed; nothing written.
    Existing,
}

/// Suffix appended to the username of a stale account whose username was taken over by another
/// Discord user. `#` cannot occur in Discord usernames, so the result never collides with a real
/// username.
pub fn stale_username(username: &str, discord_user_id: i64) -> String {
    format!("{username}#stale-{discord_user_id}")
}

/// Inserts the `vestibule_users` + `discord_accounts` rows for a Discord user, atomically and
/// idempotently.
///
/// * A transaction-scoped advisory lock on the Discord user id serializes concurrent calls for
///   the same user (live events + historical scan race on new users), so no orphan
///   `vestibule_users` rows are created.
/// * `discord_accounts.username` is `UNIQUE`, but Discord usernames are only unique at a point in
///   time and can be changed and re-used. If another (necessarily stale) account still holds the
///   username, that row's username is renamed to [`stale_username`] so the current holder can be
///   inserted. Without this, every message of the new user would fail its FK forever.
pub async fn ensure_user_rows(pool: &PgPool, row: &UserRow) -> color_eyre::Result<EnsureUser> {
    let mut tx = pool.begin().await?;
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", row.discord_user_id)
        .execute(&mut *tx)
        .await?;

    let exists = sqlx::query_scalar!(
        "SELECT 1 FROM discord_accounts WHERE discord_user_id = $1",
        row.discord_user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .is_some();
    if exists {
        tx.commit().await?;
        return Ok(EnsureUser::Existing);
    }

    let displaced = sqlx::query!(
        "UPDATE discord_accounts SET username = username || '#stale-' || discord_user_id::text
         WHERE username = $1 AND discord_user_id <> $2",
        row.username,
        row.discord_user_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if displaced > 0 {
        tracing::warn!(
            username = %row.username,
            "username was held by a stale discord account; renamed the stale account"
        );
    }

    let vestibule_user_id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO vestibule_users (id, nickname) VALUES ($1, $2)",
        vestibule_user_id,
        row.nickname
    )
    .execute(&mut *tx)
    .await?;

    let inserted = sqlx::query!(
        "INSERT INTO discord_accounts (discord_user_id, vestibule_user_id, username, display_name)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (discord_user_id) DO NOTHING",
        row.discord_user_id,
        vestibule_user_id,
        row.username,
        row.display_name
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    if inserted == 0 {
        // Lost a race with a writer that does not take the advisory lock: drop our
        // vestibule_users row with the rest of the transaction.
        tx.rollback().await?;
        return Ok(EnsureUser::Existing);
    }
    tx.commit().await?;
    Ok(EnsureUser::Created)
}

// ---------------------------------------------------------------------------------------------
// media assets / attachments
// ---------------------------------------------------------------------------------------------

pub async fn find_asset_by_hash(pool: &PgPool, hash: &str) -> color_eyre::Result<Option<Uuid>> {
    Ok(
        sqlx::query_scalar!("SELECT id FROM media_assets WHERE content_hash = $1", hash)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn find_asset_by_key(pool: &PgPool, key: &str) -> color_eyre::Result<Option<Uuid>> {
    Ok(
        sqlx::query_scalar!("SELECT id FROM media_assets WHERE object_key = $1", key)
            .fetch_optional(pool)
            .await?,
    )
}

/// Inserts a media asset row and returns its id.
pub async fn insert_media_asset(
    pool: &PgPool,
    content_type: &str,
    object_key: &str,
    size_bytes: i64,
    content_hash: &str,
) -> color_eyre::Result<Uuid> {
    let id = Uuid::new_v4();
    MediaAsset {
        id,
        content_type: content_type.to_string(),
        object_key: object_key.to_string(),
        size_bytes: Some(size_bytes),
        content_hash: Some(content_hash.to_string()),
        embedding: None,
    }
    .insert(pool)
    .await?;
    Ok(id)
}

/// Whether `message_id` already has an attachment whose asset has `object_key`.
pub async fn attachment_exists_by_key(
    pool: &PgPool,
    message_id: i64,
    object_key: &str,
) -> color_eyre::Result<bool> {
    Ok(sqlx::query_scalar!(
        "SELECT ma.id FROM message_attachments ma WHERE ma.message_id = $1 AND ma.asset_id IN (SELECT id FROM media_assets WHERE object_key = $2)",
        message_id,
        object_key
    )
    .fetch_optional(pool)
    .await?
    .is_some())
}

pub async fn insert_message_attachment(
    pool: &PgPool,
    message_id: i64,
    asset_id: Uuid,
) -> color_eyre::Result<()> {
    MessageAttachment {
        id: Uuid::new_v4(),
        message_id,
        asset_id,
        added_at: Utc::now(),
        deleted_at: None,
    }
    .insert(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// emojis / reactions
// ---------------------------------------------------------------------------------------------

pub async fn find_emoji(pool: &PgPool, discord_emoji_id: &str) -> color_eyre::Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM discord_emojis WHERE discord_emoji_id = $1",
        discord_emoji_id
    )
    .fetch_optional(pool)
    .await?)
}

/// Inserts an emoji row and returns its id.
pub async fn insert_emoji(
    pool: &PgPool,
    spec: &EmojiSpec,
    asset_id: Option<Uuid>,
) -> color_eyre::Result<Uuid> {
    let id = Uuid::new_v4();
    DiscordEmoji {
        id,
        discord_emoji_id: spec.discord_emoji_id.clone(),
        from_guild: spec.from_guild.clone(),
        emoji_display_name: spec.display_name.clone(),
        is_animated: spec.is_animated,
        emoji_url: spec.emoji_url.clone(),
        asset_id,
    }
    .insert(pool)
    .await?;
    Ok(id)
}

/// Plain insert of a reaction (used by the live handler).
pub async fn insert_reaction(
    pool: &PgPool,
    message_id: i64,
    user_id: i64,
    emoji_id: Option<Uuid>,
) -> color_eyre::Result<()> {
    MessageReaction {
        id: Uuid::new_v4(),
        message_id,
        user_id,
        emoji_id,
        reacted_at: Utc::now(),
    }
    .insert(pool)
    .await?;
    Ok(())
}

/// Insert of a reaction that ignores duplicates (used by the historical scan).
pub async fn insert_reaction_ignore_duplicate(
    pool: &PgPool,
    message_id: i64,
    user_id: i64,
    emoji_id: Option<Uuid>,
) -> color_eyre::Result<()> {
    sqlx::query!(
        "INSERT INTO message_reactions (id, message_id, user_id, emoji_id, reacted_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (message_id, user_id, emoji_id) DO NOTHING",
        Uuid::new_v4(),
        message_id,
        user_id,
        emoji_id,
        Utc::now()
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_reaction(
    pool: &PgPool,
    message_id: i64,
    user_id: i64,
    emoji_id: Uuid,
) -> color_eyre::Result<()> {
    sqlx::query!(
        "DELETE FROM message_reactions WHERE message_id = $1 AND user_id = $2 AND emoji_id = $3",
        message_id,
        user_id,
        emoji_id
    )
    .execute(pool)
    .await?;
    Ok(())
}
