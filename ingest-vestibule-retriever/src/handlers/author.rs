use ormlite::Model;
use serenity::all::Message;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use unidb::models::{DiscordAccount, MediaAsset, VestibuleUser};
use uuid::Uuid;

pub async fn upsert_discord_user(
    pool: &PgPool,
    s3_client: &aws_sdk_s3::Client,
    s3_bucket: &str,
    msg: &Message,
) -> color_eyre::Result<()> {
    if msg.author.bot {
        return Ok(());
    }

    let discord_user_id = msg.author.id.get() as i64;

    let existing_account = sqlx::query!(
        "SELECT discord_user_id, vestibule_user_id FROM discord_accounts WHERE discord_user_id = $1",
        discord_user_id
    )
    .fetch_optional(pool)
    .await?;

    if existing_account.is_none() {
        // TODO fix mixed UUID types: Postgres generates v7 (i think) but I like v4 the best
        let vestibule_user_id = Uuid::new_v4();

        let vestibule_user = VestibuleUser {
            id: vestibule_user_id,
            nickname: Some(
                msg.author
                    .global_name
                    .clone()
                    .unwrap_or_else(|| msg.author.name.clone()),
            ),
            intro_message_id: None,
            score_id: None,
            score_last_updated: None,
            current_diagram: None,
            current_diagram_last_updated: None,
            intro_diagram: None,
        };

        // TODO fix error handling
        vestibule_user.insert(pool).await?;

        let discord_account = DiscordAccount {
            discord_user_id,
            vestibule_user_id,
            username: msg.author.name.clone(),
            display_name: msg
                .author
                .global_name
                .clone()
                .unwrap_or(msg.author.name.clone()),
        };

        if let Err(e) = discord_account.insert(pool).await {
            tracing::error!(error = %e, "Could not upsert discord account");
        }
    }

    if let Some(avatar_url) = msg.author.avatar_url() {
        let avatar_hash = msg
            .author
            .avatar
            .as_ref()
            .map(|h| h.to_string())
            .unwrap_or_else(|| "default".to_string());
        let ext = if avatar_url.contains(".gif") {
            "gif"
        } else {
            "png"
        };
        let object_key = format!("discord/avatars/{}/{}", discord_user_id, avatar_hash);

        // Check if we already have this avatar
        let existing_avatar = sqlx::query_scalar!(
            "SELECT id FROM media_assets WHERE object_key = $1",
            object_key
        )
        .fetch_optional(pool)
        .await?;

        #[allow(clippy::collapsible_if)]
        if existing_avatar.is_none() {
            if let Ok(resp) = reqwest::get(&avatar_url).await {
                if let Ok(bytes) = resp.bytes().await {
                    let content_hash = hex::encode(Sha256::digest(&bytes));
                    let asset_id = Uuid::new_v4();

                    let content_type = format!("image/{}", ext);

                    let stream = aws_sdk_s3::primitives::ByteStream::from(bytes.to_vec());

                    if let Err(e) = s3_client
                        .put_object()
                        .bucket(s3_bucket)
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
                    .insert(pool)
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
