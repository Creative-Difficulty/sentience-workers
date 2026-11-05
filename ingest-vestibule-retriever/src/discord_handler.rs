use std::sync::Arc;

use crate::discord_api::SerenityDiscord;
use crate::media::{MediaFetcher, ObjectStore};
use crate::{AppCtx, IdCache};
use serenity::{
    all::{Context, EventHandler, GuildId, Message, Reaction},
    async_trait,
};
use sqlx::PgPool;

pub struct DiscordEventHandler {
    pub db_pool: PgPool,
    pub fetcher: Arc<dyn MediaFetcher>,
    pub store: Arc<dyn ObjectStore>,
    pub guild_id: GuildId,
    pub message_id_cache: IdCache,
    pub channel_id_cache: IdCache,
    pub user_id_cache: IdCache,
}

impl DiscordEventHandler {
    fn ctx(&self, discord_ctx: Context) -> AppCtx {
        AppCtx {
            db_pool: self.db_pool.clone(),
            discord: Arc::new(SerenityDiscord(Arc::clone(&discord_ctx.http))),
            fetcher: Arc::clone(&self.fetcher),
            store: Arc::clone(&self.store),
            message_id_cache: Arc::clone(&self.message_id_cache),
            channel_id_cache: Arc::clone(&self.channel_id_cache),
            user_id_cache: Arc::clone(&self.user_id_cache),
        }
    }
}

#[async_trait]
impl EventHandler for DiscordEventHandler {
    #[tracing::instrument(skip_all)]
    async fn ready(&self, ctx: Context, ready: serenity::all::Ready) {
        tracing::info!("Connected as {}", ready.user.name);

        let app_ctx = self.ctx(ctx);
        let guild_id = self.guild_id;

        tokio::spawn(async move {
            if let Err(e) = crate::historical::run_historical_scan(app_ctx, guild_id).await {
                tracing::error!(error = %e, "Historical scan task failed");
            }
        });
    }

    #[tracing::instrument(skip_all, fields(msg_id=msg.id.get(), channel_id=msg.channel_id.get()))]
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.guild_id != Some(self.guild_id) {
            return;
        }

        let app_ctx = self.ctx(ctx);

        tokio::spawn(async move {
            tracing::debug!("started message processing task");
            if let Err(e) = crate::handlers::ensure_message(&app_ctx, &msg).await {
                tracing::error!(error = %e, "Handler failed for message");
            }
            tracing::debug!("finished task");
        });
    }

    #[tracing::instrument(skip_all, fields(msg_id=event.id.get(), channel_id=event.channel_id.get()))]
    async fn message_update(
        &self,
        ctx: Context,
        _old_if_available: Option<Message>,
        _new: Option<Message>,
        event: serenity::all::MessageUpdateEvent,
    ) {
        // `_new` is only `Some` for messages in serenity's cache, which by default holds none,
        // so the event payload is used instead.
        if event.guild_id != Some(self.guild_id) {
            return;
        }

        let app_ctx = self.ctx(ctx);

        tokio::spawn(async move {
            match crate::handlers::handle_message_update(&app_ctx, &event).await {
                Ok(outcome) => tracing::debug!(?outcome, "Processed message update"),
                Err(e) => tracing::error!(error = %e, "Failed to process message update"),
            }
        });
    }

    #[tracing::instrument(skip_all, fields(msg_id=add_reaction.message_id.get()))]
    async fn reaction_add(&self, ctx: Context, add_reaction: Reaction) {
        if add_reaction.guild_id != Some(self.guild_id) {
            return;
        }

        let app_ctx = self.ctx(ctx);

        tokio::spawn(async move {
            if let Err(e) = crate::handlers::handle_reaction_add(&app_ctx, &add_reaction).await {
                tracing::error!(error = %e, "Failed to process reaction add handler");
            } else {
                tracing::debug!("Successfully logged reaction addition");
            }
        });
    }

    #[tracing::instrument(skip_all, fields(msg_id=remove_reaction.message_id.get()))]
    async fn reaction_remove(&self, ctx: Context, remove_reaction: Reaction) {
        if remove_reaction.guild_id != Some(self.guild_id) {
            return;
        }

        let app_ctx = self.ctx(ctx);

        tokio::spawn(async move {
            if let Err(e) =
                crate::handlers::handle_reaction_remove(&app_ctx, &remove_reaction).await
            {
                tracing::error!(error = %e, "Failed to delete reaction from database");
            } else {
                tracing::debug!("Successfully logged reaction removal");
            }
        });
    }
}
