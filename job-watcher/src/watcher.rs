pub async fn start_watching(pool: &sqlx::PgPool) -> color_eyre::Result<()> {
    let jobs = sqlx::query!("SELECT message_id FROM messages")
        .fetch_all(pool)
        .await?;
    tracing::info!("{:?}", jobs);
    Ok(())
}
