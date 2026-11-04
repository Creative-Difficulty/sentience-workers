//! Message edit history (bug #6).
mod common;

use common::*;
use ingest_vestibule_retriever::handlers;
use serde_json::json;

const T0: &str = "2024-01-02T03:04:05.000000+00:00";
const T1: &str = "2024-01-02T04:00:00.000000+00:00";
const T2: &str = "2024-01-02T05:00:00.000000+00:00";

fn msg_at(id: u64, content: &str, edited: Option<&str>) -> serenity::all::Message {
    let mut v = message_json(id, CHANNEL, user_json(7, "u7"), content);
    v["timestamp"] = json!(T0);
    v["edited_timestamp"] = json!(edited);
    message(v)
}

async fn edits(h: &Harness, id: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT old_content FROM message_edits WHERE message_id = $1 ORDER BY edited_at",
    )
    .bind(id)
    .fetch_all(h.pool())
    .await
    .unwrap()
}

async fn content(h: &Harness, id: i64) -> String {
    sqlx::query_scalar("SELECT content FROM messages WHERE message_id = $1")
        .bind(id)
        .fetch_one(h.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn edit_records_previous_content() {
    let h = Harness::new().await;
    handlers::ensure_message(&h.ctx, &msg_at(900, "old", None))
        .await
        .unwrap();
    handlers::handle_message_edit(&h.ctx, &msg_at(900, "new", Some(T1)))
        .await
        .unwrap();
    assert_eq!(edits(&h, 900).await, vec!["old"]);
    assert_eq!(content(&h, 900).await, "new");
}

#[tokio::test]
async fn edit_of_unknown_message_does_not_record_new_content_as_old() {
    let h = Harness::new().await;
    handlers::handle_message_edit(&h.ctx, &msg_at(901, "new", Some(T1)))
        .await
        .unwrap();
    assert_eq!(content(&h, 901).await, "new");
    assert_eq!(
        edits(&h, 901).await,
        Vec::<String>::new(),
        "edit history must not contain the new content as the old one"
    );
}

#[tokio::test]
async fn embed_only_update_records_no_edit() {
    let h = Harness::new().await;
    handlers::ensure_message(&h.ctx, &msg_at(902, "old", None))
        .await
        .unwrap();
    handlers::handle_message_edit(&h.ctx, &msg_at(902, "new", Some(T1)))
        .await
        .unwrap();
    // Discord re-sends MESSAGE_UPDATE when embeds resolve: same content, same edited_timestamp.
    let mut v = message_json(902, CHANNEL, user_json(7, "u7"), "new");
    v["edited_timestamp"] = json!(T1);
    v["embeds"] = json!([{"type": "link", "url": "https://example.com"}]);
    handlers::handle_message_edit(&h.ctx, &message(v))
        .await
        .unwrap();
    assert_eq!(
        edits(&h, 902).await,
        vec!["old"],
        "embed-only update recorded as edit"
    );
}

#[tokio::test]
async fn out_of_order_edits_keep_latest_content() {
    let h = Harness::new().await;
    handlers::ensure_message(&h.ctx, &msg_at(903, "A", None))
        .await
        .unwrap();
    // A -> B at T1, B -> C at T2, but the T2 event is processed first.
    handlers::handle_message_edit(&h.ctx, &msg_at(903, "C", Some(T2)))
        .await
        .unwrap();
    handlers::handle_message_edit(&h.ctx, &msg_at(903, "B", Some(T1)))
        .await
        .unwrap();
    assert_eq!(
        content(&h, 903).await,
        "C",
        "stale edit overwrote newer content"
    );
    assert_eq!(edits(&h, 903).await, vec!["A"]);
}

#[tokio::test]
async fn system_message_update_keeps_synthetic_content() {
    let h = Harness::new().await;
    let mut v = message_json(904, CHANNEL, user_json(7, "u7"), "");
    v["type"] = json!(7);
    handlers::ensure_message(&h.ctx, &message(v.clone()))
        .await
        .unwrap();
    v["edited_timestamp"] = json!(T1);
    handlers::handle_message_edit(&h.ctx, &message(v))
        .await
        .unwrap();
    assert_eq!(content(&h, 904).await, "user u7 joined the server");
    assert_eq!(edits(&h, 904).await, Vec::<String>::new());
}

fn update_event(v: serde_json::Value) -> serenity::all::MessageUpdateEvent {
    serde_json::from_value(v).expect("update fixture")
}

/// Premise of the `message_update` fix: serenity only hands the full new message to the handler
/// when it is in the cache, and the default cache stores no messages.
#[test]
fn serenity_default_cache_keeps_no_messages() {
    assert_eq!(serenity::cache::Settings::default().max_messages, 0);
}

#[tokio::test]
async fn update_event_records_edit_without_full_message() {
    let h = Harness::new().await;
    handlers::ensure_message(&h.ctx, &msg_at(910, "old", None))
        .await
        .unwrap();
    let ev = update_event(json!({
        "id": "910", "channel_id": CHANNEL.to_string(), "guild_id": GUILD.to_string(),
        "content": "new", "edited_timestamp": T1, "type": 0
    }));
    let out = handlers::handle_message_update(&h.ctx, &ev).await.unwrap();
    assert_eq!(out, ingest_vestibule_retriever::db::EditOutcome::Recorded);
    assert_eq!(edits(&h, 910).await, vec!["old"]);
    assert_eq!(content(&h, 910).await, "new");
    // replay of the same event is a no-op
    let out = handlers::handle_message_update(&h.ctx, &ev).await.unwrap();
    assert_eq!(out, ingest_vestibule_retriever::db::EditOutcome::Unchanged);
    assert_eq!(edits(&h, 910).await, vec!["old"]);
}

#[tokio::test]
async fn update_event_for_unknown_message_ingests_current_version() {
    let h = Harness::new().await;
    h.discord.add_message(msg_at(911, "current", Some(T1)));
    let ev = update_event(json!({
        "id": "911", "channel_id": CHANNEL.to_string(), "guild_id": GUILD.to_string(),
        "content": "current", "edited_timestamp": T1
    }));
    handlers::handle_message_update(&h.ctx, &ev).await.unwrap();
    assert_eq!(content(&h, 911).await, "current");
    assert_eq!(edits(&h, 911).await, Vec::<String>::new());
}

#[tokio::test]
async fn update_event_without_content_is_ignored() {
    let h = Harness::new().await;
    handlers::ensure_message(&h.ctx, &msg_at(912, "old", None))
        .await
        .unwrap();
    let ev = update_event(json!({
        "id": "912", "channel_id": CHANNEL.to_string(), "guild_id": GUILD.to_string(),
        "embeds": [], "edited_timestamp": T1
    }));
    handlers::handle_message_update(&h.ctx, &ev).await.unwrap();
    assert_eq!(content(&h, 912).await, "old");
    assert!(
        h.discord
            .calls()
            .iter()
            .all(|c| !c.starts_with("get_message"))
    );
}
