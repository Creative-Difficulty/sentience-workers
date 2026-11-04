//! Integration tests of the handler orchestration against a real Postgres, with fake Discord,
//! fake media downloads and an in-memory object store.
mod common;

use common::*;
use ingest_vestibule_retriever::handlers;
use serde_json::json;
use serenity::all::Reaction;

fn reaction(msg: u64, user: u64, emoji: serde_json::Value) -> Reaction {
    serde_json::from_value(json!({
        "user_id": user.to_string(),
        "channel_id": CHANNEL.to_string(),
        "message_id": msg.to_string(),
        "guild_id": GUILD.to_string(),
        "emoji": emoji,
        "burst": false,
        "type": 0,
    }))
    .expect("reaction fixture")
}

#[tokio::test]
async fn ensure_message_inserts_channel_tree_user_and_message() {
    let h = Harness::new().await;
    let m = simple_message(500, 7, "hello world");
    handlers::ensure_message(&h.ctx, &m).await.unwrap();

    assert_eq!(h.count("SELECT count(*) FROM discord_channels").await, 2);
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM discord_channels WHERE channel_id = {CHANNEL} AND parent_channel_id = {CATEGORY}"
        ))
        .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM discord_accounts WHERE discord_user_id = 7")
            .await,
        1
    );
    let content: String = sqlx::query_scalar("SELECT content FROM messages WHERE message_id = 500")
        .fetch_one(h.pool())
        .await
        .unwrap();
    assert_eq!(content, "hello world");

    // idempotent
    handlers::ensure_message(&h.ctx, &m).await.unwrap();
    handlers::ensure_message(&h.uncached_ctx(), &m)
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM messages WHERE message_id = 500")
            .await,
        1
    );
}

#[tokio::test]
async fn reply_parent_is_fetched_and_inserted_first() {
    let h = Harness::new().await;
    h.discord.add_message(simple_message(400, 8, "parent"));
    let mut v = message_json(401, CHANNEL, user_json(7, "u7"), "child");
    v["type"] = json!(19);
    v["message_reference"] = json!({"message_id": "400", "channel_id": CHANNEL.to_string()});
    handlers::ensure_message(&h.ctx, &message(v)).await.unwrap();

    let parent: (String, i64) =
        sqlx::query_as("SELECT content, sent_by FROM messages WHERE message_id = 400")
            .fetch_one(h.pool())
            .await
            .unwrap();
    assert_eq!(parent, ("parent".to_string(), 8));
    assert_eq!(
        h.count("SELECT count(*) FROM messages WHERE message_id = 401 AND in_reply_to = 400")
            .await,
        1
    );
}

#[tokio::test]
async fn missing_reply_parent_gets_placeholder() {
    let h = Harness::new().await;
    let mut v = message_json(401, CHANNEL, user_json(7, "u7"), "child");
    v["message_reference"] = json!({"message_id": "399", "channel_id": CHANNEL.to_string()});
    handlers::ensure_message(&h.ctx, &message(v)).await.unwrap();
    let (content, sent_by): (String, i64) =
        sqlx::query_as("SELECT content, sent_by FROM messages WHERE message_id = 399")
            .fetch_one(h.pool())
            .await
            .unwrap();
    assert_eq!(content, "[deleted message]");
    assert_eq!(sent_by, 0);
}

#[tokio::test]
async fn attachments_are_downloaded_stored_and_linked() {
    let h = Harness::new().await;
    let mut v = message_json(600, CHANNEL, user_json(7, "u7"), "pic");
    v["attachments"] = json!([{
        "id": "61", "filename": "a.png", "size": 3,
        "url": "https://cdn.test/a.png", "proxy_url": "https://cdn.test/a.png",
        "content_type": "image/png"
    }]);
    handlers::ensure_message(&h.ctx, &message(v)).await.unwrap();

    assert!(
        h.store
            .objects
            .lock()
            .unwrap()
            .contains_key("discord/attachments/600/61")
    );
    assert_eq!(
        h.count("SELECT count(*) FROM message_attachments ma JOIN media_assets a ON a.id = ma.asset_id WHERE ma.message_id = 600 AND a.object_key = 'discord/attachments/600/61' AND a.content_type = 'image/png'").await,
        1
    );
}

#[tokio::test]
async fn reaction_add_and_remove_roundtrip() {
    let h = Harness::new().await;
    h.discord.add_message(simple_message(700, 7, "react to me"));
    // reactor must exist as account (FK)
    handlers::ensure_user(&h.ctx, &user(8, "u8")).await.unwrap();

    let r = reaction(700, 8, json!({"id": null, "name": "👍"}));
    handlers::handle_reaction_add(&h.ctx, &r).await.unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM message_reactions WHERE message_id = 700 AND user_id = 8")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM discord_emojis WHERE discord_emoji_id = '👍'")
            .await,
        1
    );

    handlers::handle_reaction_remove(&h.ctx, &r).await.unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM message_reactions WHERE message_id = 700")
            .await,
        0
    );
}

#[tokio::test]
async fn custom_emoji_reaction_stores_emoji_asset() {
    let h = Harness::new().await;
    h.discord.add_message(simple_message(700, 7, "react to me"));
    handlers::ensure_user(&h.ctx, &user(8, "u8")).await.unwrap();
    let r = reaction(
        700,
        8,
        json!({"id": "555", "name": "pog", "animated": false}),
    );
    handlers::handle_reaction_add(&h.ctx, &r).await.unwrap();
    assert!(
        h.store
            .objects
            .lock()
            .unwrap()
            .contains_key("discord/emojis/555.webp")
    );
    assert_eq!(
        h.count("SELECT count(*) FROM discord_emojis e JOIN media_assets a ON a.id = e.asset_id WHERE e.discord_emoji_id = '555'").await,
        1
    );
}
