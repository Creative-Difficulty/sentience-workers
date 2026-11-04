use crate::mapping::snowflake;
use crate::{AppCtx, db};
use serenity::all::Message;

#[tracing::instrument(skip_all, fields(msg_id=msg.id.get()))]
pub async fn handle_message_edit(ctx: &AppCtx, msg: &Message) -> color_eyre::Result<()> {
    crate::handlers::ensure_message(ctx, msg).await?;

    if let Some(edited_at) = msg.edited_timestamp {
        let message_id = snowflake(msg.id.get());
        let old_content = db::message_content(&ctx.db_pool, message_id)
            .await?
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "Message (id={}) that was supposedly edited does not exist in database",
                    msg.id.get()
                )
            })?;

        db::insert_edit_and_update(
            &ctx.db_pool,
            message_id,
            old_content,
            &msg.content,
            *edited_at,
        )
        .await?;
    }

    Ok(())
}
