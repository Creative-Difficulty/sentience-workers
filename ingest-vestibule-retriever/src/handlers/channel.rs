use ormlite::Model as _;
use serenity::all::{Context, GuildChannel};
use sqlx::PgPool;
use unidb::models::{DiscordChannel, enums::DiscordChannelType};

#[tracing::instrument(skip_all)]
pub async fn insert_discord_channel(
    ctx: &Context,
    pool: &PgPool,
    channel: &GuildChannel,
) -> color_eyre::Result<()> {
    if sqlx::query_scalar!(
        "SELECT channel_id from discord_channels WHERE channel_id = $1",
        channel.id.get() as i64
    )
    .fetch_optional(pool)
    .await?
    .is_some()
    {
        return Ok(());
    }

    let mut channels_to_insert = vec![channel.clone()];
    let mut current_parent_id = channel.parent_id;

    // Recursively collect all parent channels up to the root
    while let Some(parent_id) = current_parent_id {
        match ctx.http.get_channel(parent_id).await {
            Ok(serenity::all::Channel::Guild(parent_channel)) => {
                current_parent_id = parent_channel.parent_id;
                channels_to_insert.push(parent_channel);
            }
            Ok(_) => {
                tracing::warn!("Parent channel {} is not a guild channel", parent_id.get());
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
        let channel_type = DiscordChannelType::try_from(ch.kind).unwrap_or_else(|_| {
            tracing::error!(
                channel_id=ch.id.get(),
                "Discord channel kind (\"{}\") is not a known channel type, returning text type as fallback.",
                ch.kind.name()
            );
            DiscordChannelType::Text
        });

        let db_channel = DiscordChannel {
            channel_id: ch.id.get() as i64,
            name: ch.name.clone(),
            channel_type,
            parent_channel_id: ch.parent_id.map(|id| id.get() as i64),
        };

        if let Err(e) = db_channel.insert(pool).await {
            tracing::error!("Failed to insert channel {}: {}", ch.id.get(), e);
        }
    }

    Ok(())
}
