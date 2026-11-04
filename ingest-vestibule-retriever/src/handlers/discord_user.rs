use crate::handlers::attachments::store_media_asset_unchecked_avatar;
use crate::mapping::{avatar_spec, user_row};
use crate::{AppCtx, cache_contains, cache_insert, db};

/// Makes sure the Discord user is in `vestibule_users` + `discord_accounts` and stores its avatar.
pub async fn ensure_user(ctx: &AppCtx, user: &serenity::all::User) -> color_eyre::Result<()> {
    let row = user_row(user);
    if cache_contains(&ctx.user_id_cache, row.discord_user_id) {
        return Ok(());
    }

    db::insert_user(&ctx.db_pool, &row).await?;
    cache_insert(&ctx.user_id_cache, row.discord_user_id);

    if let Some(spec) = avatar_spec(user) {
        // Check if we already have this avatar
        if db::find_asset_by_key(&ctx.db_pool, &spec.object_key)
            .await?
            .is_none()
        {
            store_media_asset_unchecked_avatar(ctx, &spec).await;
        }
    }

    Ok(())
}
