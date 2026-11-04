use crate::mapping::{AssetSpec, attachment_spec, snowflake, sticker_spec};
use crate::{AppCtx, db};
use serenity::all::Message;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Downloads `spec.url`, and stores it in the object store + `media_assets`.
/// If identical content (by SHA-256) is already stored, the existing asset id is returned instead
/// of uploading twice. Because the upload happens first, the worst case is a stray object.
pub async fn download_and_store_media_asset(
    ctx: &AppCtx,
    spec: &AssetSpec,
) -> color_eyre::Result<Uuid> {
    let bytes = match ctx.fetcher.fetch(&spec.url).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, url = %spec.url, "Failed to download media");
            return Err(e);
        }
    };

    let content_hash = hex::encode(Sha256::digest(&bytes));
    if let Some(existing) = db::find_asset_by_hash(&ctx.db_pool, &content_hash).await? {
        return Ok(existing);
    }

    let size_bytes = bytes.len() as i64;
    if let Err(e) = ctx
        .store
        .put(&spec.object_key, bytes, &spec.content_type)
        .await
    {
        tracing::error!(error = %e, object_key = %spec.object_key, "Failed to upload media to S3 bucket");
        return Err(e);
    }

    match db::insert_media_asset(
        &ctx.db_pool,
        &spec.content_type,
        &spec.object_key,
        size_bytes,
        &content_hash,
    )
    .await
    {
        Ok(id) => Ok(id),
        Err(e) => {
            tracing::error!(error = %e, "Failed to insert media asset");
            Err(e)
        }
    }
}

/// Avatar variant: uploads and inserts the asset row, logging (not returning) errors.
pub async fn store_media_asset_unchecked_avatar(ctx: &AppCtx, spec: &AssetSpec) {
    let Ok(bytes) = ctx.fetcher.fetch(&spec.url).await else {
        return;
    };
    let content_hash = hex::encode(Sha256::digest(&bytes));
    let size_bytes = bytes.len() as i64;

    if let Err(e) = ctx
        .store
        .put(&spec.object_key, bytes, &spec.content_type)
        .await
    {
        tracing::error!(error = %e, object_key = %spec.object_key, "Failed to write avatar to S3 bucket");
    }

    if let Err(e) = db::insert_media_asset(
        &ctx.db_pool,
        &spec.content_type,
        &spec.object_key,
        size_bytes,
        &content_hash,
    )
    .await
    {
        tracing::error!(error = %e, "Failed to insert avatar media asset");
    }
}

#[tracing::instrument(skip_all, fields(message_id=msg.id.get(), attachment_id = tracing::field::Empty))]
pub async fn insert_message_attachments(ctx: &AppCtx, msg: &Message) -> color_eyre::Result<()> {
    let message_id = snowflake(msg.id.get());
    for attachment in &msg.attachments {
        tracing::Span::current().record("attachment_id", attachment.id.get());
        let spec = attachment_spec(msg, attachment);

        match db::attachment_exists_by_key(&ctx.db_pool, message_id, &spec.object_key).await {
            Ok(true) => {
                tracing::debug!("Attachment is already stored in db attachments table, skipping");
                continue;
            }
            Ok(false) => (),
            Err(e) => {
                tracing::error!(error=%e);
                continue;
            }
        }

        let asset_id = match download_and_store_media_asset(ctx, &spec).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    "Error while downloading media and uploading it to S3: {}",
                    e
                );
                continue;
            }
        };

        if let Err(e) = db::insert_message_attachment(&ctx.db_pool, message_id, asset_id).await {
            tracing::error!(error=%e);
            continue;
        }

        tracing::debug!(
            asset_id = %asset_id,
            "Stored attachment and inserted media asset and attachment row"
        );
    }

    Ok(())
}

#[tracing::instrument(skip_all, fields(message_id=msg.id.get(), sticker_id = tracing::field::Empty))]
pub async fn insert_stickers(ctx: &AppCtx, msg: &Message) -> color_eyre::Result<()> {
    let message_id = snowflake(msg.id.get());
    for sticker in &msg.sticker_items {
        tracing::Span::current().record("sticker_id", sticker.id.get());

        let Some(spec) = sticker_spec(sticker) else {
            tracing::error!(
                sticker_format_type = ?sticker.format_type,
                "Failed to determine file type of sticker, skipping"
            );
            continue;
        };

        if db::attachment_exists_by_key(&ctx.db_pool, message_id, &spec.object_key).await? {
            continue;
        }

        let asset_id = match db::find_asset_by_key(&ctx.db_pool, &spec.object_key).await? {
            Some(id) => id,
            None => match download_and_store_media_asset(ctx, &spec).await {
                Ok(u) => u,
                Err(e) => {
                    tracing::error!(error=%e);
                    continue;
                }
            },
        };

        if let Err(e) = db::insert_message_attachment(&ctx.db_pool, message_id, asset_id).await {
            tracing::error!(error=%e, "Failed to insert sticker attachment row");
            continue;
        }
    }
    Ok(())
}
