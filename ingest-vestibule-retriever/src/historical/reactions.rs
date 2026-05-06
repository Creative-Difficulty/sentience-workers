use crate::AppCtx;
use ormlite::Model as _;
use serenity::all::Message;
use unidb::models::MessageReaction;
use uuid::Uuid;

use crate::handlers::emoji::handle_emoji_resolution;

// Usage:
// if let Err(e) =
//                 crate::handlers::reaction::insert_all_reactions_for_message(&app_ctx, &msg).await
//             {
//                 tracing::error!(error = %e, "Failed to process reactions");
//             }

pub async fn insert_all_reactions_for_message(
    ctx: &AppCtx,
    msg: &Message,
) -> color_eyre::Result<()> {
    crate::handlers::ensure_message(ctx, msg).await?;

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
            match crate::handlers::ensure_user(ctx, &user).await {
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
