use ormlite::Model as _;
use serenity::all::GuildChannel;
use sqlx::PgPool;
use unidb::models::{DiscordChannel, enums::DiscordChannelType};

#[tracing::instrument(skip_all)]
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
        // TODO ix parent channel handling (insert parent always before child, maybe use recursion for this to get the top top top most parent?)
        // parent_channel_id: channel.parent_id.map(|id| id.get() as i64),
        parent_channel_id: None,
    }
    .insert(pool)
    .await?;

    Ok(())
}
