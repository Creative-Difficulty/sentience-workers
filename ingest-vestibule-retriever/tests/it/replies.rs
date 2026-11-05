//! Reply parents and channel tree.
use crate::common::*;
use ingest_vestibule_retriever::handlers;
use serde_json::json;

const THREAD: u64 = 1_200_000_000_000_000_001;

async fn reply_to(h: &Harness, id: i64) -> Option<i64> {
    sqlx::query_scalar("SELECT in_reply_to FROM messages WHERE message_id = $1")
        .bind(id)
        .fetch_one(h.pool())
        .await
        .unwrap()
}

/// A thread's starter message references the parent message in the *parent* channel.
#[tokio::test]
async fn reply_parent_is_fetched_from_the_referenced_channel() {
    let h = Harness::new().await;
    h.discord
        .add_channel(guild_channel(THREAD, 11, "thread", Some(CHANNEL)));
    h.discord
        .add_message(simple_message(5000, 8, "the original post"));
    let mut v = message_json(5001, THREAD, user_json(7, "u7"), "");
    v["type"] = json!(21);
    v["message_reference"] = json!({
        "message_id": "5000", "channel_id": CHANNEL.to_string(), "guild_id": GUILD.to_string()
    });
    handlers::ensure_message(&h.ctx, &message(v)).await.unwrap();

    let (content, channel): (String, i64) =
        sqlx::query_as("SELECT content, channel_id FROM messages WHERE message_id = 5000")
            .fetch_one(h.pool())
            .await
            .unwrap();
    assert_eq!(
        content, "the original post",
        "parent replaced by a placeholder"
    );
    assert_eq!(channel, CHANNEL as i64);
    assert_eq!(reply_to(&h, 5001).await, Some(5000));
}

#[tokio::test]
async fn placeholder_parent_lives_in_the_referenced_channel() {
    let h = Harness::new().await;
    h.discord
        .add_channel(guild_channel(THREAD, 11, "thread", Some(CHANNEL)));
    let mut v = message_json(5011, THREAD, user_json(7, "u7"), "");
    v["type"] = json!(21);
    v["message_reference"] = json!({"message_id": "5010", "channel_id": CHANNEL.to_string()});
    handlers::ensure_message(&h.ctx, &message(v)).await.unwrap();
    let channel: i64 =
        sqlx::query_scalar("SELECT channel_id FROM messages WHERE message_id = 5010")
            .fetch_one(h.pool())
            .await
            .unwrap();
    assert_eq!(channel, CHANNEL as i64);
}

/// Forwards (reference type 1) and cross-guild references (crossposts) are not replies.
#[tokio::test]
async fn forwards_and_cross_guild_references_are_not_replies() {
    let h = Harness::new().await;
    let mut fwd = message_json(5021, CHANNEL, user_json(7, "u7"), "");
    fwd["message_reference"] = json!({
        "type": 1, "message_id": "77", "channel_id": "88", "guild_id": "99"
    });
    handlers::ensure_message(&h.ctx, &message(fwd))
        .await
        .unwrap();
    let mut xpost = message_json(5022, CHANNEL, user_json(7, "u7"), "news");
    xpost["message_reference"] = json!({"message_id": "78", "channel_id": "89", "guild_id": "99"});
    handlers::ensure_message(&h.ctx, &message(xpost))
        .await
        .unwrap();

    assert_eq!(reply_to(&h, 5021).await, None);
    assert_eq!(reply_to(&h, 5022).await, None);
    assert_eq!(
        h.count("SELECT count(*) FROM messages WHERE content = '[deleted message]'")
            .await,
        0,
        "bogus placeholders for messages of other guilds"
    );
}

/// A reply whose parent channel we cannot access keeps the message, without the link.
#[tokio::test]
async fn reply_into_inaccessible_channel_keeps_message() {
    let h = Harness::new().await;
    let mut v = message_json(5031, CHANNEL, user_json(7, "u7"), "child");
    v["message_reference"] = json!({"message_id": "5030", "channel_id": "424242"});
    handlers::ensure_message(&h.ctx, &message(v)).await.unwrap();
    assert_eq!(reply_to(&h, 5031).await, None);
}

/// If a parent channel cannot be fetched (e.g. a category the bot cannot see), the channel is
/// stored without a parent instead of failing its FK and being cached as stored.
#[tokio::test]
async fn channel_with_unfetchable_parent_is_still_stored() {
    let h = Harness::new().await;
    let hidden_parent = 1_300_000_000_000_000_000u64;
    let ch = 1_300_000_000_000_000_001u64;
    h.discord
        .add_channel(guild_channel(ch, 0, "orphan", Some(hidden_parent)));
    let m = message(message_json(5040, ch, user_json(7, "u7"), "hi"));
    handlers::ensure_message(&h.ctx, &m)
        .await
        .expect("message in a channel whose parent is not visible");
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM discord_channels WHERE channel_id = {ch} AND parent_channel_id IS NULL")).await,
        1
    );
}
