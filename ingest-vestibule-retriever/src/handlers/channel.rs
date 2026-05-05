use ormlite::Model as _;
use serenity::all::GuildChannel;
use sqlx::PgPool;
use unidb::models::{DiscordChannel, enums::DiscordChannelType};

#[tracing::instrument(skip(pool, channel))]
pub async fn insert_discord_channel(
    pool: &PgPool,
    channel: &GuildChannel,
) -> color_eyre::Result<()> {
    let channel_type = DiscordChannelType::try_from(channel.kind).unwrap_or_else(|_| {
        tracing::error!(
            channel_id=channel.id.get(),
            "Discord channel kind (\"{}\") is not a known channel type, returning text type as fallback.",
            channel.kind.name()
        );
        DiscordChannelType::Text
    });

    DiscordChannel {
        channel_id: channel.id.get() as i64,
        name: channel.name.clone(),
        channel_type,
        parent_channel_id: channel.parent_id.map(|id| id.get() as i64),
    }
    .insert(pool)
    .await?;

    Ok(())
}
