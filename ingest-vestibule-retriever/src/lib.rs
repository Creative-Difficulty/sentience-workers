pub mod discord_handler;
pub mod handlers;
pub mod historical;

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use serenity::all::Context;
use sqlx::PgPool;

pub type IdCache = Arc<RwLock<HashSet<i64>>>;

#[derive(Clone)]
pub struct AppCtx {
    pub db_pool: PgPool,
    pub s3_client: aws_sdk_s3::Client,
    pub s3_bucket: String,
    pub discord_ctx: Context,
    pub message_id_cache: IdCache,
    pub channel_id_cache: IdCache,
    pub user_id_cache: IdCache,
}

// TODO Keep this around until impl of message deletion marking handler
#[tracing::instrument(skip_all, fields(message_id = msg_id))]
pub async fn delete_message(pool: &PgPool, msg_id: i64) -> color_eyre::Result<()> {
    sqlx::query!(
        "UPDATE messages SET deleted_at = NOW() WHERE message_id = $1",
        msg_id
    )
    .execute(pool)
    .await?;
    tracing::debug!("Successfully marked message as deleted");
    Ok(())
}
