use crate::AppCtx;
use crate::handlers::attachments::{insert_message_attachments, insert_stickers};
use crate::handlers::channel::ensure_discord_channel;
use crate::handlers::discord_user::ensure_user;
use ormlite::Model as _;
use serenity::all::Message;
use unidb::models::Message as DbMessage;

/// Processes a single Discord message completely:
///   1. Inserts author if not in db (VestibuleUser + DiscordAccount)
///   2. Inserts message row
///   3. Downloads and stores attachments as MediaAssets & MessageAttachments and uploads them to S3 bucket
///   4. Fetches and stores reactions
///
/// Used by both the live `EventHandler` and `historical_scan`.
pub async fn ensure_message(ctx: &AppCtx, msg: &Message) -> color_eyre::Result<()> {
    if sqlx::query!(
        "SELECT message_id FROM messages WHERE message_id = $1",
        msg.id.get() as i64
    )
    .fetch_optional(&ctx.db_pool)
    .await?
    .is_some()
    {
        return Ok(());
    }

    if let Err(e) = ensure_discord_channel(ctx, msg.channel_id).await {
        tracing::error!(
            "Failed to ensure channel exists before message is inserted: {}",
            e
        );
        return Err(e);
    }

    ensure_user(ctx, &msg.author).await?;

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
    .insert(&ctx.db_pool)
    .await?;

    if !msg.attachments.is_empty() {
        if let Err(e) = insert_message_attachments(ctx, msg).await {
            tracing::error!(error = %e, "Failed to process attachments");
        }

        if let Err(e) = insert_stickers(ctx, msg).await {
            tracing::error!(error = %e, "Failed to process stickers");
        }
    }

    Ok(())
}
