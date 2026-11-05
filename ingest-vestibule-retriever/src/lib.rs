pub mod db;
pub mod discord_api;
pub mod discord_handler;
pub mod handlers;
pub mod historical;
pub mod mapping;
pub mod media;
pub mod startup;

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use sqlx::PgPool;

use crate::discord_api::DiscordApi;
use crate::media::{MediaFetcher, ObjectStore};

pub type IdCache = Arc<RwLock<HashSet<i64>>>;

fn cache_contains(cache: &IdCache, id: i64) -> bool {
    cache
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&id)
}

fn cache_insert(cache: &IdCache, id: i64) {
    cache.write().unwrap_or_else(|e| e.into_inner()).insert(id);
}

/// Everything a handler needs: DB, Discord REST access, media download + storage and the
/// in-memory "already in DB" caches.
#[derive(Clone)]
pub struct AppCtx {
    pub db_pool: PgPool,
    pub discord: Arc<dyn DiscordApi>,
    pub fetcher: Arc<dyn MediaFetcher>,
    pub store: Arc<dyn ObjectStore>,
    pub message_id_cache: IdCache,
    pub channel_id_cache: IdCache,
    pub user_id_cache: IdCache,
}

impl AppCtx {
    /// Context with empty caches.
    pub fn new(
        db_pool: PgPool,
        discord: Arc<dyn DiscordApi>,
        fetcher: Arc<dyn MediaFetcher>,
        store: Arc<dyn ObjectStore>,
    ) -> Self {
        Self {
            db_pool,
            discord,
            fetcher,
            store,
            message_id_cache: IdCache::default(),
            channel_id_cache: IdCache::default(),
            user_id_cache: IdCache::default(),
        }
    }
}

/// Fills the "already in DB" caches from the database.
pub async fn load_caches(pool: &PgPool) -> color_eyre::Result<(IdCache, IdCache, IdCache)> {
    let messages = sqlx::query_scalar!("SELECT message_id FROM messages")
        .fetch_all(pool)
        .await?;
    let channels = sqlx::query_scalar!("SELECT channel_id FROM discord_channels")
        .fetch_all(pool)
        .await?;
    let users = sqlx::query_scalar!("SELECT discord_user_id FROM discord_accounts")
        .fetch_all(pool)
        .await?;
    tracing::debug!(
        messages = messages.len(),
        channels = channels.len(),
        users = users.len(),
        "loaded ID caches from database"
    );
    let mk = |v: Vec<i64>| Arc::new(RwLock::new(v.into_iter().collect::<HashSet<i64>>()));
    Ok((mk(messages), mk(channels), mk(users)))
}

// TODO Keep this around until impl of message deletion marking handler
#[tracing::instrument(skip_all, fields(message_id = msg_id))]
pub async fn delete_message(pool: &PgPool, msg_id: i64) -> color_eyre::Result<()> {
    db::mark_message_deleted(pool, msg_id).await?;
    tracing::debug!("Successfully marked message as deleted");
    Ok(())
}
