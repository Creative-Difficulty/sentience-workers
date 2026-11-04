use crate::AppCtx;
use serenity::all::GuildId;
use tracing::Instrument as _;

#[tracing::instrument(skip_all)]
pub async fn run_historical_scan(ctx: AppCtx, guild_id: GuildId) -> color_eyre::Result<()> {
    tracing::info!("Starting historical message scan across all channels in guild");

    let channels = match ctx.discord.guild_channels(guild_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "Failed to get guild channels");
            return Err(e);
        }
    };

    for channel in channels {
        let channel_id = channel.id;
        async {
            tracing::info!("Scanning channel: \"{}\", with id {}", channel.name, channel_id);

            let mut last_message_id = None;
            let mut all_messages = Vec::new();

            loop {
                match ctx.discord.messages_before(channel_id, last_message_id).await {
                    Ok(messages) => {
                        if messages.is_empty() {
                            break;
                        }
                        last_message_id = messages.last().map(|m| m.id);
                        all_messages.extend(messages);
                    }
                    Err(e) => {
                        // This is mostly due to missing access permissions
                        tracing::debug!(error = %e, "Failed to get messages for channel");
                        break;
                    }
                }
            }

            let total = all_messages.len();
            for (i, msg) in all_messages.iter().rev().enumerate() {
                tracing::info!("Processing message {}/{} in channel \"{}\"", i + 1, total, channel.name);
                if let Err(e) = crate::handlers::ensure_message(&ctx, msg).await {
                    tracing::error!(error = %e, msg_id = msg.id.get(), "Failed to ensure message is in database");
                }

                if let Err(e) =
                    crate::historical::reactions::insert_all_reactions_for_message(&ctx, msg).await
                {
                    tracing::error!(error = %e, msg_id = msg.id.get(), "Failed to process all reactions of message");
                }
            }

            tracing::info!("Finished scanning channel: {} ({})", channel.name, channel_id);
        }
        .instrument(tracing::info_span!("scan_channel", channel_id = channel_id.get()))
        .await;
    }

    tracing::info!("Finished historical scan for guild");
    Ok(())
}
