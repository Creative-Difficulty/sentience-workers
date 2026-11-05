//! Integration tests of the historical scan against a real Postgres and a fake Discord.

use crate::common::*;
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

fn msg_in(id: u64, channel: u64) -> serenity::all::Message {
    message(message_json(
        id,
        channel,
        user_json(7, "u7"),
        &format!("m{id}"),
    ))
}

/// Bug #11: the scan only walked `guild_channels`, which does not include threads.
#[tokio::test]
async fn scan_includes_active_and_archived_public_threads() {
    let d = FakeDiscord::new();
    d.0.lock().unwrap().guild_channels = vec![CATEGORY, CHANNEL];
    d.add_message(msg_in(30_000, CHANNEL));
    let active = 1_400_000_000_000_000_000u64;
    d.add_channel(thread_channel(active, CHANNEL, None));
    d.0.lock().unwrap().active_threads = vec![active];
    d.add_message(msg_in(30_001, active));
    // three archived threads: more than one page (the fake pages by 2)
    let archived: Vec<u64> = (1..=3).map(|i| active + i).collect();
    for (i, t) in archived.iter().enumerate() {
        let ts = format!("2024-02-0{}T00:00:00.000000+00:00", i + 1);
        d.add_channel(thread_channel(*t, CHANNEL, Some(&ts)));
        d.add_message(msg_in(30_010 + i as u64, *t));
    }
    d.0.lock()
        .unwrap()
        .archived_threads
        .insert(CHANNEL, archived.clone());

    let h = Harness::with(d, FakeFetcher::default()).await;
    run_historical_scan(h.ctx.clone(), GuildId::new(GUILD))
        .await
        .unwrap();

    assert_eq!(
        h.count("SELECT count(*) FROM messages WHERE message_id >= 30000")
            .await,
        5,
        "thread messages missing"
    );
    // 3 archived threads, pages of 2: exactly two page requests, and none for the category.
    let archived_calls: Vec<String> = h
        .discord
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("archived_public_threads"))
        .collect();
    assert_eq!(archived_calls.len(), 2, "{archived_calls:?}");
    assert!(
        archived_calls
            .iter()
            .all(|c| c.contains(&CHANNEL.to_string()))
    );
}

/// Media channels (type 16) have threads too; categories and voice channels do not.
#[tokio::test]
async fn scan_lists_archived_threads_only_for_thread_capable_channels() {
    let d = FakeDiscord::new();
    let media = 1_500_000_000_000_000_000u64;
    let voice = media + 1;
    d.add_channel(guild_channel(media, 16, "media", Some(CATEGORY)));
    d.add_channel(guild_channel(voice, 2, "voice", Some(CATEGORY)));
    d.0.lock().unwrap().guild_channels = vec![CATEGORY, media, voice];
    let thread = media + 2;
    d.add_channel(thread_channel(
        thread,
        media,
        Some("2024-02-01T00:00:00.000000+00:00"),
    ));
    d.0.lock()
        .unwrap()
        .archived_threads
        .insert(media, vec![thread]);
    d.add_message(msg_in(31_000, thread));

    let h = Harness::with(d, FakeFetcher::default()).await;
    run_historical_scan(h.ctx.clone(), GuildId::new(GUILD))
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM messages WHERE message_id = 31000")
            .await,
        1
    );
    let listed: Vec<String> = h
        .discord
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("archived_public_threads"))
        .collect();
    assert_eq!(
        listed,
        vec![format!("archived_public_threads {media} None")]
    );
}

/// The API may include the `after` message itself; it must not be processed twice and the
/// cursor must still advance.
#[tokio::test]
async fn scan_pages_strictly_after_cursor() {
    let d = FakeDiscord::new();
    d.0.lock().unwrap().guild_channels = vec![CHANNEL];
    for id in 1..=150u64 {
        d.add_message(msg_in(50_000 + id, CHANNEL));
    }
    d.0.lock().unwrap().inclusive_after = true;
    let h = Harness::with(d, FakeFetcher::default()).await;
    run_historical_scan(h.ctx.clone(), GuildId::new(GUILD))
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM messages WHERE message_id > 50000")
            .await,
        150
    );
    let pages = h
        .discord
        .calls()
        .iter()
        .filter(|c| c.starts_with("messages_after"))
        .count();
    assert_eq!(pages, 3, "pages of 100, 50(+1 overlap), then empty");
}

/// Bug #11: every message of a channel was buffered before anything was written, so a crash
/// (or OOM on a large channel) lost all progress.
#[tokio::test]
async fn scan_persists_each_page_before_fetching_more() {
    let d = FakeDiscord::new();
    d.0.lock().unwrap().guild_channels = vec![CHANNEL];
    for id in 1..=250u64 {
        d.add_message(msg_in(40_000 + id, CHANNEL));
    }
    d.0.lock().unwrap().panic_after_pages = Some(2);
    let h = Harness::with(d, FakeFetcher::default()).await;
    let ctx = h.ctx.clone();
    let res =
        tokio::spawn(async move { run_historical_scan(ctx, GuildId::new(GUILD)).await }).await;
    assert!(res.is_err(), "scan should have hit the simulated crash");
    assert!(
        h.count("SELECT count(*) FROM messages WHERE message_id > 40000")
            .await
            >= 100,
        "no page was persisted before the crash"
    );
}
