use crate::AppCtx;
use crate::db::{self, EditOutcome};
use crate::mapping::{edit_content, snowflake};
use serenity::all::{Message, MessageUpdateEvent};

/// Records an edit given the full, current message.
///
/// The message is ingested first if it is unknown (its current content becomes the baseline; the
/// pre-edit content is not knowable). Then the stored content is compared with the new one, so
/// embed-only updates and duplicate events do not create edit rows.
#[tracing::instrument(skip_all, fields(msg_id=msg.id.get()))]
pub async fn handle_message_edit(ctx: &AppCtx, msg: &Message) -> color_eyre::Result<EditOutcome> {
    crate::handlers::ensure_message(ctx, msg).await?;

    let (Some(edited_at), Some(new_content)) = (
        msg.edited_timestamp,
        edit_content(Some(msg.kind), Some(&msg.content)),
    ) else {
        return Ok(EditOutcome::Unchanged);
    };
    db::record_edit(
        &ctx.db_pool,
        snowflake(msg.id.get()),
        &new_content,
        *edited_at,
    )
    .await
}

/// Records an edit from a gateway `MESSAGE_UPDATE` event.
///
/// serenity only passes the full new message to `message_update` when the message is in its
/// cache, and the default cache keeps no messages, so the event itself must be used.
#[tracing::instrument(skip_all, fields(msg_id=event.id.get()))]
pub async fn handle_message_update(
    ctx: &AppCtx,
    event: &MessageUpdateEvent,
) -> color_eyre::Result<EditOutcome> {
    let (Some(edited_at), Some(new_content)) = (
        event.edited_timestamp,
        edit_content(event.kind, event.content.as_deref()),
    ) else {
        return Ok(EditOutcome::Unchanged);
    };

    match db::record_edit(
        &ctx.db_pool,
        snowflake(event.id.get()),
        &new_content,
        *edited_at,
    )
    .await?
    {
        EditOutcome::NotFound => match ctx.discord.get_message(event.channel_id, event.id).await? {
            Some(msg) => handle_message_edit(ctx, &msg).await,
            None => Ok(EditOutcome::NotFound),
        },
        outcome => Ok(outcome),
    }
}
