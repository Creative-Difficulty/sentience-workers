use crate::handlers::attachments::{insert_message_attachments, insert_stickers};
use crate::handlers::author::upsert_discord_user;
use crate::handlers::reaction::insert_reactions;
use ormlite::Model as _;
use serenity::all::{Context, Message};
use sqlx::PgPool;
use unidb::models::Message as DbMessage;

/// Processes a single Discord message completely:
///   1. Inserts author if not in db (VestibuleUser + DiscordAccount)
///   2. Inserts message row
///   3. Downloads and stores attachments as MediaAssets & MessageAttachments and uploads them to S3 bucket
///   4. Fetches and stores reactions
///
/// Used by both the live `EventHandler` and `historical_scan`.
#[tracing::instrument(skip_all)]
pub async fn process_discord_message_and_children(
    ctx: &Context,
    pool: &PgPool,
    s3_client: &aws_sdk_s3::Client,
    s3_bucket: &str,
    msg: &Message,
) -> color_eyre::Result<()> {
    upsert_discord_user(pool, s3_client, s3_bucket, msg).await?;

    let existing = sqlx::query!(
        "SELECT message_id FROM messages WHERE message_id = $1",
        msg.id.get() as i64
    )
    .fetch_optional(pool)
    .await?;

    if existing.is_some() {
        tracing::debug!("Message already exists in database, returning");
        return Ok(());
    }

    // TODO When its all done, how do we make sure every messages' `in_reply_to` message is acutally in the db: Insert messgaes by first sent = first inserted
    let in_reply_to = msg
        .message_reference
        .as_ref()
        .and_then(|r| r.message_id.map(|id| id.get() as i64));

    DbMessage {
        message_id: msg.id.get() as i64,
        channel_id: msg.channel_id.get() as i64,
        sent_by: msg.author.id.get() as i64,
        content: msg.content.clone(),
        sent_at: *msg.timestamp,
        last_edited: msg.edited_timestamp.map(|t| *t),
        deleted_at: None,
        in_reply_to,
        added_at: chrono::Utc::now(),
    }
    .insert(pool)
    .await?;

    #[allow(clippy::collapsible_if)]
    if !msg.attachments.is_empty() {
        if let Err(e) = insert_message_attachments(pool, s3_client, s3_bucket, msg).await {
            tracing::error!(error = %e, "Failed to process attachments");
        }

        if let Err(e) = insert_stickers(msg, pool, s3_client, s3_bucket).await {
            tracing::error!(error = %e, "Failed to process stickers");
        }
    }

    if let Err(e) = insert_reactions(ctx, pool, s3_client, s3_bucket, msg).await {
        tracing::error!(error = %e, "Failed to process reactions");
    }

    Ok(())
}
