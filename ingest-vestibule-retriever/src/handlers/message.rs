use crate::AppCtx;
use crate::handlers::attachments::{insert_message_attachments, insert_stickers};
use crate::handlers::channel::ensure_discord_channel;
use crate::handlers::discord_user::ensure_user;
use chrono::{TimeZone, Utc};
use serenity::all::{Message, MessageType};

/// Returns a synthetic content string for system messages (joins, boosts, pins, ...).
/// Returns `None` for regular messages/replies where the original content should be kept.
fn system_message_content(msg: &Message) -> Option<String> {
    let author = &msg.author.name;
    let s = match msg.kind {
        MessageType::Regular
        | MessageType::InlineReply
        | MessageType::ChatInputCommand
        | MessageType::ContextMenuCommand => return None,
        MessageType::GroupRecipientAddition => format!("{} added a recipient to the group", author),
        MessageType::GroupRecipientRemoval => {
            format!("{} removed a recipient from the group", author)
        }
        MessageType::GroupCallCreation => format!("{} started a call", author),
        MessageType::GroupNameUpdate => format!("{} changed the group name", author),
        MessageType::GroupIconUpdate => format!("{} changed the group icon", author),
        MessageType::PinsAdd => format!("{} pinned a message", author),
        MessageType::MemberJoin => format!("user {} joined the server", author),
        MessageType::NitroBoost => format!("{} boosted the server", author),
        MessageType::NitroTier1 => "the server reached Nitro Boost tier 1".to_string(),
        MessageType::NitroTier2 => "the server reached Nitro Boost tier 2".to_string(),
        MessageType::NitroTier3 => "the server reached Nitro Boost tier 3".to_string(),
        MessageType::ChannelFollowAdd => format!("{} followed a news channel", author),
        MessageType::GuildDiscoveryDisqualified => {
            "the server was disqualified from Discovery".to_string()
        }
        MessageType::GuildDiscoveryRequalified => {
            "the server was requalified for Discovery".to_string()
        }
        MessageType::GuildDiscoveryGracePeriodInitialWarning => {
            "initial Discovery grace period warning".to_string()
        }
        MessageType::GuildDiscoveryGracePeriodFinalWarning => {
            "final Discovery grace period warning".to_string()
        }
        MessageType::ThreadCreated => format!("{} created a thread", author),
        MessageType::ThreadStarterMessage => "[thread starter message]".to_string(),
        MessageType::GuildInviteReminder => "[guild invite reminder]".to_string(),
        MessageType::AutoModAction => "[auto-moderation action]".to_string(),
        MessageType::RoleSubscriptionPurchase => {
            format!("{} purchased a role subscription", author)
        }
        MessageType::InteractionPremiumUpsell => "[interaction premium upsell]".to_string(),
        MessageType::StageStart => "[stage started]".to_string(),
        MessageType::StageEnd => "[stage ended]".to_string(),
        MessageType::StageSpeaker => format!("{} is now a stage speaker", author),
        MessageType::StageTopic => "[stage topic changed]".to_string(),
        MessageType::GuildApplicationPremiumSubscription => {
            "[application premium subscription]".to_string()
        }
        MessageType::GuildIncidentAlertModeEnabled => "[incident alert mode enabled]".to_string(),
        MessageType::GuildIncidentAlertModeDisabled => "[incident alert mode disabled]".to_string(),
        MessageType::GuildIncidentReportRaid => "[incident report: raid]".to_string(),
        MessageType::GuildIncidentReportFalseAlarm => "[incident report: false alarm]".to_string(),
        MessageType::PurchaseNotification => format!("{} made a purchase", author),
        _ => return None,
    };
    Some(s)
}

/// Processes a single Discord message completely:
///   1. Inserts author if not in db (VestibuleUser + DiscordAccount)
///   2. Inserts message row
///   3. Downloads and stores attachments as MediaAssets & MessageAttachments and uploads them to S3 bucket
///   4. Fetches and stores reactions
///
/// Used by both the live `EventHandler` and `historical_scan`.
pub async fn ensure_message(ctx: &AppCtx, msg: &Message) -> color_eyre::Result<()> {
    if ctx
        .message_id_cache
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&(msg.id.get() as i64))
    {
        return Ok(());
    }

    ensure_discord_channel(ctx, msg.channel_id).await?;

    ensure_user(ctx, &msg.author).await?;

    // TODO When its all done, how do we make sure every messages' `in_reply_to` message is actually in the db: Insert messages by first sent = first inserted
    let in_reply_to = msg
        .message_reference
        .as_ref()
        .and_then(|r| r.message_id.map(|id| id.get() as i64));

    #[allow(clippy::collapsible_if)]
    if let Some(parent_id) = in_reply_to {
        if !ctx
            .message_id_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&parent_id)
        {
            match ctx
                .discord_ctx
                .http
                .get_message(
                    msg.channel_id,
                    serenity::all::MessageId::new(parent_id.try_into()?),
                )
                .await
            {
                Ok(parent_msg) => {
                    // I have no idea why I need to box here and what it does in an async context but it works
                    Box::pin(ensure_message(ctx, &parent_msg)).await?;
                }
                Err(serenity::Error::Http(http_err)) => {
                    let is_not_found = match &http_err {
                        serenity::all::HttpError::UnsuccessfulRequest(res) => {
                            res.status_code == serenity::all::StatusCode::NOT_FOUND
                        }
                        _ => false,
                    };

                    if is_not_found {
                        tracing::debug!(
                            msg_id = parent_id,
                            "Parent message not found on Discord, inserting placeholder"
                        );
                        sqlx::query!(
                            "INSERT INTO messages (message_id, channel_id, sent_by, content, sent_at, last_edited, deleted_at, in_reply_to, added_at)
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                             ON CONFLICT (message_id) DO NOTHING",
                            parent_id,
                            msg.channel_id.get() as i64,
                            0000000000, // Fallback to 0000000000, this is a  dummy account inserted into unidb for this purpose
                            "[deleted message]",
                            Utc.timestamp_opt(0, 0).unwrap(),
                            Option::<chrono::DateTime<Utc>>::None,
                            Utc.timestamp_opt(0, 0).unwrap(),
                            Option::<i64>::None,
                            chrono::Utc::now()
                        )
                        .execute(&ctx.db_pool)
                        .await?;
                        ctx.message_id_cache
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(parent_id);
                    } else {
                        return Err(serenity::Error::Http(http_err).into());
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    let content = system_message_content(msg).unwrap_or_else(|| msg.content.clone());
    sqlx::query!(
        "INSERT INTO messages (message_id, channel_id, sent_by, content, sent_at, last_edited, deleted_at, in_reply_to, added_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (message_id) DO NOTHING",
        msg.id.get() as i64,
        msg.channel_id.get() as i64,
        msg.author.id.get() as i64,
        content,
        *msg.timestamp,
        msg.edited_timestamp.map(|t| *t),
        Option::<chrono::DateTime<Utc>>::None,
        in_reply_to,
        chrono::Utc::now()
    )
    .execute(&ctx.db_pool)
    .await?;
    ctx.message_id_cache
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(msg.id.get() as i64);

    if !msg.attachments.is_empty() {
        if let Err(e) = insert_message_attachments(ctx, msg).await {
            tracing::error!(error = %e, "Failed to process attachments");
        }

        if let Err(e) = insert_stickers(ctx, msg).await {
            tracing::error!(error = %e, "Failed to process stickers");
        }
    }

    Ok(())
}
