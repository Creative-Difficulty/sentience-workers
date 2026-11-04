//! Integration tests of the historical scan against a real Postgres and a fake Discord.
mod common;

use common::*;
use ingest_vestibule_retriever::historical::run_historical_scan;
use serde_json::json;
use serenity::all::GuildId;

#[tokio::test]
async fn scan_inserts_all_channel_messages_and_reactions() {
    let d = FakeDiscord::new();
    d.0.lock().unwrap().guild_channels = vec![CATEGORY, CHANNEL];
    for id in 1..=250u64 {
        d.add_message(simple_message(10_000 + id, 7, &format!("m{id}")));
    }
    let mut v = message_json(20_000, CHANNEL, user_json(7, "u7"), "with reaction");
    v["reactions"] = json!([{"count": 1, "count_details": {"burst": 0, "normal": 1}, "me": false, "me_burst": false, "burst_colors": [], "emoji": {"id": null, "name": "🔥"}}]);
    d.add_message(message(v));
    d.0.lock()
        .unwrap()
        .reaction_users
        .insert((20_000, "🔥".into()), vec![user(9, "u9")]);

    let h = Harness::with(d, FakeFetcher::default()).await;
    run_historical_scan(h.ctx.clone(), GuildId::new(GUILD))
        .await
        .unwrap();

    assert_eq!(h.count("SELECT count(*) FROM messages").await, 251);
    assert_eq!(
        h.count("SELECT count(*) FROM message_reactions WHERE message_id = 20000 AND user_id = 9")
            .await,
        1
    );
}
