use crate::handlers::emoji::handle_emoji_resolution;
use crate::mapping::{emoji_key, snowflake};
use crate::{AppCtx, db};
use serenity::all::Reaction;

async fn ensure_reacted_message(ctx: &AppCtx, reaction: &Reaction) -> color_eyre::Result<()> {
    match ctx
        .discord
        .get_message(reaction.channel_id, reaction.message_id)
        .await?
    {
        Some(msg) => crate::handlers::ensure_message(ctx, &msg).await,
        None => Err(color_eyre::eyre::eyre!(
            "Reacted message {} not found on Discord",
            reaction.message_id
        )),
    }
}

// TODO set deleted_at instead of removing from db
#[tracing::instrument(skip_all, fields(msg_id = reaction.message_id.get()))]
pub async fn handle_reaction_remove(ctx: &AppCtx, reaction: &Reaction) -> color_eyre::Result<()> {
    let Some(user_id) = reaction.user_id.map(|id| snowflake(id.get())) else {
        tracing::debug!("Reaction remove has no user_id, skipping");
        return Ok(());
    };
    let msg_id = snowflake(reaction.message_id.get());

    ensure_reacted_message(ctx, reaction).await?;

    let Some(key) = emoji_key(&reaction.emoji) else {
        tracing::warn!("Unknown reaction type in reaction_remove, skipping");
        return Ok(());
    };

    match db::find_emoji(&ctx.db_pool, &key).await? {
        Some(emoji_id) => db::delete_reaction(&ctx.db_pool, msg_id, user_id, emoji_id).await?,
        None => tracing::debug!(
            msg_id,
            user_id,
            "Reaction to message is not in database, nothing to delete"
        ),
    }

    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn handle_reaction_add(ctx: &AppCtx, reaction: &Reaction) -> color_eyre::Result<()> {
    let Some(user_id) = reaction.user_id.map(|id| snowflake(id.get())) else {
        tracing::debug!("Reaction has no user_id, skipping");
        return Ok(());
    };

    if let Err(e) = ensure_reacted_message(ctx, reaction).await {
        tracing::error!(error = %e, "Failed to process missing message for reaction");
        return Err(e);
    }

    let emoji_id = handle_emoji_resolution(ctx, &reaction.emoji, reaction.guild_id).await?;

    if let Err(e) = db::insert_reaction(
        &ctx.db_pool,
        snowflake(reaction.message_id.get()),
        user_id,
        emoji_id,
    )
    .await
    {
        tracing::error!(error = %e, "Failed to insert reaction into database");
        return Err(e);
    }

    Ok(())
}
