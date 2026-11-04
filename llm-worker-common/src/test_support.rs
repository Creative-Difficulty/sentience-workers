//! Integration-test harness shared by the LLM workers.
//!
//! * [`TestDb`] creates a brand-new Postgres database per test (on the server
//!   pointed to by `DATABASE_URL`), loads the unidb schema into it at the
//!   revision pinned in `Cargo.lock`, and drops it again when the test ends.
//! * Seed helpers insert the minimum rows the workers need.
//! * [`chat_completion`] builds an OpenAI-compatible chat completion response
//!   for a mocked LLM ([`wiremock`]).
//!
//! When `DATABASE_URL` is not set, [`TestDb::new`] returns `None` and the
//! calling test is expected to skip (set `REQUIRE_DB_TESTS=1` to turn that
//! into a hard failure, e.g. in CI).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection as _, PgConnection, PgPool};
use uuid::Uuid;

pub use wiremock;

/// A throw-away database with the full unidb schema loaded.
pub struct TestDb {
    pub pool: PgPool,
    /// Connection string of the throw-away database.
    pub url: String,
    admin_url: String,
    name: String,
}

impl TestDb {
    /// Create a fresh database, or `None` when no `DATABASE_URL` is configured.
    pub async fn new() -> Option<TestDb> {
        let Ok(admin_url) = std::env::var("DATABASE_URL") else {
            if std::env::var("REQUIRE_DB_TESTS").is_ok_and(|v| v == "1") {
                panic!("REQUIRE_DB_TESTS=1 but DATABASE_URL is not set");
            }
            eprintln!("DATABASE_URL not set: skipping database integration test");
            return None;
        };

        let name = format!("wtest_{}", Uuid::new_v4().simple());
        let mut admin = PgConnection::connect(&admin_url)
            .await
            .expect("connect to DATABASE_URL");
        sqlx::raw_sql(&format!("CREATE DATABASE {name}"))
            .execute(&mut admin)
            .await
            .expect("create test database");
        admin.close().await.ok();

        let url = replace_db_name(&admin_url, &name);
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .expect("connect to test database");
        sqlx::raw_sql(schema_sql())
            .execute(&pool)
            .await
            .expect("load unidb schema into test database");

        Some(TestDb {
            pool,
            url,
            admin_url,
            name,
        })
    }

    /// Make the next `times` INSERTs into `table` fail with an exception
    /// (simulates the process dying at exactly that statement).
    ///
    /// Uses a sequence as the counter because sequences are not rolled back
    /// with the failing transaction.
    pub async fn fail_inserts_into(&self, table: &str, times: i64) {
        let sql = format!(
            r#"
            CREATE SEQUENCE IF NOT EXISTS injected_fault_{table};
            CREATE OR REPLACE FUNCTION injected_fault_{table}() RETURNS trigger AS $$
            BEGIN
              IF nextval('injected_fault_{table}') <= {times} THEN
                RAISE EXCEPTION 'injected crash on insert into {table}';
              END IF;
              RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
            DROP TRIGGER IF EXISTS injected_fault_{table} ON {table};
            CREATE TRIGGER injected_fault_{table} BEFORE INSERT ON {table}
              FOR EACH ROW EXECUTE FUNCTION injected_fault_{table}();
            "#
        );
        sqlx::raw_sql(&sql)
            .execute(&self.pool)
            .await
            .expect("install fault-injection trigger");
    }

    pub async fn seed_channel(&self, channel_id: i64, name: &str) {
        sqlx::query(
            "INSERT INTO discord_channels (channel_id, name, channel_type) VALUES ($1, $2, 'text')",
        )
        .bind(channel_id)
        .bind(name)
        .execute(&self.pool)
        .await
        .expect("seed channel");
    }

    /// Insert a vestibule user plus its discord account; returns the vestibule user id.
    pub async fn seed_user(&self, discord_user_id: i64) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO vestibule_users (id, nickname) VALUES ($1, $2)")
            .bind(id)
            .bind(format!("user{discord_user_id}"))
            .execute(&self.pool)
            .await
            .expect("seed vestibule user");
        sqlx::query(
            "INSERT INTO discord_accounts (discord_user_id, vestibule_user_id, username, display_name)
             VALUES ($1, $2, $3, $3)",
        )
        .bind(discord_user_id)
        .bind(id)
        .bind(format!("user{discord_user_id}"))
        .execute(&self.pool)
        .await
        .expect("seed discord account");
        id
    }

    pub async fn seed_message(&self, m: SeedMessage<'_>) {
        sqlx::query(
            "INSERT INTO messages (message_id, channel_id, sent_by, content, sent_at, deleted_at, in_reply_to)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(m.message_id)
        .bind(m.channel_id)
        .bind(m.sent_by)
        .bind(m.content)
        .bind(m.sent_at)
        .bind(m.deleted_at)
        .bind(m.in_reply_to)
        .execute(&self.pool)
        .await
        .expect("seed message");
    }

    pub async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(sql)
            .fetch_one(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("count query failed: {sql}: {e}"))
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin_url = self.admin_url.clone();
        let name = self.name.clone();
        // Drop is sync; run the cleanup on a private runtime in another thread
        // so this works from inside a #[tokio::test] runtime too.
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("cleanup runtime");
            rt.block_on(async move {
                if let Ok(mut admin) = PgConnection::connect(&admin_url).await {
                    let _ = sqlx::raw_sql(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
                        .execute(&mut admin)
                        .await;
                }
            });
        })
        .join();
    }
}

/// A message row to seed. Use struct update syntax with [`SeedMessage::new`].
#[derive(Debug, Clone)]
pub struct SeedMessage<'a> {
    pub message_id: i64,
    pub channel_id: i64,
    pub sent_by: i64,
    pub content: &'a str,
    pub sent_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub in_reply_to: Option<i64>,
}

impl<'a> SeedMessage<'a> {
    pub fn new(
        message_id: i64,
        channel_id: i64,
        sent_by: i64,
        content: &'a str,
        sent_at: DateTime<Utc>,
    ) -> Self {
        SeedMessage {
            message_id,
            channel_id,
            sent_by,
            content,
            sent_at,
            deleted_at: None,
            in_reply_to: None,
        }
    }
}

/// An OpenAI-compatible `/chat/completions` response body whose single choice
/// carries `content`.
pub fn chat_completion(content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 1_700_000_000u32,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }]
    })
}

fn replace_db_name(url: &str, db: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let base = match base.rfind('/') {
        // keep "postgres://host" intact when there is no database path
        Some(i) if i > "postgres://".len() => &base[..i],
        _ => base,
    };
    match query {
        Some(q) => format!("{base}/{db}?{q}"),
        None => format!("{base}/{db}"),
    }
}

fn schema_sql() -> &'static str {
    static SQL: OnceLock<String> = OnceLock::new();
    SQL.get_or_init(|| {
        let dir = schema_dir();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read schema dir {}: {e}", dir.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "sql"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no .sql files in {}", dir.display());
        files
            .iter()
            .map(|p| std::fs::read_to_string(p).expect("read schema file"))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Directory with the unidb `schema/*.sql` files.
///
/// `UNIDB_SCHEMA_DIR` overrides; otherwise the checkout of the `unidb` git
/// dependency (the exact revision pinned in Cargo.lock) is located with
/// `cargo metadata`.
fn schema_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("UNIDB_SCHEMA_DIR") {
        return PathBuf::from(dir);
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("metadata json");
    let manifest = meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|p| p["name"] == "unidb")
        .and_then(|p| p["manifest_path"].as_str())
        .expect("unidb package in cargo metadata");
    Path::new(manifest)
        .parent()
        .expect("unidb manifest dir")
        .join("schema")
}

#[cfg(test)]
mod tests {
    use super::replace_db_name;

    #[test]
    fn replaces_database_name() {
        assert_eq!(
            replace_db_name("postgres://u:p@h:5432/sentience", "x"),
            "postgres://u:p@h:5432/x"
        );
        assert_eq!(
            replace_db_name("postgres://u:p@h:5432/sentience?sslmode=disable", "x"),
            "postgres://u:p@h:5432/x?sslmode=disable"
        );
        assert_eq!(replace_db_name("postgres://h", "x"), "postgres://h/x");
    }
}
