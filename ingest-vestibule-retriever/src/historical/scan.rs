use std::collections::HashSet;

use crate::AppCtx;
use serenity::all::{ChannelId, ChannelType, GuildChannel, GuildId, MessageId};
use tracing::Instrument as _;

/// Channel types that can contain threads.
fn can_have_threads(kind: ChannelType) -> bool {
    matches!(
        kind,
        ChannelType::Text | ChannelType::News | ChannelType::Forum
    ) || u8::from(kind) == 16 // media channel
}

/// Scans every channel of the guild, then every active thread and every archived public thread.
///
/// Archived *private* threads are not scanned: listing them requires MANAGE_THREADS.
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

    let mut scanned = HashSet::new();
    for channel in &channels {
        scan_channel(&ctx, channel.id, &channel.name).await;
        scanned.insert(channel.id);
    }

    match ctx.discord.active_threads(guild_id).await {
        Ok(threads) => {
            for t in threads {
                if scanned.insert(t.id) {
                    scan_channel(&ctx, t.id, &t.name).await;
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "Failed to list active threads"),
    }

    for channel in channels.iter().filter(|c| can_have_threads(c.kind)) {
        for t in archived_public_threads(&ctx, channel).await {
            if scanned.insert(t.id) {
                scan_channel(&ctx, t.id, &t.name).await;
            }
        }
    }

    tracing::info!("Finished historical scan for guild");
    Ok(())
}

/// All archived public threads of `channel`, following Discord's `before` pagination.
async fn archived_public_threads(ctx: &AppCtx, channel: &GuildChannel) -> Vec<GuildChannel> {
    let mut out = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let page = match ctx
            .discord
            .archived_public_threads(channel.id, before.clone())
            .await
        {
            Ok(p) => p,
            Err(e) => {
                // Mostly missing READ_MESSAGE_HISTORY on the channel.
                tracing::debug!(error = %e, channel_id = channel.id.get(), "Failed to list archived threads");
                break;
            }
        };
        let next = page
            .threads
            .iter()
            .filter_map(|t| t.thread_metadata.and_then(|m| m.archive_timestamp))
            .min()
            .map(|ts| ts.to_string());
        out.extend(page.threads);
        match next {
            // Stop if the cursor does not move (defensive against a misbehaving API).
            Some(n) if page.has_more && before.as_ref() != Some(&n) => before = Some(n),
            _ => break,
        }
    }
    out
}

/// Scans one channel or thread from its oldest message forward, one page (100 messages) at a
/// time: each page is written before the next is fetched, so memory stays bounded and a crash
/// keeps the progress made so far. Oldest-first also means reply parents are usually stored
/// before their replies.
async fn scan_channel(ctx: &AppCtx, channel_id: ChannelId, name: &str) {
    async {
        tracing::info!("Scanning channel: \"{}\", with id {}", name, channel_id);
        // Snowflake 1 is older than any message.
        let mut after = MessageId::new(1);
        let mut processed = 0usize;
        loop {
            let mut page = match ctx.discord.messages_after(channel_id, after).await {
                Ok(m) => m,
                Err(e) => {
                    // This is mostly due to missing access permissions
                    tracing::debug!(error = %e, "Failed to get messages for channel");
                    break;
                }
            };
            page.retain(|m| m.id > after);
            if page.is_empty() {
                break;
            }
            page.sort_by_key(|m| m.id);
            after = page.last().map(|m| m.id).unwrap_or(after);

            for msg in &page {
                if let Err(e) = crate::handlers::ensure_message(ctx, msg).await {
                    tracing::error!(error = %e, msg_id = msg.id.get(), "Failed to ensure message is in database");
                }
                if let Err(e) =
                    crate::historical::reactions::insert_all_reactions_for_message(ctx, msg).await
                {
                    tracing::error!(error = %e, msg_id = msg.id.get(), "Failed to process all reactions of message");
                }
            }
            processed += page.len();
            tracing::info!(processed, "Processed page of messages in channel \"{}\"", name);
        }
        tracing::info!(processed, "Finished scanning channel: {} ({})", name, channel_id);
    }
    .instrument(tracing::info_span!("scan_channel", channel_id = channel_id.get()))
    .await
}
