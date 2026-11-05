//! Shared test harness: per-test Postgres databases cloned from the schema-loaded template DB,
//! a fixture-backed fake Discord API, an in-memory object store and a fake media fetcher.
//!
//! Requires `DATABASE_URL` (the same DB used for sqlx compile-time checks, loaded with the
//! unidb schema). Every test gets its own `CREATE DATABASE ... TEMPLATE <that db>` copy.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use ingest_vestibule_retriever::AppCtx;
use ingest_vestibule_retriever::discord_api::{DiscordApi, ThreadPage};
use ingest_vestibule_retriever::media::{MediaFetcher, ObjectStore};
use serde_json::{Value, json};
use serenity::all::{
    Channel, ChannelId, GuildChannel, GuildId, Message, MessageId, ReactionType, User, UserId,
};
use serenity::async_trait;
use sqlx::{Connection, PgConnection, PgPool};

static CREATE_DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static SWEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Drops test databases left behind by test processes that are no longer running (per-test DBs
/// are not dropped at the end of a test because pools may still be in use by spawned tasks).
async fn sweep_stale_test_dbs(admin: &mut PgConnection) {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT datname FROM pg_database WHERE datname LIKE 'ivr\\_test\\_%'")
            .fetch_all(&mut *admin)
            .await
            .unwrap_or_default();
    for name in names {
        let owner_alive = name
            .strip_prefix("ivr_test_")
            .and_then(|rest| rest.split_once('_'))
            .and_then(|(pid, _)| pid.parse::<u32>().ok())
            .is_some_and(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists());
        if !owner_alive {
            let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
                .execute(&mut *admin)
                .await;
        }
    }
}

/// Creates a fresh database from the template and returns a pool connected to it.
pub async fn fresh_db() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must point at a unidb DB");
    let (base, template) = url.rsplit_once('/').expect("DATABASE_URL has no db name");
    let template = template.split('?').next().unwrap().to_string();
    let name = format!(
        "ivr_test_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );

    {
        let _guard = CREATE_DB_LOCK.lock().await;
        let mut admin = PgConnection::connect(&format!("{base}/postgres"))
            .await
            .expect("connect admin");
        if !SWEPT.swap(true, std::sync::atomic::Ordering::SeqCst) {
            sweep_stale_test_dbs(&mut admin).await;
        }
        let mut attempt = 0;
        loop {
            // Other test binaries may be cloning the template at the same time.
            match sqlx::query(&format!(
                "CREATE DATABASE \"{name}\" TEMPLATE \"{template}\""
            ))
            .execute(&mut admin)
            .await
            {
                Ok(_) => break,
                Err(e) if attempt < 50 => {
                    attempt += 1;
                    let _ = e;
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Err(e) => panic!("create test db: {e}"),
            }
        }
    }

    sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(&format!("{base}/{name}"))
        .await
        .expect("connect test db")
}

// ---------------------------------------------------------------------------------------------
// JSON fixtures
// ---------------------------------------------------------------------------------------------

pub const GUILD: u64 = 1_000_000_000_000_000_001;
pub const CHANNEL: u64 = 1_100_000_000_000_000_001;
pub const CATEGORY: u64 = 1_100_000_000_000_000_000;

pub fn user_json(id: u64, username: &str) -> Value {
    json!({
        "id": id.to_string(),
        "username": username,
        "discriminator": "0",
        "global_name": format!("{username} display"),
        "avatar": null,
        "bot": false,
    })
}

pub fn user(id: u64, username: &str) -> User {
    serde_json::from_value(user_json(id, username)).expect("user fixture")
}

pub fn message_json(id: u64, channel: u64, author: Value, content: &str) -> Value {
    json!({
        "id": id.to_string(),
        "channel_id": channel.to_string(),
        "guild_id": GUILD.to_string(),
        "author": author,
        "content": content,
        "timestamp": "2024-01-02T03:04:05.000000+00:00",
        "edited_timestamp": null,
        "tts": false,
        "mention_everyone": false,
        "mentions": [],
        "mention_roles": [],
        "attachments": [],
        "embeds": [],
        "pinned": false,
        "type": 0,
    })
}

pub fn message(v: Value) -> Message {
    serde_json::from_value(v).expect("message fixture")
}

pub fn simple_message(id: u64, author: u64, content: &str) -> Message {
    message(message_json(
        id,
        CHANNEL,
        user_json(author, &format!("user{author}")),
        content,
    ))
}

pub fn channel_json(id: u64, kind: u8, name: &str, parent: Option<u64>) -> Value {
    json!({
        "id": id.to_string(),
        "type": kind,
        "guild_id": GUILD.to_string(),
        "name": name,
        "parent_id": parent.map(|p| p.to_string()),
        "position": 0,
        "permission_overwrites": [],
        "nsfw": false,
    })
}

/// A thread (type 11) of `parent`; archived threads get an `archive_timestamp`.
pub fn thread_channel(id: u64, parent: u64, archived_at: Option<&str>) -> GuildChannel {
    let mut v = channel_json(id, 11, &format!("thread{id}"), Some(parent));
    v["thread_metadata"] = json!({
        "archived": archived_at.is_some(),
        "archive_timestamp": archived_at.unwrap_or("2024-01-01T00:00:00.000000+00:00"),
        "auto_archive_duration": 60,
        "locked": false,
    });
    serde_json::from_value(v).expect("thread fixture")
}

pub fn guild_channel(id: u64, kind: u8, name: &str, parent: Option<u64>) -> GuildChannel {
    serde_json::from_value(channel_json(id, kind, name, parent)).expect("channel fixture")
}

// ---------------------------------------------------------------------------------------------
// Fake Discord API
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct FakeDiscordState {
    pub channels: HashMap<u64, GuildChannel>,
    /// Messages per channel (any order).
    pub messages: HashMap<u64, Vec<Message>>,
    pub reaction_users: HashMap<(u64, String), Vec<User>>,
    pub guild_channels: Vec<u64>,
    pub active_threads: Vec<u64>,
    pub archived_threads: HashMap<u64, Vec<u64>>,
    /// Log of calls, e.g. "get_message 1/2".
    pub calls: Vec<String>,
    /// Simulates the process dying: the message-page fetch after this many pages panics.
    pub panic_after_pages: Option<usize>,
    pub pages_fetched: usize,
}

#[derive(Default, Clone)]
pub struct FakeDiscord(pub Arc<Mutex<FakeDiscordState>>);

impl FakeDiscord {
    pub fn new() -> Self {
        let d = Self::default();
        d.add_channel(guild_channel(CATEGORY, 4, "category", None));
        d.add_channel(guild_channel(CHANNEL, 0, "general", Some(CATEGORY)));
        d
    }
    pub fn add_channel(&self, ch: GuildChannel) {
        self.0.lock().unwrap().channels.insert(ch.id.get(), ch);
    }
    pub fn add_message(&self, m: Message) {
        self.0
            .lock()
            .unwrap()
            .messages
            .entry(m.channel_id.get())
            .or_default()
            .push(m);
    }
    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }
    fn log(&self, s: String) {
        self.0.lock().unwrap().calls.push(s);
    }
    fn count_page(&self) {
        let crash = {
            let mut st = self.0.lock().unwrap();
            st.pages_fetched += 1;
            st.panic_after_pages.is_some_and(|n| st.pages_fetched > n)
        };
        if crash {
            panic!("simulated crash during historical scan");
        }
    }
}

fn reaction_key(r: &ReactionType) -> String {
    match r {
        ReactionType::Custom { id, .. } => id.get().to_string(),
        ReactionType::Unicode(s) => s.clone(),
        _ => "?".into(),
    }
}

#[async_trait]
impl DiscordApi for FakeDiscord {
    async fn get_channel(&self, channel_id: ChannelId) -> color_eyre::Result<Channel> {
        self.log(format!("get_channel {channel_id}"));
        let st = self.0.lock().unwrap();
        st.channels
            .get(&channel_id.get())
            .cloned()
            .map(Channel::Guild)
            .ok_or_else(|| color_eyre::eyre::eyre!("403 Missing Access (fake)"))
    }

    async fn get_message(
        &self,
        channel_id: ChannelId,
        message_id: MessageId,
    ) -> color_eyre::Result<Option<Message>> {
        self.log(format!("get_message {channel_id}/{message_id}"));
        let st = self.0.lock().unwrap();
        Ok(st
            .messages
            .get(&channel_id.get())
            .and_then(|v| v.iter().find(|m| m.id == message_id).cloned()))
    }

    async fn reaction_users(
        &self,
        _channel_id: ChannelId,
        message_id: MessageId,
        reaction: &ReactionType,
        after: Option<UserId>,
    ) -> color_eyre::Result<Vec<User>> {
        let st = self.0.lock().unwrap();
        let mut users = st
            .reaction_users
            .get(&(message_id.get(), reaction_key(reaction)))
            .cloned()
            .unwrap_or_default();
        users.sort_by_key(|u| u.id);
        Ok(users
            .into_iter()
            .filter(|u| after.is_none_or(|a| u.id > a))
            .take(100)
            .collect())
    }

    async fn get_user(&self, user_id: UserId) -> color_eyre::Result<User> {
        self.log(format!("get_user {user_id}"));
        Ok(user(user_id.get(), &format!("user{user_id}")))
    }

    async fn guild_channels(&self, _guild_id: GuildId) -> color_eyre::Result<Vec<GuildChannel>> {
        let st = self.0.lock().unwrap();
        Ok(st
            .guild_channels
            .iter()
            .filter_map(|id| st.channels.get(id).cloned())
            .collect())
    }

    async fn messages_before(
        &self,
        channel_id: ChannelId,
        before: Option<MessageId>,
    ) -> color_eyre::Result<Vec<Message>> {
        self.log(format!("messages_before {channel_id} {before:?}"));
        self.count_page();
        let st = self.0.lock().unwrap();
        let mut v: Vec<Message> = st
            .messages
            .get(&channel_id.get())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| before.is_none_or(|b| m.id < b))
            .collect();
        v.sort_by_key(|m| std::cmp::Reverse(m.id));
        v.truncate(100);
        Ok(v)
    }

    async fn messages_after(
        &self,
        channel_id: ChannelId,
        after: MessageId,
    ) -> color_eyre::Result<Vec<Message>> {
        self.log(format!("messages_after {channel_id} {after}"));
        self.count_page();
        let st = self.0.lock().unwrap();
        let mut v: Vec<Message> = st
            .messages
            .get(&channel_id.get())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.id > after)
            .collect();
        // Discord: the 100 messages right after `after`, returned newest-first.
        v.sort_by_key(|m| m.id);
        v.truncate(100);
        v.reverse();
        Ok(v)
    }

    async fn active_threads(&self, _guild_id: GuildId) -> color_eyre::Result<Vec<GuildChannel>> {
        let st = self.0.lock().unwrap();
        Ok(st
            .active_threads
            .iter()
            .filter_map(|id| st.channels.get(id).cloned())
            .collect())
    }

    async fn archived_public_threads(
        &self,
        channel_id: ChannelId,
        before: Option<String>,
    ) -> color_eyre::Result<ThreadPage> {
        self.log(format!("archived_public_threads {channel_id} {before:?}"));
        let st = self.0.lock().unwrap();
        let mut threads: Vec<GuildChannel> = st
            .archived_threads
            .get(&channel_id.get())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|id| st.channels.get(id).cloned())
            .collect();
        // newest archive first, like Discord
        let ts = |c: &GuildChannel| {
            c.thread_metadata
                .and_then(|m| m.archive_timestamp)
                .map(|t| t.to_string())
                .unwrap_or_default()
        };
        threads.sort_by_key(|c| std::cmp::Reverse(ts(c)));
        if let Some(b) = before {
            threads.retain(|c| ts(c) < b);
        }
        // Small page size so pagination is exercised.
        let has_more = threads.len() > 2;
        threads.truncate(2);
        Ok(ThreadPage { threads, has_more })
    }
}

// ---------------------------------------------------------------------------------------------
// Fake media fetcher / object store
// ---------------------------------------------------------------------------------------------

#[derive(Default, Clone)]
pub struct FakeFetcher {
    pub bodies: Arc<Mutex<HashMap<String, Bytes>>>,
    pub fetched: Arc<Mutex<Vec<String>>>,
}

impl FakeFetcher {
    pub fn with(self, url: &str, body: &[u8]) -> Self {
        self.bodies
            .lock()
            .unwrap()
            .insert(url.to_string(), Bytes::copy_from_slice(body));
        self
    }
}

#[async_trait]
impl MediaFetcher for FakeFetcher {
    async fn fetch(&self, url: &str) -> color_eyre::Result<Bytes> {
        self.fetched.lock().unwrap().push(url.to_string());
        let b = self.bodies.lock().unwrap();
        // Unknown URLs get a deterministic per-URL body.
        Ok(b.get(url)
            .cloned()
            .unwrap_or_else(|| Bytes::from(format!("body of {url}"))))
    }
}

#[derive(Default, Clone)]
pub struct MemStore {
    pub objects: Arc<Mutex<HashMap<String, Bytes>>>,
    pub fail: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl ObjectStore for MemStore {
    async fn put(&self, key: &str, body: Bytes, _content_type: &str) -> color_eyre::Result<()> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            color_eyre::eyre::bail!("S3 unavailable (fake)");
        }
        self.objects.lock().unwrap().insert(key.to_string(), body);
        Ok(())
    }
}

pub struct Harness {
    pub ctx: AppCtx,
    pub discord: FakeDiscord,
    pub fetcher: FakeFetcher,
    pub store: MemStore,
}

impl Harness {
    pub async fn new() -> Self {
        Self::with(FakeDiscord::new(), FakeFetcher::default()).await
    }
    pub async fn with(discord: FakeDiscord, fetcher: FakeFetcher) -> Self {
        let pool = fresh_db().await;
        let store = MemStore::default();
        let ctx = AppCtx::new(
            pool,
            Arc::new(discord.clone()),
            Arc::new(fetcher.clone()),
            Arc::new(store.clone()),
        );
        Self {
            ctx,
            discord,
            fetcher,
            store,
        }
    }
    pub fn pool(&self) -> &PgPool {
        &self.ctx.db_pool
    }
    /// A context sharing DB/fakes but with fresh (empty) caches, like a second process or a
    /// concurrent task that raced past the cache check.
    pub fn uncached_ctx(&self) -> AppCtx {
        AppCtx::new(
            self.ctx.db_pool.clone(),
            Arc::new(self.discord.clone()),
            Arc::new(self.fetcher.clone()),
            Arc::new(self.store.clone()),
        )
    }
    pub async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(sql)
            .fetch_one(self.pool())
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }
}
