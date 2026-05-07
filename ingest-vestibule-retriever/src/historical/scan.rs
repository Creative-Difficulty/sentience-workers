use crate::AppCtx;
use serenity::all::GuildId;
use serenity::builder::GetMessages;
use std::time::Duration;

#[tracing::instrument(skip_all)]
pub async fn run_historical_scan(ctx: AppCtx, guild_id: GuildId) -> color_eyre::Result<()> {
    tracing::info!("Starting historical message scan across all channels in guild");

    let channels = match guild_id.channels(&ctx.discord_ctx.http).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "Failed to get guild channels");
            return Err(e.into());
        }
    };

    for (channel_id, channel) in channels {
        use tracing::Instrument;
        async {
            tracing::info!("Scanning channel: \"{}\", with id {}", channel.name, channel_id);

            let mut last_message_id = None;
            let mut all_messages = Vec::new();

            loop {
                let mut builder = GetMessages::new().limit(100);
                if let Some(id) = last_message_id {
                    builder = builder.before(id);
                }

                match channel_id.messages(&ctx.discord_ctx.http, builder).await {
                    Ok(messages) => {
                        if messages.is_empty() {
                            break;
                        }

                        last_message_id = messages.last().map(|m| m.id);
                        all_messages.extend(messages);

                        // A slight delay to avoid hammering the Discord API too hard
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Err(e) => {
                        // This is mostly due to missing access permissions
                        tracing::warn!(error = %e, "Failed to get messages for channel");
                        break;
                    }
                }
            }


            if !all_messages.is_empty() {
                tracing::info!(
                    "Found {} messages in channel \"{}\", processing...",
                    all_messages.len(),
                    channel.name
                );
            }

            for msg in all_messages.iter().rev() {
                tracing::trace!("Processing message {}", msg.id.get());
                if let Err(e) = crate::handlers::ensure_message(&ctx, msg).await {
                    tracing::error!(error = %e, msg_id = msg.id.get(), "Failed to ensure message is in database");
                }

                if let Err(e) =
                    crate::historical::reactions::insert_all_reactions_for_message(&ctx, msg).await
                {
                    tracing::error!(error = %e, msg_id = msg.id.get(), "Failed to process all reactions of message");
                }
            }

            tracing::info!(
                "Finished scanning channel: {} ({})",
                channel.name,
                channel_id
            );
        }
        .instrument(tracing::info_span!("scan_channel", channel_id = channel_id.get()))
        .await;
    }

    tracing::info!("Finished historical scan for guild");
    Ok(())
}
