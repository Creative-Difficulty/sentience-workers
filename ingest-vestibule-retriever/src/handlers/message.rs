use crate::AppCtx;
use crate::handlers::attachments::{insert_message_attachments, insert_stickers};
use crate::handlers::channel::ensure_discord_channel;
use crate::handlers::discord_user::ensure_user;
use chrono::{TimeZone, Utc};
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

    if let Some(in_reply_to) = in_reply_to {
        let exists =
            sqlx::query_scalar!("SELECT 1 FROM messages WHERE message_id = $1", in_reply_to)
                .fetch_optional(&ctx.db_pool)
                .await?;

        if exists.is_none() {
            match ctx
                .discord_ctx
                .http
                .get_message(
                    msg.channel_id,
                    serenity::all::MessageId::new(in_reply_to.try_into()?),
                )
                .await
            {
                Ok(parent_msg) => {
                    // I have no idea why I need to box here and what it does in an async context but it works
                    Box::pin(ensure_message(ctx, &parent_msg)).await?;
                }
                Err(serenity::Error::Http(http_err)) => {
                    let is_not_found = match &http_err {
                        serenity::all::HttpError::UnsuccessfulRequest(res) => {
                            res.status_code == serenity::all::StatusCode::NOT_FOUND
                        }
                        _ => false,
                    };

                    if is_not_found {
                        tracing::warn!(
                            msg_id = in_reply_to,
                            "Parent message not found on Discord, inserting placeholder"
                        );
                        DbMessage {
                            message_id: in_reply_to,
                            channel_id: msg.channel_id.get() as i64,
                            sent_by: 00000000000, // Fallback to current author since we don't know the original
                            content: "[deleted message]".to_string(),
                            sent_at: Utc.timestamp_opt(0, 0).unwrap(), // Fallback to current timestamp
                            last_edited: None,
                            deleted_at: Some(Utc.timestamp_opt(0, 0).unwrap()),
                            in_reply_to: None,
                            added_at: chrono::Utc::now(),
                        }
                        .insert(&ctx.db_pool)
                        .await?;
                    } else {
                        return Err(serenity::Error::Http(http_err).into());
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

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
