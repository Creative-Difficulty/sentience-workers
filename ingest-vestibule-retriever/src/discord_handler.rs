use std::time::Duration;

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
}

#[async_trait]
impl EventHandler for DiscordEventHandler {
    #[tracing::instrument(skip_all, fields(msg_id=msg.id.get()))]
    async fn message(&self, ctx: Context, msg: Message) {
        let span = tracing::info_span!("new_msg_handler", msg_id = msg.id.get());
        let _enter = span.enter();

        tracing::debug!("starting message handler");

        if msg.guild_id != Some(self.guild_id) {
            return;
        }

        let db_pool = self.db_pool.clone();
        let s3_client = self.s3_client.clone();
        let s3_bucket = self.s3_bucket.clone();
        let intro_channel_id = self.intro_channel_id;

        tokio::spawn(async move {
            let span = tracing::info_span!("process_message_task", msg_id = msg.id.get());
            let _enter = span.enter();

            tracing::debug!("started message processing task");

            let guild_channel = match ctx.http.get_channel(msg.channel_id).await {
                Ok(c) => match c.guild() {
                    Some(gc) => gc,
                    None => {
                        tracing::error!("not a guild channel");
                        return;
                    }
                },
                Err(e) => {
                    tracing::error!(
                        channel_id = msg.channel_id.get(),
                        "Failed to get channel with ctx.http: {:?}",
                        e
                    );
                    return;
                }
            };

            if let Err(e) =
                crate::handlers::insert_discord_channel(&ctx, &db_pool, &guild_channel).await
            {
                tracing::error!(error = %e, "channel insertion into db failed");
            } else {
                tracing::debug!("Upserted channel metadata");
            }

            if let Err(e) = crate::handlers::process_discord_message_and_children(
                &ctx, &db_pool, &s3_client, &s3_bucket, &msg,
            )
            .await
            {
                tracing::error!(error = %e, "Handler failed for message");
            }

            if msg.channel_id == intro_channel_id {
                tracing::info!("Received message in intro channel");
            }

            tokio::time::sleep(Duration::from_secs(5)).await;

            // Mnemos no longer creates new threads as it has been removed from the server :(
            // let thread_id = serenity::all::ChannelId::new(msg.id.get());
            // match ctx.http.get_channel(thread_id).await {
            //     Ok(c) => {
            //         tracing::info!(thread_id = %c.id(), "Found automatically opened intro thread");
            //     }
            //     Err(e) => {
            //         tracing::warn!(error = %e, "Failed to get new thread channel id for intro message after 5 seconds");
            //         return;
            //     }
            // };

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
        let msg = match new_msg {
            Some(m) => m,
            None => {
                tracing::warn!(
                    "Message edit event was dispatched, however new message content is None"
                );
                return;
            }
        };

        if msg.guild_id != Some(self.guild_id) {
            return;
        }

        // bc the handler outlives the function, we need to clone
        let db_pool = self.db_pool.clone();
        let ctx_clone = ctx.clone();

        tokio::spawn(async move {
            if let Err(e) = crate::handlers::handle_message_edit(&ctx_clone, &db_pool, &msg).await {
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

        let db_pool = self.db_pool.clone();
        let s3_client = self.s3_client.clone();
        let s3_bucket = self.s3_bucket.clone();

        tokio::spawn(async move {
            if let Err(e) = crate::handlers::handle_reaction_add(
                &ctx,
                &db_pool,
                &s3_client,
                &s3_bucket,
                &add_reaction,
            )
            .await
            {
                tracing::error!(error = %e, "Failed to process reaction add handler");
            } else {
                tracing::debug!("Successfully logged reaction addition");
            }
        });
    }

    #[tracing::instrument(skip_all, fields(msg_id=remove_reaction.message_id.get()))]
    async fn reaction_remove(&self, _ctx: Context, remove_reaction: Reaction) {
        let span = tracing::info_span!(
            "reaction_remove_handler",
            msg_id = remove_reaction.message_id.get()
        );
        let _enter = span.enter();
        tracing::debug!("starting reaction remove handler");

        if remove_reaction.guild_id != Some(self.guild_id) {
            return;
        }

        let db_pool = self.db_pool.clone();

        tokio::spawn(async move {
            if let Err(e) =
                crate::handlers::handle_reaction_remove(&db_pool, &remove_reaction).await
            {
                tracing::error!(error = %e, "Failed to delete reaction from database");
            } else {
                tracing::debug!("Successfully logged reaction removal");
            }
        });
    }
}
