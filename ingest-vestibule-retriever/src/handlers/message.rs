use crate::handlers::attachments::{insert_message_attachments, insert_stickers};
use crate::handlers::channel::ensure_discord_channel;
use crate::handlers::discord_user::ensure_user;
use crate::mapping::{message_row, reply_parent, snowflake};
use crate::{AppCtx, cache_contains, cache_insert, db};
use serenity::all::{ChannelId, Message, MessageId};

/// Processes a single Discord message completely:
///   1. Inserts channel (+ parents) and author if not in db
///   2. Inserts the reply parent (or a placeholder for it) first
///   3. Inserts message row
///   4. Downloads and stores attachments/stickers as media assets + message attachments
///
/// Used by both the live `EventHandler` and `historical_scan`.
pub async fn ensure_message(ctx: &AppCtx, msg: &Message) -> color_eyre::Result<()> {
    let mut row = message_row(msg);
    if cache_contains(&ctx.message_id_cache, row.message_id) {
        return Ok(());
    }

    ensure_discord_channel(ctx, msg.channel_id).await?;
    ensure_user(ctx, &msg.author).await?;

    if let Some((parent_channel, parent_id)) = reply_parent(msg)
        && !cache_contains(&ctx.message_id_cache, snowflake(parent_id.get()))
        && let Err(e) = ensure_reply_parent(ctx, parent_channel, parent_id).await
    {
        // The message itself is still worth storing; only the reply link is lost.
        tracing::warn!(error = %e, parent_id = parent_id.get(), "Could not resolve reply parent, storing message without it");
        row.in_reply_to = None;
    }

    let inserted = db::insert_message(&ctx.db_pool, &row).await?;
    cache_insert(&ctx.message_id_cache, row.message_id);
    if !inserted {
        // Another task (live event vs. historical scan) stored it and handles its media.
        return Ok(());
    }

    // Attachments and stickers are independent: a sticker-only message has no attachments.
    if !msg.attachments.is_empty()
        && let Err(e) = insert_message_attachments(ctx, msg).await
    {
        tracing::error!(error = %e, "Failed to process attachments");
    }

    if !msg.sticker_items.is_empty()
        && let Err(e) = insert_stickers(ctx, msg).await
    {
        tracing::error!(error = %e, "Failed to process stickers");
    }

    Ok(())
}

/// Inserts the reply parent (or a "[deleted message]" placeholder for it) in the channel it
/// lives in. Errors when that channel or message cannot be read from Discord.
async fn ensure_reply_parent(
    ctx: &AppCtx,
    channel_id: ChannelId,
    message_id: MessageId,
) -> color_eyre::Result<()> {
    ensure_discord_channel(ctx, channel_id).await?;
    match ctx.discord.get_message(channel_id, message_id).await? {
        // Boxed because of async recursion
        Some(parent) => Box::pin(ensure_message(ctx, &parent)).await,
        None => {
            let parent_id = snowflake(message_id.get());
            tracing::debug!(
                msg_id = parent_id,
                "Parent message not found on Discord, inserting placeholder"
            );
            db::insert_placeholder_message(&ctx.db_pool, parent_id, snowflake(channel_id.get()))
                .await?;
            cache_insert(&ctx.message_id_cache, parent_id);
            Ok(())
        }
    }
}
