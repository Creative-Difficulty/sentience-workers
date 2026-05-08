use crate::AppCtx;
use serenity::all::Message;
use uuid::Uuid;

use crate::handlers::emoji::handle_emoji_resolution;

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
                    tracing::error!(error = %e, "Could not ensure user is already in the db when inserting reaction");
                    continue;
                }
            }

            let msg_id = msg.id.get() as i64;
            let user_id = user.id.get() as i64;
            let now = chrono::Utc::now();
            let new_id = Uuid::new_v4();

            // ON CONFLICT DO NOTHING avoids duplicate key errors if the reaction was
            // already inserted by the live event handler or a previous scan run.
            let result = sqlx::query!(
                "INSERT INTO message_reactions (id, message_id, user_id, emoji_id, reacted_at)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (message_id, user_id, emoji_id) DO NOTHING",
                new_id,
                msg_id,
                user_id,
                emoji_id,
                now
            )
            .execute(&ctx.db_pool)
            .await;

            if let Err(e) = result {
                tracing::error!(error = %e, "Failed to insert reaction");
            }
        }
    }

    Ok(())
}
