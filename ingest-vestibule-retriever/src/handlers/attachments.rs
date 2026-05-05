use ormlite::Model as _;
use serenity::all::Message;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use unidb::models::{MediaAsset, MessageAttachment};
use uuid::Uuid;

/// Uploads from URL to S3 bucket and check if the data already exists in the media_assets table via hash. In that case it returns the media asset id of the identical asset instead of uploading twice.
/// Returns ID of media asset it inserted.
/// Because S3 upload happens first, worse case is that we have stray files in the S3 bucket
pub async fn process_and_store_media(
    pool: &PgPool,
    s3_client: &aws_sdk_s3::Client,
    s3_bucket: &str,
    object_key: String,
    url: &str,
    content_type: String,
) -> color_eyre::Result<Uuid> {
    let bytes = match reqwest::get(url).await {
        Ok(resp) => match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                // TODO do we log and return or just return?
                tracing::error!(error = %e, url = %url, "Failed to download media");
                return Err(e.into());
            }
        },
        Err(e) => {
            tracing::error!(error = %e, url = %url, "Failed to fetch media URL");
            return Err(e.into());
        }
    };

    let content_hash = hex::encode(Sha256::digest(&bytes));
    if let Some(already_uploaded_uuid) = sqlx::query_scalar!(
        "SELECT id FROM media_assets WHERE content_hash = $1",
        content_hash
    )
    .fetch_optional(pool)
    .await?
    {
        return Ok(already_uploaded_uuid);
    };

    let size_bytes = bytes.len() as i64;
    let stream = aws_sdk_s3::primitives::ByteStream::from(bytes);

    if let Err(e) = s3_client
        .put_object()
        .bucket(s3_bucket)
        .key(&object_key)
        .body(stream)
        .content_type(&content_type)
        .send()
        .await
    {
        tracing::error!(error = %e, object_key = %object_key, "Failed to upload media to S3 bucket");
        return Err(e.into());
    }

    let asset_id = Uuid::new_v4();

    if let Err(e) = (MediaAsset {
        id: asset_id,
        content_type,
        object_key,
        size_bytes: Some(size_bytes),
        content_hash: Some(content_hash),
        embedding: None,
    })
    .insert(pool)
    .await
    {
        tracing::error!("Failed to insert media asset: {}", e);
        return Err(e.into());
    }

    Ok(asset_id)
}

#[tracing::instrument(skip_all, fields(message_id=msg.id.get(), attachment_id = tracing::field::Empty))]
pub async fn insert_message_attachments(
    pool: &PgPool,
    s3_client: &aws_sdk_s3::Client,
    s3_bucket: &str,
    msg: &Message,
) -> color_eyre::Result<()> {
    for attachment in &msg.attachments {
        tracing::Span::current().record("attachment_id", attachment.id.get());

        let discord_attachment_id = attachment.id.get() as i64;
        match sqlx::query_scalar!(
            "SELECT ma.id FROM message_attachments ma WHERE ma.message_id = $1 AND ma.asset_id IN (SELECT id FROM media_assets WHERE object_key = $2)",
            msg.id.get() as i64,
            format!("discord/attachments/{}/{}", msg.id.get(), discord_attachment_id)
        )
        .fetch_optional(pool).await {
            Ok(Some(_)) => {
                tracing::debug!("Attachment is already stored in db attachments table, skipping");
                continue;
            }
            Ok(None) => (),
            Err(e) => {
                tracing::error!("{}", e);
                continue;
            }
        };

        // TODO warn on fallback
        let _ext = attachment.filename.rsplit('.').next().unwrap_or("bin");

        let content_type = attachment
            .content_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let object_key = format!(
            "discord/attachments/{}/{}",
            msg.id.get(),
            discord_attachment_id,
        );

        let asset_id = match process_and_store_media(
            pool,
            s3_client,
            s3_bucket,
            object_key,
            &attachment.url,
            content_type,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    "Error while downloading media and uploading it to S3: {}",
                    e
                );
                continue;
            }
        };

        if let Err(e) = (MessageAttachment {
            id: Uuid::new_v4(),
            message_id: msg.id.get() as i64,
            asset_id,
            added_at: chrono::Utc::now(),
            deleted_at: None,
        })
        .insert(pool)
        .await
        {
            tracing::error!("{}", e);
            continue;
        };

        tracing::debug!(
            asset_id = %asset_id,
            "Stored attachment and inserted media asset and attachment row"
        );
    }

    Ok(())
}

pub async fn insert_stickers(
    msg: &Message,
    pool: &PgPool,
    s3_client: &aws_sdk_s3::Client,
    s3_bucket: &str,
) -> color_eyre::Result<()> {
    for sticker in &msg.sticker_items {
        let sticker_id = sticker.id.get() as i64;
        let existing = sqlx::query_scalar!(
            "SELECT ma.id FROM message_attachments ma WHERE ma.message_id = $1 AND ma.asset_id IN (SELECT id FROM media_assets WHERE object_key LIKE $2)",
            msg.id.get() as i64,
            format!("discord/stickers/{}.%", sticker_id)
        )
        .fetch_optional(pool)
        .await?;

        if existing.is_some() {
            continue;
        }

        let ext = match sticker.format_type {
            serenity::all::StickerFormatType::Png | serenity::all::StickerFormatType::Apng => "png",
            serenity::all::StickerFormatType::Lottie => "json",
            serenity::all::StickerFormatType::Gif => "gif",
            _ => "bin",
        };

        if ext == "bin" {
            tracing::warn!(
                "Failed to determine file type of sticker, so falling back to .bin extension"
            );
            continue;
        }

        // TODO Where is this documented?
        let sticker_url = format!(
            "https://cdn.discordapp.com/stickers/{}.{}",
            sticker.id.get(),
            ext
        );

        let object_key = format!("discord/stickers/{}.{}", sticker.id.get(), ext);
        let mut asset_id = sqlx::query_scalar!(
            "SELECT id FROM media_assets WHERE object_key = $1",
            object_key
        )
        .fetch_optional(pool)
        .await?;

        if asset_id.is_none() {
            // TODO Refine to not use raw JSON (these are likely Lottie format stickers)
            let content_type = if ext == "json" {
                "application/json".to_string()
            } else {
                format!("image/{}", ext)
            };

            asset_id = Some(
                match process_and_store_media(
                    pool,
                    s3_client,
                    s3_bucket,
                    object_key.clone(),
                    &sticker_url,
                    content_type,
                )
                .await
                {
                    Ok(u) => u,
                    Err(e) => {
                        tracing::error!("{}", e);
                        continue;
                    }
                },
            );
        }

        #[allow(clippy::collapsible_if)]
        if let Some(id) = asset_id {
            if let Err(e) = (MessageAttachment {
                id: Uuid::new_v4(),
                message_id: msg.id.get() as i64,
                asset_id: id,
                added_at: chrono::Utc::now(),
                deleted_at: None,
            })
            .insert(pool)
            .await
            {
                tracing::error!("Failed to insert sticker attachment row: {}", e);
                continue;
            };
        }
    }
    Ok(())
}
