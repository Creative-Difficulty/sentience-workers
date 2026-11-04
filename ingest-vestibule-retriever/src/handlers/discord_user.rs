use crate::db::EnsureUser;
use crate::handlers::attachments::store_media_asset;
use crate::mapping::{avatar_spec, user_row};
use crate::{AppCtx, cache_contains, cache_insert, db};

/// Makes sure the Discord user is in `vestibule_users` + `discord_accounts` and stores the
/// avatar of newly created users. The cache is only updated once the rows are committed.
pub async fn ensure_user(ctx: &AppCtx, user: &serenity::all::User) -> color_eyre::Result<()> {
    let row = user_row(user);
    if cache_contains(&ctx.user_id_cache, row.discord_user_id) {
        return Ok(());
    }

    let outcome = db::ensure_user_rows(&ctx.db_pool, &row).await?;
    cache_insert(&ctx.user_id_cache, row.discord_user_id);

    if outcome == EnsureUser::Created
        && let Some(spec) = avatar_spec(user)
        && let Err(e) = store_media_asset(ctx, &spec).await
    {
        // The account is stored; a missing avatar is not worth failing the message for.
        tracing::warn!(error = %e, "Failed to store avatar");
    }

    Ok(())
}
