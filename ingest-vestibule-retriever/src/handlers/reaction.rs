use crate::handlers::emoji::handle_emoji_resolution;
use crate::handlers::ensure_user;
use crate::mapping::{emoji_key, snowflake};
use crate::{AppCtx, cache_contains, db};
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

#[tracing::instrument(skip_all, fields(msg_id = reaction.message_id.get()))]
pub async fn handle_reaction_add(ctx: &AppCtx, reaction: &Reaction) -> color_eyre::Result<()> {
    let Some(reactor) = reaction.user_id else {
        tracing::debug!("Reaction has no user_id, skipping");
        return Ok(());
    };
    let user_id = snowflake(reactor.get());
    let message_id = snowflake(reaction.message_id.get());

    // Only hit the Discord API when the message is not known yet.
    if !cache_contains(&ctx.message_id_cache, message_id)
        && let Err(e) = ensure_reacted_message(ctx, reaction).await
    {
        tracing::error!(error = %e, "Failed to process missing message for reaction");
        return Err(e);
    }

    // The reactor may never have posted, so may not be in discord_accounts yet
    // (FK message_reactions.user_id). Guild reaction events carry the member.
    if !cache_contains(&ctx.user_id_cache, user_id) {
        match reaction.member.as_ref().map(|m| &m.user) {
            Some(u) if u.id == reactor => ensure_user(ctx, u).await?,
            _ => ensure_user(ctx, &ctx.discord.get_user(reactor).await?).await?,
        }
    }

    let Some(emoji_id) = handle_emoji_resolution(ctx, &reaction.emoji, reaction.guild_id).await?
    else {
        tracing::warn!("Unknown reaction type, skipping");
        return Ok(());
    };

    if let Err(e) = db::insert_reaction(&ctx.db_pool, message_id, user_id, emoji_id).await {
        tracing::error!(error = %e, "Failed to insert reaction into database");
        return Err(e);
    }

    Ok(())
}
