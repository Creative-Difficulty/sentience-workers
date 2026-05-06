use crate::{AppCtx, handlers::ensure_message};
use ormlite::Model as _;
use serenity::all::{Message, Reaction};
use unidb::models::MessageReaction;
use uuid::Uuid;

use crate::handlers::emoji::handle_emoji_resolution;

// TODO This deletes every reaction of that user on that message, we need to narrow this down
// TODO set deleted_at instead of removing from db
pub async fn handle_reaction_remove(ctx: &AppCtx, reaction: &Reaction) -> color_eyre::Result<()> {
    ensure_message(
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

    let message_exists = sqlx::query_scalar!(
        "SELECT 1 FROM messages WHERE message_id = $1",
        reaction.message_id.get() as i64
    )
    .fetch_optional(&ctx.db_pool)
    .await?;

    if message_exists.is_none() {
        tracing::debug!(
            "Message {} does not yet exist in database, attempting to fetch and process it",
            reaction.message_id.get()
        );
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
                tracing::error!(error = %e, "Failed to fetch missing message for reaction from Discord");
                return Err(e.into());
            }
        }

        // ensure_message already processes all reactions on the message,
        // so we don't need to insert this one again.
        return Ok(());
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

pub async fn insert_reactions(ctx: &AppCtx, msg: &Message) -> color_eyre::Result<()> {
    ensure_message(
        ctx,
        &ctx.discord_ctx
            .http
            .get_message(msg.channel_id, msg.id)
            .await?,
    )
    .await?;

    for reaction in &msg.reactions {
        let mut users = vec![];
        let mut after = None;

        // Get all message reactions if there are over 100, Discord API limits at 100 per request
        loop {
            match msg
                .reaction_users(
                    &ctx.discord_ctx.http,
                    reaction.reaction_type.clone(),
                    Some(100),
                    after,
                )
                .await
            {
                Ok(batch) => {
                    after = match batch.last() {
                        Some(last) => Some(last.id),
                        None => break,
                    };
                    users.extend(batch);
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to fetch reaction users");
                    break;
                }
            }
        }

        let emoji_id = handle_emoji_resolution(ctx, &reaction.reaction_type, msg.guild_id).await?;

        for user in users {
            match super::ensure_user(ctx, &msg.author).await {
                Ok(_) => (),
                Err(e) => {
                    tracing::error!(error = %e, "Could not ensure user is alrady in the db when inserting reaction");
                    continue;
                }
            }

            let db_reaction = MessageReaction {
                id: Uuid::new_v4(),
                message_id: msg.id.get() as i64,
                user_id: user.id.get() as i64,
                emoji_id,
                reacted_at: chrono::Utc::now(),
            };
            if let Err(e) = db_reaction.insert(&ctx.db_pool).await {
                tracing::error!(error = %e, "Failed to insert reaction");
            }
        }
    }

    Ok(())
}
