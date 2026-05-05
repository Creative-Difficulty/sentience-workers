use ormlite::Model as _;
use serenity::all::{Context, Message, Reaction};
use sqlx::PgPool;
use unidb::models::MessageReaction;
use uuid::Uuid;

use crate::handlers::emoji::handle_emoji_resolution;

// TODO This deletes every reaction of that user on that message, we need to narrow this down
// TODO set deleted_at instead of removing from db
pub async fn handle_reaction_remove(pool: &PgPool, reaction: &Reaction) -> color_eyre::Result<()> {
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
    .execute(pool)
    .await?;

    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn handle_reaction_add(
    ctx: &Context,
    pool: &PgPool,
    s3_client: &aws_sdk_s3::Client,
    s3_bucket: &str,
    reaction: &Reaction,
) -> color_eyre::Result<()> {
    let user_id = match reaction.user_id {
        Some(id) => id.get() as i64,
        None => {
            tracing::debug!("Reaction has no user_id, skipping");
            return Ok(());
        }
    };

    let message_exists = sqlx::query!(
        "SELECT 1 as exists FROM messages WHERE message_id = $1",
        reaction.message_id.get() as i64
    )
    .fetch_optional(pool)
    .await?;

    if message_exists.is_none() {
        tracing::warn!("Message {} does not exist in database, attempting to fetch and process it", reaction.message_id.get());
        match ctx.http.get_message(reaction.channel_id, reaction.message_id).await {
            Ok(msg) => {
                if let Err(e) = crate::handlers::message::process_discord_message_and_children(
                    ctx,
                    pool,
                    s3_client,
                    s3_bucket,
                    &msg,
                )
                .await
                {
                    tracing::error!(error = %e, "Failed to process missing message for reaction");
                    return Err(e);
                }
                // process_discord_message_and_children already processes all reactions on the message,
                // so we don't need to insert this one again.
                return Ok(());
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to fetch missing message for reaction from Discord");
                return Err(e.into());
            }
        }
    }

    let emoji_id = handle_emoji_resolution(
        pool,
        s3_client,
        s3_bucket,
        &reaction.emoji,
        reaction.guild_id,
    )
    .await?;

    let db_reaction = MessageReaction {
        id: Uuid::new_v4(),
        message_id: reaction.message_id.get() as i64,
        user_id,
        emoji_id,
        reacted_at: chrono::Utc::now(),
    };

    if let Err(e) = db_reaction.insert(pool).await {
        tracing::error!(error = %e, "Failed to insert reaction into database");
        return Err(e.into());
    }

    Ok(())
}

pub async fn insert_reactions(
    ctx: &Context,
    pool: &PgPool,
    s3_client: &aws_sdk_s3::Client,
    s3_bucket: &str,
    msg: &Message,
) -> color_eyre::Result<()> {
    for reaction in &msg.reactions {
        let mut users = vec![];
        let mut after = None;

        // Get all message reactions if there are over 100, Discord API limits at 100 per request
        loop {
            match msg
                .reaction_users(&ctx.http, reaction.reaction_type.clone(), Some(100), after)
                .await
            {
                Ok(batch) => {
                    after = Some(batch.last().unwrap().id);
                    users.extend(batch);
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to fetch reaction users");
                    break;
                }
            }
        }

        let emoji_id = handle_emoji_resolution(
            pool,
            s3_client,
            s3_bucket,
            &reaction.reaction_type,
            msg.guild_id,
        )
        .await?;

        for user in users {
            let db_reaction = MessageReaction {
                id: Uuid::new_v4(),
                message_id: msg.id.get() as i64,
                user_id: user.id.get() as i64,
                emoji_id,
                reacted_at: chrono::Utc::now(),
            };
            if let Err(e) = db_reaction.insert(pool).await {
                tracing::warn!(error = %e, "Failed to insert reaction");
            }
        }
    }

    Ok(())
}
