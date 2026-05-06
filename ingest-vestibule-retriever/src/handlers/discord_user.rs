use crate::AppCtx;
use ormlite::Model;
use sha2::{Digest, Sha256};
use unidb::models::{DiscordAccount, MediaAsset, VestibuleUser};
use uuid::Uuid;

pub async fn ensure_user(ctx: &AppCtx, user: &serenity::all::User) -> color_eyre::Result<()> {
    let existing_account = sqlx::query!(
        "SELECT discord_user_id, vestibule_user_id FROM discord_accounts WHERE discord_user_id = $1",
        user.id.get() as i64
    )
    .fetch_optional(&ctx.db_pool)
    .await?;

    if existing_account.is_none() {
        // TODO fix mixed UUID types: Postgres generates v7 (i think) but I like v4 the best
        let vestibule_user_id = Uuid::new_v4();

        let vestibule_user = VestibuleUser {
            id: vestibule_user_id,
            nickname: Some(
                user.global_name
                    .clone()
                    .unwrap_or_else(|| user.name.clone()),
            ),
            intro_message_id: None,
            score_id: None,
            score_last_updated: None,
            current_diagram: None,
            current_diagram_last_updated: None,
            intro_diagram: None,
        };

        // TODO fix error handling
        vestibule_user.insert(&ctx.db_pool).await?;

        let discord_account = DiscordAccount {
            discord_user_id: user.id.get() as i64,
            vestibule_user_id,
            username: user.name.clone(),
            display_name: user.global_name.clone().unwrap_or(user.name.clone()),
        };

        if let Err(e) = discord_account.insert(&ctx.db_pool).await {
            tracing::error!(error = %e, "Could not upsert discord account");
        }
    }

    if let Some(avatar_url) = user.avatar_url() {
        let avatar_hash = user
            .avatar
            .as_ref()
            .map(|h| h.to_string())
            .unwrap_or_else(|| "default".to_string());

        let ext = if avatar_url.contains(".gif") {
            "gif"
        } else {
            "webp"
        };

        let object_key = format!("discord/avatars/{}/{}", user.id.get(), avatar_hash);

        // Check if we already have this avatar
        let existing_avatar = sqlx::query_scalar!(
            "SELECT id FROM media_assets WHERE object_key = $1",
            object_key
        )
        .fetch_optional(&ctx.db_pool)
        .await?;

        #[allow(clippy::collapsible_if)]
        if existing_avatar.is_none() {
            if let Ok(resp) = reqwest::get(&avatar_url).await {
                if let Ok(bytes) = resp.bytes().await {
                    let content_hash = hex::encode(Sha256::digest(&bytes));
                    let asset_id = Uuid::new_v4();

                    let content_type = format!("image/{}", ext);

                    let stream = aws_sdk_s3::primitives::ByteStream::from(bytes.to_vec());

                    if let Err(e) = ctx
                        .s3_client
                        .put_object()
                        .bucket(&ctx.s3_bucket)
                        .key(&object_key)
                        .body(stream)
                        .content_type(&content_type)
                        .send()
                        .await
                    {
                        tracing::error!(error = %e, object_key = %object_key, "Failed to write avatar to S3 bucket");
                    }

                    if let Err(e) = (MediaAsset {
                        id: asset_id,
                        content_type: content_type.clone(),
                        object_key: object_key.clone(),
                        size_bytes: Some(bytes.len() as i64),
                        content_hash: Some(content_hash),
                        embedding: None,
                    })
                    .insert(&ctx.db_pool)
                    .await
                    {
                        tracing::error!(error = %e, "Failed to insert avatar media asset");
                    }
                }
            }
        }
    }

    Ok(())
}
