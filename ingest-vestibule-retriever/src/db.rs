//! Database operations. Every function takes a `&PgPool` (or runs its own transaction on it) and
//! no Discord/HTTP types, so they can be integration tested against a real Postgres.

use chrono::{DateTime, TimeZone, Utc};
use sqlx::{Connection as _, PgPool};
use unidb::models::DiscordChannel;
use uuid::Uuid;

use crate::mapping::{EmojiSpec, MessageRow, UserRow};

/// Placeholder author for messages we could not fetch (seeded by unidb).
pub const DELETED_USER_ID: i64 = 0;

// ---------------------------------------------------------------------------------------------
// channels
// ---------------------------------------------------------------------------------------------

/// Inserts a channel; an existing row (e.g. concurrent insert) is left as is.
pub async fn insert_channel(pool: &PgPool, ch: &DiscordChannel) -> color_eyre::Result<()> {
    sqlx::query!(
        "INSERT INTO discord_channels (channel_id, name, channel_type, parent_channel_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING",
        ch.channel_id,
        ch.name,
        ch.channel_type as _,
        ch.parent_channel_id
    )
    .execute(pool)
    .await?;
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

async fn insert_account(
    conn: &mut sqlx::PgConnection,
    row: &UserRow,
    vestibule_user_id: Uuid,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query!(
        "INSERT INTO discord_accounts (discord_user_id, vestibule_user_id, username, display_name)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (discord_user_id) DO NOTHING",
        row.discord_user_id,
        vestibule_user_id,
        row.username,
        row.display_name
    )
    .execute(conn)
    .await?
    .rows_affected())
}

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
///   time and can be changed and re-used. If the insert hits that UNIQUE because another
///   (necessarily stale) account still holds the username, that row's username is renamed to
///   [`stale_username`] and the insert retried, so the current holder can be inserted. Without
///   this, every message of the new user would fail its FK forever. On a schema without the
///   UNIQUE nothing is renamed.
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

    let vestibule_user_id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO vestibule_users (id, nickname) VALUES ($1, $2)",
        vestibule_user_id,
        row.nickname
    )
    .execute(&mut *tx)
    .await?;

    // First attempt inside a savepoint: on a schema where `username` is UNIQUE, a stale holder of
    // the username makes it fail with unique_violation on that constraint.
    let mut attempt = tx.begin().await?;
    let first = insert_account(&mut attempt, row, vestibule_user_id).await;
    let inserted = match first {
        Ok(n) => {
            attempt.commit().await?;
            n
        }
        Err(sqlx::Error::Database(e))
            if e.is_unique_violation() && e.constraint() != Some("discord_accounts_pkey") =>
        {
            attempt.rollback().await?;
            let displaced = sqlx::query!(
                "UPDATE discord_accounts SET username = username || '#stale-' || discord_user_id::text
                 WHERE username = $1 AND discord_user_id <> $2",
                row.username,
                row.discord_user_id
            )
            .execute(&mut *tx)
            .await?
            .rows_affected();
            tracing::warn!(
                username = %row.username,
                displaced,
                "username was held by a stale discord account; renamed the stale account"
            );
            insert_account(&mut tx, row, vestibule_user_id).await?
        }
        Err(e) => return Err(e.into()),
    };

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

/// Inserts a media asset row and returns its id. If an asset with the same `content_hash` or
/// `object_key` already exists (both are UNIQUE; e.g. a concurrent download of identical content),
/// the existing asset's id is returned instead of failing.
pub async fn insert_media_asset(
    pool: &PgPool,
    content_type: &str,
    object_key: &str,
    size_bytes: i64,
    content_hash: &str,
) -> color_eyre::Result<Uuid> {
    let inserted = sqlx::query_scalar!(
        "INSERT INTO media_assets (id, content_type, object_key, size_bytes, content_hash)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT DO NOTHING
         RETURNING id",
        Uuid::new_v4(),
        content_type,
        object_key,
        size_bytes,
        content_hash
    )
    .fetch_optional(pool)
    .await?;
    if let Some(id) = inserted {
        return Ok(id);
    }
    sqlx::query_scalar!(
        "SELECT id FROM media_assets WHERE content_hash = $1 OR object_key = $2
         ORDER BY (content_hash = $1) DESC LIMIT 1",
        content_hash,
        object_key
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| color_eyre::eyre::eyre!("media asset conflict but no existing row found"))
}

/// Links an asset to a message unless that exact link already exists. Returns whether a row was
/// written.
pub async fn insert_message_attachment(
    pool: &PgPool,
    message_id: i64,
    asset_id: Uuid,
) -> color_eyre::Result<bool> {
    let res = sqlx::query!(
        "INSERT INTO message_attachments (id, message_id, asset_id, added_at)
         SELECT $1, $2, $3, $4
         WHERE NOT EXISTS (
             SELECT 1 FROM message_attachments WHERE message_id = $2 AND asset_id = $3
         )",
        Uuid::new_v4(),
        message_id,
        asset_id,
        Utc::now()
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 1)
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

/// Returns the id of the emoji with `spec.discord_emoji_id`, inserting it if missing.
///
/// `discord_emojis.discord_emoji_id` has no UNIQUE constraint on the current schema, so a plain
/// lookup-then-insert races (concurrent reactions with a new emoji created duplicates). The
/// lookup and insert run in one transaction under an advisory lock keyed on the emoji id
/// (two-int key space, so it cannot collide with the bigint user-id locks). Works with and
/// without a UNIQUE on `discord_emoji_id`.
pub async fn resolve_emoji(
    pool: &PgPool,
    spec: &EmojiSpec,
    asset_id: Option<Uuid>,
) -> color_eyre::Result<Uuid> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(1, hashtext($1))",
        spec.discord_emoji_id
    )
    .execute(&mut *tx)
    .await?;

    if let Some(id) = sqlx::query_scalar!(
        "SELECT id FROM discord_emojis WHERE discord_emoji_id = $1 ORDER BY id LIMIT 1",
        spec.discord_emoji_id
    )
    .fetch_optional(&mut *tx)
    .await?
    {
        tx.commit().await?;
        return Ok(id);
    }

    let id = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO discord_emojis (id, discord_emoji_id, from_guild, emoji_display_name, is_animated, emoji_url, asset_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        id,
        spec.discord_emoji_id,
        spec.from_guild,
        spec.display_name,
        spec.is_animated,
        spec.emoji_url,
        asset_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Inserts a reaction; duplicates (same message, user, emoji) are ignored. Returns whether a
/// row was written. Uses a bare `ON CONFLICT DO NOTHING` so it works with both
/// `UNIQUE (message_id, user_id, emoji_id)` and `UNIQUE NULLS NOT DISTINCT (...)`.
pub async fn insert_reaction(
    pool: &PgPool,
    message_id: i64,
    user_id: i64,
    emoji_id: Uuid,
) -> color_eyre::Result<bool> {
    let res = sqlx::query!(
        "INSERT INTO message_reactions (id, message_id, user_id, emoji_id, reacted_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT DO NOTHING",
        Uuid::new_v4(),
        message_id,
        user_id,
        emoji_id,
        Utc::now()
    )
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 1)
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
