use crate::handlers::emoji::handle_emoji_resolution;
use crate::mapping::snowflake;
use crate::{AppCtx, db};
use serenity::all::Message;

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
            match ctx
                .discord
                .reaction_users(msg.channel_id, msg.id, &reaction.reaction_type, after)
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
            if let Err(e) = crate::handlers::ensure_user(ctx, &user).await {
                tracing::error!(error = %e, "Could not ensure user is already in the db when inserting reaction");
                continue;
            }

            // ON CONFLICT DO NOTHING avoids duplicate key errors if the reaction was
            // already inserted by the live event handler or a previous scan run.
            if let Err(e) = db::insert_reaction_ignore_duplicate(
                &ctx.db_pool,
                snowflake(msg.id.get()),
                snowflake(user.id.get()),
                emoji_id,
            )
            .await
            {
                tracing::error!(error = %e, "Failed to insert reaction");
            }
        }
    }

    Ok(())
}
