use crate::mapping::emoji_spec;
use crate::{AppCtx, db};
use serenity::all::{GuildId, ReactionType};
use uuid::Uuid;

/// Returns the `discord_emojis.id` for a reaction type, inserting (and downloading) it if needed.
pub async fn handle_emoji_resolution(
    ctx: &AppCtx,
    reaction_type: &ReactionType,
    guild_id: Option<GuildId>,
) -> color_eyre::Result<Option<Uuid>> {
    let Some(spec) = emoji_spec(reaction_type, guild_id) else {
        return Ok(None);
    };

    if let Some(id) = db::find_emoji(&ctx.db_pool, &spec.discord_emoji_id).await? {
        return Ok(Some(id));
    }

    let asset_id = match &spec.asset {
        // Download emoji and store as a media asset via S3 object key
        Some(asset) => Some(super::attachments::download_and_store_media_asset(ctx, asset).await?),
        None => None,
    };

    match db::insert_emoji(&ctx.db_pool, &spec, asset_id).await {
        Ok(id) => Ok(Some(id)),
        Err(e) => {
            tracing::error!(error = %e, "Failed to insert discord emoji");
            Err(e)
        }
    }
}
