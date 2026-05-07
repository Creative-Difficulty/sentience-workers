use crate::AppCtx;
use ormlite::Model as _;
use serenity::all::{Reaction, ReactionType};
use unidb::models::MessageReaction;
use uuid::Uuid;

use crate::handlers::emoji::handle_emoji_resolution;

// TODO set deleted_at instead of removing from db
#[tracing::instrument(skip_all, fields(msg_id = reaction.message_id.get()))]
pub async fn handle_reaction_remove(ctx: &AppCtx, reaction: &Reaction) -> color_eyre::Result<()> {
    let user_id = match reaction.user_id {
        Some(id) => id.get() as i64,
        None => {
            tracing::debug!("Reaction remove has no user_id, skipping");
            return Ok(());
        }
    };

    let msg_id = reaction.message_id.get() as i64;

    let msg = ctx
        .discord_ctx
        .http
        .get_message(reaction.channel_id, reaction.message_id)
        .await?;

    super::ensure_message(ctx, &msg).await?;

    let emoji_discord_id = match &reaction.emoji {
        ReactionType::Custom { id, .. } => id.get().to_string(),
        ReactionType::Unicode(s) => s.clone(),
        _ => {
            tracing::warn!("Unknown reaction type in reaction_remove, skipping");
            return Ok(());
        }
    };

    let emoji_id: Option<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM discord_emojis WHERE discord_emoji_id = $1",
        emoji_discord_id
    )
    .fetch_optional(&ctx.db_pool)
    .await?;

    if let Some(id) = emoji_id {
        sqlx::query!(
            "DELETE FROM message_reactions WHERE message_id = $1 AND user_id = $2 AND emoji_id = $3",
            msg_id,
            user_id,
            id
        )
        .execute(&ctx.db_pool)
        .await?;
    } else {
        tracing::debug!(
            msg_id,
            user_id,
            "Reaction to message is not in database, nothing to delete"
        );
    }

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
