use crate::handlers::attachments::{insert_message_attachments, insert_stickers};
use crate::handlers::channel::ensure_discord_channel;
use crate::handlers::discord_user::ensure_user;
use crate::mapping::{message_row, reply_parent, snowflake};
use crate::{AppCtx, cache_contains, cache_insert, db};
use serenity::all::Message;

/// Processes a single Discord message completely:
///   1. Inserts channel (+ parents) and author if not in db
///   2. Inserts the reply parent (or a placeholder for it) first
///   3. Inserts message row
///   4. Downloads and stores attachments/stickers as media assets + message attachments
///
/// Used by both the live `EventHandler` and `historical_scan`.
pub async fn ensure_message(ctx: &AppCtx, msg: &Message) -> color_eyre::Result<()> {
    let row = message_row(msg);
    if cache_contains(&ctx.message_id_cache, row.message_id) {
        return Ok(());
    }

    ensure_discord_channel(ctx, msg.channel_id).await?;
    ensure_user(ctx, &msg.author).await?;

    if let Some((parent_channel, parent_id)) = reply_parent(msg) {
        let parent_db_id = snowflake(parent_id.get());
        if !cache_contains(&ctx.message_id_cache, parent_db_id) {
            match ctx.discord.get_message(parent_channel, parent_id).await? {
                Some(parent_msg) => {
                    // Boxed because of async recursion
                    Box::pin(ensure_message(ctx, &parent_msg)).await?;
                }
                None => {
                    tracing::debug!(
                        msg_id = parent_db_id,
                        "Parent message not found on Discord, inserting placeholder"
                    );
                    db::insert_placeholder_message(&ctx.db_pool, parent_db_id, row.channel_id)
                        .await?;
                    cache_insert(&ctx.message_id_cache, parent_db_id);
                }
            }
        }
    }

    db::insert_message(&ctx.db_pool, &row).await?;
    cache_insert(&ctx.message_id_cache, row.message_id);

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
