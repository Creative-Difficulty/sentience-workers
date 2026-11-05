use crate::mapping::{AssetSpec, attachment_spec, snowflake, sticker_spec};
use crate::{AppCtx, db};
use serenity::all::Message;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Makes sure the media described by `spec` is stored and returns its `media_assets.id`.
///
/// 1. An asset already stored under `spec.object_key` is reused without downloading.
/// 2. The download is hashed; identical content already stored (any key) is reused without
///    uploading, so identical avatars/attachments do not collide on the UNIQUE `content_hash`.
/// 3. Otherwise the bytes are uploaded and only then the row is inserted. An upload failure
///    returns an error and writes no row, so `media_assets` never points at a missing object.
///    Concurrent inserts of the same content resolve to the winner's row.
pub async fn store_media_asset(ctx: &AppCtx, spec: &AssetSpec) -> color_eyre::Result<Uuid> {
    if let Some(existing) = db::find_asset_by_key(&ctx.db_pool, &spec.object_key).await? {
        return Ok(existing);
    }

    let bytes = ctx.fetcher.fetch(&spec.url).await.map_err(|e| {
        tracing::error!(error = %e, url = %spec.url, "Failed to download media");
        e
    })?;

    let content_hash = hex::encode(Sha256::digest(&bytes));
    if let Some(existing) = db::find_asset_by_hash(&ctx.db_pool, &content_hash).await? {
        return Ok(existing);
    }

    let size_bytes = bytes.len() as i64;
    ctx.store
        .put(&spec.object_key, bytes, &spec.content_type)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, object_key = %spec.object_key, "Failed to upload media");
            e
        })?;

    db::insert_media_asset(
        &ctx.db_pool,
        &spec.content_type,
        &spec.object_key,
        size_bytes,
        &content_hash,
    )
    .await
}

#[tracing::instrument(skip_all, fields(message_id=msg.id.get(), attachment_id = tracing::field::Empty))]
pub async fn insert_message_attachments(ctx: &AppCtx, msg: &Message) -> color_eyre::Result<()> {
    let message_id = snowflake(msg.id.get());
    for attachment in &msg.attachments {
        tracing::Span::current().record("attachment_id", attachment.id.get());
        let spec = attachment_spec(msg, attachment);

        let asset_id = match store_media_asset(ctx, &spec).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    "Error while downloading media and uploading it to S3: {}",
                    e
                );
                continue;
            }
        };

        match db::insert_message_attachment(&ctx.db_pool, message_id, asset_id).await {
            Ok(true) => tracing::debug!(%asset_id, "Stored attachment"),
            Ok(false) => tracing::debug!(%asset_id, "Attachment already linked"),
            Err(e) => tracing::error!(error=%e, "Failed to insert attachment row"),
        }
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

        let asset_id = match store_media_asset(ctx, &spec).await {
            Ok(u) => u,
            Err(e) => {
                tracing::error!(error=%e, "Failed to store sticker");
                continue;
            }
        };

        if let Err(e) = db::insert_message_attachment(&ctx.db_pool, message_id, asset_id).await {
            tracing::error!(error=%e, "Failed to insert sticker attachment row");
        }
    }
    Ok(())
}
