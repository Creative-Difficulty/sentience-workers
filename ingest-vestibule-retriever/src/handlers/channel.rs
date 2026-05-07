use crate::AppCtx;
use ormlite::Model as _;
use unidb::models::{DiscordChannel, enums::DiscordChannelType};

#[tracing::instrument(skip_all, fields(channel_id=channel_id.get()))]
pub async fn ensure_discord_channel(
    ctx: &AppCtx,
    channel_id: serenity::all::ChannelId,
) -> color_eyre::Result<()> {
    if ctx
        .channel_id_cache
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&(channel_id.get() as i64))
    {
        return Ok(());
    }

    // Need to fetch it and its parents
    let channel = match ctx.discord_ctx.http.get_channel(channel_id).await {
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
        if ctx
            .channel_id_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(parent_id.get() as i64))
        {
            break;
        }

        match ctx.discord_ctx.http.get_channel(parent_id).await {
            Ok(serenity::all::Channel::Guild(parent_channel)) => {
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

    // Insert from top (root parent) to bottom (the actual channel we want to insert)
    for ch in channels_to_insert.into_iter().rev() {
        let ch_id = ch.id.get() as i64;
        let channel_type = DiscordChannelType::try_from(ch.kind).unwrap_or_else(|_| {
            tracing::error!(
                channel_id = ch_id,
                "Discord channel kind (\"{}\") is not a known channel type, returning text type as fallback.",
                ch.kind.name()
            );
            DiscordChannelType::Text
        });

        let db_channel = DiscordChannel {
            channel_id: ch_id,
            name: ch.name.clone(),
            channel_type,
            parent_channel_id: ch.parent_id.map(|id| id.get() as i64),
        };

        if let Err(e) = db_channel.insert(&ctx.db_pool).await {
            tracing::error!("Failed to insert channel {}: {}", ch_id, e);
        }

        ctx.channel_id_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ch_id);
    }

    Ok(())
}
