pub mod discord_handler;
pub mod handlers;

// TODO Keep this around until impl of message deletion marking handler
#[tracing::instrument(skip_all, fields(message_id = msg_id))]
pub async fn delete_message(pool: &sqlx::PgPool, msg_id: i64) -> color_eyre::Result<()> {
    tracing::trace!("Marking message as deleted in database");
    sqlx::query!(
        "UPDATE messages SET deleted_at = NOW() WHERE message_id = $1",
        msg_id
    )
    .execute(pool)
    .await?;
    tracing::debug!("Successfully marked message as deleted");
    Ok(())
}
