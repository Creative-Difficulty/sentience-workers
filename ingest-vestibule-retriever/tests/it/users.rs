//! ensure_user correctness (bug #7).

use crate::common::*;
use ingest_vestibule_retriever::handlers;

#[tokio::test]
async fn concurrent_ensure_user_creates_exactly_one_user() {
    let h = Harness::new().await;
    let base_vu = h.count("SELECT count(*) FROM vestibule_users").await;
    let u = user(7, "u7");
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let ctx = h.ctx.clone();
            let u = u.clone();
            tokio::spawn(async move { handlers::ensure_user(&ctx, &u).await })
        })
        .collect();
    for t in tasks {
        t.await.unwrap().unwrap();
    }
    assert_eq!(
        h.count("SELECT count(*) FROM discord_accounts WHERE discord_user_id = 7")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM vestibule_users").await - base_vu,
        1,
        "orphan vestibule_users rows created"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM vestibule_users v WHERE NOT EXISTS (SELECT 1 FROM discord_accounts d WHERE d.vestibule_user_id = v.id)").await,
        0
    );
}

#[tokio::test]
async fn ensure_user_across_processes_does_not_duplicate() {
    let h = Harness::new().await;
    let base_vu = h.count("SELECT count(*) FROM vestibule_users").await;
    handlers::ensure_user(&h.ctx, &user(7, "u7")).await.unwrap();
    // a second process / restarted process with a cold cache
    handlers::ensure_user(&h.uncached_ctx(), &user(7, "u7"))
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM vestibule_users").await - base_vu,
        1
    );
}

/// Discord usernames are unique only at a point in time: when user 1 renamed away from "reused"
/// and user 2 took it, the stale row of user 1 must not block user 2 forever.
#[tokio::test]
async fn recycled_username_does_not_block_new_user() {
    let h = Harness::new().await;
    handlers::ensure_user(&h.ctx, &user(1, "reused"))
        .await
        .unwrap();

    let ctx = h.uncached_ctx();
    handlers::ensure_user(&ctx, &user(2, "reused"))
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM discord_accounts WHERE discord_user_id = 2")
            .await,
        1,
        "new user with a recycled username was not inserted"
    );
    // and their messages can be stored (FK messages.sent_by -> discord_accounts)
    handlers::ensure_message(&ctx, &simple_message(1000, 2, "hi"))
        .await
        .expect("message of user with recycled username");
}

#[tokio::test]
async fn failed_ensure_user_is_not_cached() {
    let h = Harness::new().await;
    h.ctx.db_pool.close().await;
    assert!(handlers::ensure_user(&h.ctx, &user(3, "u3")).await.is_err());
    assert!(!h.ctx.user_id_cache.read().unwrap().contains(&3));
}

/// Schema-dependent: with `UNIQUE (username)` (unidb main) the stale holder is renamed; without
/// it (unidb-schema PR "schema-integrity") both rows keep their username and nothing is renamed.
#[tokio::test]
async fn stale_username_holder_is_renamed_only_when_unique() {
    let h = Harness::new().await;
    let unique = h
        .count("SELECT count(*) FROM pg_constraint WHERE conrelid = 'discord_accounts'::regclass AND contype = 'u' AND pg_get_constraintdef(oid) = 'UNIQUE (username)'")
        .await
        == 1;
    handlers::ensure_user(&h.ctx, &user(1, "reused"))
        .await
        .unwrap();
    handlers::ensure_user(&h.uncached_ctx(), &user(2, "reused"))
        .await
        .unwrap();
    let names: Vec<(i64, String)> =
        sqlx::query_as("SELECT discord_user_id, username FROM discord_accounts WHERE discord_user_id IN (1, 2) ORDER BY 1")
            .fetch_all(h.pool())
            .await
            .unwrap();
    let stale = if unique { "reused#stale-1" } else { "reused" };
    assert_eq!(
        names,
        vec![(1, stale.to_string()), (2, "reused".to_string())]
    );
    assert_eq!(
        ingest_vestibule_retriever::db::stale_username("reused", 1),
        "reused#stale-1"
    );
}
