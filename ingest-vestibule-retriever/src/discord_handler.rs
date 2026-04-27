use std::time::Duration;

use serenity::{all::Message, async_trait, prelude::*};
use sqlx::PgPool;

pub struct DiscordEventHandler {
    pub db_pool: PgPool,
    pub intro_channel_id: serenity::all::ChannelId,
    pub guild_id: serenity::all::GuildId,
}

#[async_trait]
impl EventHandler for DiscordEventHandler {
    async fn message(&self, ctx: Context, msg: Message) {
        let span = tracing::info_span!("new_msg_handler", msg_id = msg.id.get());
        let _enter = span.enter();

        tracing::debug!("starting message handler");

        if msg.guild_id != Some(self.guild_id) {
            return;
        }

        let db_pool = self.db_pool.clone();
        let intro_channel_id = self.intro_channel_id;

        tokio::spawn(async move {
            let span = tracing::info_span!("process_message_task", msg_id = msg.id.get());
            let _enter = span.enter();

            tracing::debug!("started message processing task");

            if msg.channel_id == intro_channel_id {
                tracing::info!("Received message in intro channel");
            }

            tokio::time::sleep(Duration::from_secs(5)).await;

            let thread_id = serenity::all::ChannelId::new(msg.id.get());
            let mnemos_opened_thread_id = match ctx.http.get_channel(thread_id).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to get new thread channel id for intro message after 5 seconds");
                    return;
                }
            };

            tracing::info!(thread_id = %mnemos_opened_thread_id.id(), "Found thread");
            tracing::debug!("finished task");
        });
    }
}
