use crate::mapping::channel_row;
use crate::{AppCtx, cache_contains, cache_insert, db};
use serenity::all::{Channel, ChannelId};

/// Makes sure `channel_id` and all of its parents are in `discord_channels`.
#[tracing::instrument(skip_all, fields(channel_id=channel_id.get()))]
pub async fn ensure_discord_channel(ctx: &AppCtx, channel_id: ChannelId) -> color_eyre::Result<()> {
    if cache_contains(&ctx.channel_id_cache, channel_id.get() as i64) {
        return Ok(());
    }

    // Need to fetch it and its parents
    let channel = match ctx.discord.get_channel(channel_id).await {
        Ok(c) => match c.guild() {
            Some(gc) => gc,
            None => return Err(color_eyre::eyre::eyre!("Not a guild channel")),
        },
        Err(e) => return Err(color_eyre::eyre::eyre!("Failed to fetch channel: {}", e)),
    };

    let mut channels_to_insert = vec![channel.clone()];
    let mut current_parent_id = channel.parent_id;

    // Recursively collect all parent channels up to the root, stopping early if already cached
    while let Some(parent_id) = current_parent_id {
        if cache_contains(&ctx.channel_id_cache, parent_id.get() as i64) {
            break;
        }

        match ctx.discord.get_channel(parent_id).await {
            Ok(Channel::Guild(parent_channel)) => {
                current_parent_id = parent_channel.parent_id;
                channels_to_insert.push(parent_channel);
            }
            Ok(_) => {
                tracing::error!(
                    "Parent channel {} is not a guild channel, not recording it as a parent channel",
                    parent_id.get()
                );
                break;
            }
            Err(e) => {
                tracing::error!("Failed to fetch parent channel {}: {}", parent_id.get(), e);
                break;
            }
        }
    }

    // Insert from top (root parent) to bottom (the actual channel we want to insert). A parent that
    // could not be fetched is not in the DB, so the child is stored without it rather than
    // failing the FK (which used to be logged, cached as stored, and break every message in it).
    for ch in channels_to_insert.into_iter().rev() {
        let mut row = channel_row(&ch);
        if let Some(parent) = row.parent_channel_id
            && !cache_contains(&ctx.channel_id_cache, parent)
        {
            tracing::warn!(
                channel_id = row.channel_id,
                parent,
                "Parent channel unavailable, storing channel without parent"
            );
            row.parent_channel_id = None;
        }
        db::insert_channel(&ctx.db_pool, &row).await?;
        cache_insert(&ctx.channel_id_cache, row.channel_id);
    }

    Ok(())
}
