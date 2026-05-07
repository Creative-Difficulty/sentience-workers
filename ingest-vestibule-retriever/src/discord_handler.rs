use std::sync::Arc;

use crate::{AppCtx, MessageIdCache};
use serenity::{
    all::{ChannelId, Context, EventHandler, GuildId, Message, Reaction},
    async_trait,
};
use sqlx::PgPool;

pub struct DiscordEventHandler {
    pub db_pool: PgPool,
    pub s3_client: aws_sdk_s3::Client,
    pub s3_bucket: String,
    pub intro_channel_id: ChannelId,
    pub guild_id: GuildId,
    pub message_id_cache: MessageIdCache,
}

impl DiscordEventHandler {
    fn ctx(&self, discord_ctx: Context) -> AppCtx {
        AppCtx {
            db_pool: self.db_pool.clone(),
            s3_client: self.s3_client.clone(),
            s3_bucket: self.s3_bucket.clone(),
            discord_ctx,
            message_id_cache: Arc::clone(&self.message_id_cache),
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

    #[tracing::instrument(skip_all, fields(msg_id=msg.id.get()))]
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.guild_id != Some(self.guild_id) {
            return;
        }

        let app_ctx = self.ctx(ctx);
        let intro_channel_id = self.intro_channel_id;

        tokio::spawn(async move {
            let span = tracing::info_span!("process_message_task", msg_id = msg.id.get());
            let _enter = span.enter();

            tracing::debug!("started message processing task");

            if let Err(e) = crate::handlers::ensure_message(&app_ctx, &msg).await {
                tracing::error!(error = %e, "Handler failed for message");
            }

            if msg.channel_id == intro_channel_id {
                tracing::info!("Received message in intro channel");
            }

            tracing::debug!("finished task");
        });
    }

    #[tracing::instrument(skip_all, fields(msg_id=_event.id.get()))]
    async fn message_update(
        &self,
        ctx: Context,
        _old_if_available: Option<Message>,
        new_msg: Option<Message>,
        _event: serenity::all::MessageUpdateEvent,
    ) {
        let Some(msg) = new_msg else {
            return;
        };

        if msg.guild_id != Some(self.guild_id) {
            return;
        }

        let app_ctx = self.ctx(ctx);

        tokio::spawn(async move {
            if let Err(e) = crate::handlers::handle_message_edit(&app_ctx, &msg).await {
                tracing::error!(error = %e, "Failed to process message edit handler");
            } else {
                tracing::debug!("Successfully logged message edit");
            }
        });
    }

    #[tracing::instrument(skip_all, fields(msg_id=add_reaction.message_id.get()))]
    async fn reaction_add(&self, ctx: Context, add_reaction: Reaction) {
        let span = tracing::info_span!(
            "reaction_add_handler",
            msg_id = add_reaction.message_id.get()
        );
        let _enter = span.enter();
        tracing::debug!("starting reaction add handler");

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
        let span = tracing::info_span!(
            "reaction_remove_handler",
            msg_id = remove_reaction.message_id.get()
        );
        let _enter = span.enter();
        tracing::debug!("starting reaction remove handler");

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
