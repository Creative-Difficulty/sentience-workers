use crate::AppCtx;
use ormlite::Model as _;
use serenity::all::Reaction;
use unidb::models::MessageReaction;
use uuid::Uuid;

use crate::handlers::emoji::handle_emoji_resolution;

// TODO This deletes every reaction of that user on that message, we need to narrow this down
// TODO set deleted_at instead of removing from db
pub async fn handle_reaction_remove(ctx: &AppCtx, reaction: &Reaction) -> color_eyre::Result<()> {
    super::ensure_message(
        ctx,
        &ctx.discord_ctx
            .http
            .get_message(reaction.channel_id, reaction.message_id)
            .await?,
    )
    .await?;

    let user_id = match reaction.user_id {
        Some(id) => id.get() as i64,
        None => {
            tracing::debug!("Reaction remove has no user_id, skipping");
            return Ok(());
        }
    };

    let msg_id = reaction.message_id.get() as i64;

    sqlx::query!(
        "DELETE FROM message_reactions WHERE message_id = $1 AND user_id = $2",
        msg_id,
        user_id
    )
    .execute(&ctx.db_pool)
    .await?;

    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn handle_reaction_add(ctx: &AppCtx, reaction: &Reaction) -> color_eyre::Result<()> {
    let user_id = match reaction.user_id {
        Some(id) => id.get() as i64,
        None => {
            tracing::debug!("Reaction has no user_id, skipping");
            return Ok(());
        }
    };

    match ctx
        .discord_ctx
        .http
        .get_message(reaction.channel_id, reaction.message_id)
        .await
    {
        Ok(msg) => {
            if let Err(e) = crate::handlers::ensure_message(ctx, &msg).await {
                tracing::error!(error = %e, "Failed to process missing message for reaction");
                return Err(e);
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to fetch if message exists for reaction from Discord");
            return Err(e.into());
        }
    }

    let emoji_id = handle_emoji_resolution(ctx, &reaction.emoji, reaction.guild_id).await?;

    let db_reaction = MessageReaction {
        id: Uuid::new_v4(),
        message_id: reaction.message_id.get() as i64,
        user_id,
        emoji_id,
        reacted_at: chrono::Utc::now(),
    };

    if let Err(e) = db_reaction.insert(&ctx.db_pool).await {
        tracing::error!(error = %e, "Failed to insert reaction into database");
        return Err(e.into());
    }

    Ok(())
}
