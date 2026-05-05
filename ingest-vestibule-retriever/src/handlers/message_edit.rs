use ormlite::Model as _;
use serenity::all::{Context, Message};
use sqlx::PgPool;
use unidb::models::MessageEdit;
use uuid::Uuid;

#[tracing::instrument(skip_all, fields(msg_id=msg.id.get()))]
pub async fn handle_message_edit(
    _ctx: &Context,
    pool: &PgPool,
    msg: &Message,
) -> color_eyre::Result<()> {
    if let Some(edited_at) = msg.edited_timestamp {
        let old_content = sqlx::query_scalar!(
            "SELECT content FROM messages WHERE message_id = $1",
            msg.id.get() as i64
        )
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "Message (id={}) that was supposedly edited does not exist in database",
                msg.id.get()
            )
        })?;

        let edit = MessageEdit {
            id: Uuid::new_v4(),
            message_id: msg.id.get() as i64,
            old_content,
            edited_at: *edited_at,
        };

        edit.insert(pool).await?;

        // Also update the messages table row to the current state
        sqlx::query!(
            "UPDATE messages SET content = $1, last_edited = $2 WHERE message_id = $3",
            msg.content,
            *edited_at,
            msg.id.get() as i64
        )
        .execute(pool)
        .await?;
    }

    Ok(())
}
