//! Reaction ingestion (bug #8 + reactor FK).
use crate::common::*;
use ingest_vestibule_retriever::handlers;
use serde_json::{Value, json};
use serenity::all::Reaction;

pub fn reaction(msg: u64, user: u64, emoji: Value) -> Reaction {
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

async fn setup(users: &[u64]) -> Harness {
    let h = Harness::new().await;
    h.discord.add_message(simple_message(700, 7, "react to me"));
    for u in users {
        handlers::ensure_user(&h.ctx, &user(*u, &format!("u{u}")))
            .await
            .unwrap();
    }
    h
}

#[tokio::test]
async fn duplicate_reaction_add_is_idempotent() {
    let h = setup(&[8]).await;
    let r = reaction(700, 8, json!({"id": null, "name": "👍"}));
    handlers::handle_reaction_add(&h.ctx, &r).await.unwrap();
    // gateway replay after reconnect / historical scan + live event
    handlers::handle_reaction_add(&h.ctx, &r)
        .await
        .expect("second identical reaction_add must not fail");
    assert_eq!(
        h.count("SELECT count(*) FROM message_reactions WHERE message_id = 700")
            .await,
        1
    );
}

#[tokio::test]
async fn concurrent_new_emoji_is_inserted_once() {
    let users: Vec<u64> = (100..116).collect();
    let h = setup(&users).await;
    let tasks: Vec<_> = users
        .iter()
        .map(|u| {
            let ctx = h.ctx.clone();
            let r = reaction(700, *u, json!({"id": null, "name": "🎉"}));
            tokio::spawn(async move { handlers::handle_reaction_add(&ctx, &r).await })
        })
        .collect();
    for t in tasks {
        t.await.unwrap().unwrap();
    }
    assert_eq!(
        h.count("SELECT count(*) FROM discord_emojis WHERE discord_emoji_id = '🎉'")
            .await,
        1,
        "duplicate discord_emojis rows"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM message_reactions WHERE message_id = 700")
            .await,
        16
    );
}

#[tokio::test]
async fn reaction_from_user_without_messages_is_stored() {
    // user 9 never posted, so is not in discord_accounts (FK message_reactions.user_id)
    let h = setup(&[]).await;
    let mut v = serde_json::to_value(reaction(700, 9, json!({"id": null, "name": "👀"}))).unwrap();
    v["member"] = json!({
        "user": user_json(9, "lurker"),
        "roles": [], "joined_at": "2024-01-01T00:00:00.000000+00:00",
        "deaf": false, "mute": false, "flags": 0
    });
    let r: Reaction = serde_json::from_value(v).unwrap();
    handlers::handle_reaction_add(&h.ctx, &r)
        .await
        .expect("reaction of a user that never posted");
    assert_eq!(
        h.count("SELECT count(*) FROM message_reactions WHERE user_id = 9")
            .await,
        1
    );
}

#[tokio::test]
async fn reaction_on_known_message_does_not_refetch_it() {
    let h = setup(&[8]).await;
    handlers::ensure_message(&h.ctx, &simple_message(700, 7, "react to me"))
        .await
        .unwrap();
    let r = reaction(700, 8, json!({"id": null, "name": "👍"}));
    handlers::handle_reaction_add(&h.ctx, &r).await.unwrap();
    assert!(
        !h.discord
            .calls()
            .iter()
            .any(|c| c.starts_with("get_message")),
        "{:?}",
        h.discord.calls()
    );
}

#[tokio::test]
async fn reactor_without_member_is_fetched() {
    let h = setup(&[]).await;
    let r = reaction(700, 11, json!({"id": null, "name": "👍"}));
    handlers::handle_reaction_add(&h.ctx, &r).await.unwrap();
    assert!(h.discord.calls().contains(&"get_user 11".to_string()));
    assert_eq!(
        h.count("SELECT count(*) FROM message_reactions WHERE user_id = 11")
            .await,
        1
    );
}
