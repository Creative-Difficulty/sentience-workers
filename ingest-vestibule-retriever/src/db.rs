use ormlite::model::Model;
use sqlx::PgPool;
use unidb::models::{DiscordAccount, DiscordChannel, Message, VestibuleUser};

#[tracing::instrument(skip_all, fields(message_id = msg.message_id, channel_id = msg.channel_id))]
pub async fn insert_message(pool: &PgPool, msg: &Message) -> color_eyre::Result<()> {
    tracing::trace!("Upserting message into database");
    msg.clone().insert(pool).await?;
    tracing::debug!("Successfully upserted message");
    Ok(())
}

#[tracing::instrument(skip_all, fields(channel_id = channel.channel_id))]
pub async fn insert_channel(pool: &PgPool, channel: &DiscordChannel) -> color_eyre::Result<()> {
    tracing::trace!("Upserting discord channel into database");
    channel.clone().insert(pool).await?;
    tracing::debug!("Successfully upserted discord channel");
    Ok(())
}

#[tracing::instrument(skip_all, fields(user_id = %user.id))]
pub async fn insert_vestibule_user(pool: &PgPool, user: &VestibuleUser) -> color_eyre::Result<()> {
    tracing::trace!("Upserting vestibule user into database");
    user.clone().insert(pool).await?;
    tracing::debug!("Successfully upserted vestibule user");
    Ok(())
}

#[tracing::instrument(skip_all, fields(discord_user_id = account.discord_user_id))]
pub async fn insert_discord_account(
    pool: &PgPool,
    account: &DiscordAccount,
) -> color_eyre::Result<()> {
    tracing::trace!("Upserting discord account into database");
    account.clone().insert(pool).await?;
    tracing::debug!("Successfully upserted discord account");
    Ok(())
}
