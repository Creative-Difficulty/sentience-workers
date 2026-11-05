//! End-to-end tests of the topic-sorter worker against a real Postgres
//! (fresh database per test, unidb schema loaded) and a mocked LLM.
//!
//! Skipped when `DATABASE_URL` is not set (see llm_worker_common::test_support).

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, TimeZone as _, Utc};
use llm_worker_common::test_support::{SeedMessage, TestDb, chat_completion};
use serde_json::{Value, json};
use topic_sorter::{config::Config, llm, worker::Worker};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHANNEL: i64 = 10;
const OTHER_CHANNEL: i64 = 11;
const AUTHOR: i64 = 1000;

fn t(min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 8, 12, 0, 0).unwrap() + chrono::Duration::minutes(min as i64)
}

fn config(db: &TestDb, llm: &MockServer, extra: &[(&str, &str)]) -> Config {
    let mut vars: HashMap<String, String> = [
        ("DATABASE_URL", db.url.as_str()),
        ("LLM_BASE_URL", llm.uri().as_str()),
        ("LLM_API_KEY", "test-key"),
        ("LLM_MODEL", "test-model"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    for (k, v) in extra {
        vars.insert(k.to_string(), v.to_string());
    }
    Config::from_lookup(|k| vars.get(k).cloned()).expect("valid test config")
}

fn worker(db: &TestDb, llm: &MockServer, extra: &[(&str, &str)]) -> Worker {
    let cfg = config(db, llm, extra);
    Worker::new(db.pool.clone(), llm::build_client(&cfg), &cfg)
}

async fn setup() -> Option<(TestDb, MockServer)> {
    let db = TestDb::new().await?;
    db.seed_channel(CHANNEL, "general").await;
    db.seed_channel(OTHER_CHANNEL, "random").await;
    db.seed_user(AUTHOR).await;
    Some((db, MockServer::start().await))
}

async fn seed(db: &TestDb, id: i64, channel: i64, content: &str, minute: u32) {
    db.seed_message(SeedMessage::new(id, channel, AUTHOR, content, t(minute)))
        .await;
}

fn groups(gs: &[(&str, &[i64])]) -> ResponseTemplate {
    let groups: Vec<Value> = gs
        .iter()
        .map(|(topic, ids)| json!({ "topic": topic, "message_ids": ids }))
        .collect();
    ResponseTemplate::new(200)
        .set_body_json(chat_completion(&json!({ "groups": groups }).to_string()))
}

async fn attempted(db: &TestDb, message_id: i64) -> bool {
    db.count(&format!(
        "SELECT count(*) FROM message_classification_attempts WHERE message_id = {message_id}"
    ))
    .await
        == 1
}

async fn topic_of(db: &TestDb, message_id: i64) -> Option<String> {
    sqlx::query_scalar(
        "SELECT t.name FROM topic t JOIN topic_message_relation r ON r.topic_id = t.id
         WHERE r.message_id = $1",
    )
    .bind(message_id)
    .fetch_optional(&db.pool)
    .await
    .unwrap()
}

async fn system_prompts(llm: &MockServer) -> Vec<String> {
    llm.received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| {
            let body: Value = serde_json::from_slice(&r.body).unwrap();
            body["messages"][0]["content"].as_str().unwrap().to_string()
        })
        .collect()
}

#[tokio::test]
async fn happy_path_creates_topics_relations_and_attempts() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    seed(&db, 1, CHANNEL, "anyone fighting the borrow checker?", 0).await;
    seed(&db, 2, CHANNEL, "lunch at noon?", 1).await;
    seed(&db, 3, CHANNEL, "just clone it lol", 2).await;
    seed(&db, 4, CHANNEL, "noon works, ramen?", 3).await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(groups(&[
            ("rust borrow checker", &[1, 3]),
            ("lunch plans", &[2, 4]),
        ]))
        .expect(1)
        .mount(&llm)
        .await;

    let w = worker(&db, &llm, &[]);
    let _ = w.process_batch().await;
    let _ = w.process_batch().await; // queue empty: no second LLM call

    assert_eq!(db.count("SELECT count(*) FROM topic").await, 2);
    assert_eq!(
        db.count("SELECT count(*) FROM topic_message_relation")
            .await,
        4
    );
    assert_eq!(
        db.count("SELECT count(*) FROM message_classification_attempts")
            .await,
        4
    );
    assert_eq!(
        topic_of(&db, 3).await.as_deref(),
        Some("rust borrow checker")
    );
    assert_eq!(topic_of(&db, 4).await.as_deref(), Some("lunch plans"));

    let reqs = llm.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let user = body["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("[1]: anyone fighting the borrow checker?"));
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
}

#[tokio::test]
async fn malformed_json_is_retried_then_given_up() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    seed(&db, 1, CHANNEL, "hello there", 0).await;
    seed(&db, 2, CHANNEL, "general kenobi", 1).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_completion("{\"groups\": [")))
        .mount(&llm)
        .await;

    let w = worker(
        &db,
        &llm,
        &[("MAX_ATTEMPTS", "2"), ("RETRY_BASE_DELAY_SECS", "0")],
    );
    for _ in 0..4 {
        let _ = w.process_batch().await;
    }
    assert!(
        attempted(&db, 1).await && attempted(&db, 2).await,
        "batch with a permanently malformed LLM response was never recorded as attempted"
    );
    assert_eq!(db.count("SELECT count(*) FROM topic").await, 0);
    assert_eq!(llm.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn failing_batch_does_not_block_the_queue() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    seed(&db, 1, CHANNEL, "POISON one", 0).await;
    seed(&db, 2, CHANNEL, "POISON two", 1).await;
    seed(&db, 3, OTHER_CHANNEL, "deploy failed", 2).await;
    seed(&db, 4, OTHER_CHANNEL, "rolling back the deploy", 3).await;
    Mock::given(method("POST"))
        .and(body_string_contains("POISON"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_completion("garbage")))
        .with_priority(1)
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .respond_with(groups(&[("deploy failure", &[3, 4])]))
        .with_priority(2)
        .mount(&llm)
        .await;

    let w = worker(&db, &llm, &[("BATCH_SIZE", "2")]);
    for _ in 0..3 {
        let _ = w.process_batch().await;
    }
    assert!(
        attempted(&db, 3).await,
        "channel {OTHER_CHANNEL} never classified: queue stalled behind failing batch"
    );
    assert_eq!(topic_of(&db, 4).await.as_deref(), Some("deploy failure"));
    assert!(!attempted(&db, 1).await, "still within the retry budget");
}

#[tokio::test]
async fn server_errors_do_not_consume_the_retry_budget() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    seed(&db, 1, CHANNEL, "hello there", 0).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
        .mount(&llm)
        .await;
    let w = worker(
        &db,
        &llm,
        &[("MAX_ATTEMPTS", "1"), ("RETRY_BASE_DELAY_SECS", "0")],
    );
    for _ in 0..3 {
        let _ = w.process_batch().await;
    }
    assert!(
        !attempted(&db, 1).await,
        "message given up because of a provider outage"
    );
    // (async-openai itself retries 5xx up to 3 times per call)
    assert!(llm.received_requests().await.unwrap().len() >= 3);
}

/// Simulate the process dying while writing the attempt rows. The restarted
/// worker must end up with exactly the topics of the run that completed (no
/// half-applied state from the crashed run).
#[tokio::test]
async fn crash_before_attempt_rows_leaves_no_partial_state() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    seed(&db, 1, CHANNEL, "ramen spot near the office?", 0).await;
    seed(&db, 2, CHANNEL, "ichiran on 5th is solid", 1).await;
    Mock::given(method("POST"))
        .respond_with(groups(&[("alpha", &[1, 2])]))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .respond_with(groups(&[("ramen recommendations", &[1, 2])]))
        .with_priority(2)
        .mount(&llm)
        .await;
    db.fail_inserts_into("message_classification_attempts", 1)
        .await;

    let cfg = [("RETRY_BASE_DELAY_SECS", "0")];
    let _ = worker(&db, &llm, &cfg).process_batch().await; // "crashes"
    let w = worker(&db, &llm, &cfg); // restart
    let _ = w.process_batch().await;

    assert!(attempted(&db, 1).await && attempted(&db, 2).await);
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM topic ORDER BY name")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        names,
        vec!["ramen recommendations".to_string()],
        "state from the crashed run was committed"
    );
    assert_eq!(
        topic_of(&db, 1).await.as_deref(),
        Some("ramen recommendations")
    );
}

#[tokio::test]
async fn hallucinated_message_ids_are_ignored() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    // an earlier, already classified message (shows up as context)
    seed(&db, 7, CHANNEL, "older context message", 0).await;
    sqlx::query("INSERT INTO message_classification_attempts (message_id) VALUES (7)")
        .execute(&db.pool)
        .await
        .unwrap();
    seed(&db, 8, CHANNEL, "new message a", 1).await;
    seed(&db, 9, CHANNEL, "new message b", 2).await;
    Mock::given(method("POST"))
        .respond_with(groups(&[("things", &[8, 9, 7, 999_999, -1])]))
        .mount(&llm)
        .await;

    let _ = worker(&db, &llm, &[]).process_batch().await;

    assert_eq!(
        db.count("SELECT count(*) FROM topic_message_relation")
            .await,
        2
    );
    assert_eq!(
        topic_of(&db, 7).await,
        None,
        "context message was re-labelled"
    );
    assert!(attempted(&db, 8).await && attempted(&db, 9).await);
}

#[tokio::test]
async fn deleted_messages_are_not_classified() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    seed(&db, 1, CHANNEL, "visible message", 0).await;
    db.seed_message(SeedMessage {
        deleted_at: Some(t(10)),
        ..SeedMessage::new(2, CHANNEL, AUTHOR, "SECRET deleted message", t(1))
    })
    .await;
    Mock::given(method("POST"))
        .respond_with(groups(&[]))
        .mount(&llm)
        .await;

    let _ = worker(&db, &llm, &[]).process_batch().await;

    let reqs = llm.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let body = String::from_utf8_lossy(&reqs[0].body);
    assert!(
        !body.contains("SECRET deleted message"),
        "deleted message content was sent to the LLM"
    );
}

/// The number of existing topic names injected into the prompt must be
/// bounded, keeping the most recently used ones.
#[tokio::test]
async fn existing_topics_in_prompt_are_capped() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    for i in 0..30i64 {
        seed(&db, 100 + i, OTHER_CHANNEL, "old", i as u32).await;
        let topic: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO topic (name) VALUES ($1) RETURNING id")
                .bind(format!("old topic {i:02}"))
                .fetch_one(&db.pool)
                .await
                .unwrap();
        sqlx::query("INSERT INTO topic_message_relation (topic_id, message_id) VALUES ($1, $2)")
            .bind(topic)
            .bind(100 + i)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO message_classification_attempts (message_id) VALUES ($1)")
            .bind(100 + i)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    seed(&db, 1, CHANNEL, "new message", 100).await;
    Mock::given(method("POST"))
        .respond_with(groups(&[]))
        .mount(&llm)
        .await;

    let _ = worker(&db, &llm, &[("MAX_PROMPT_TOPICS", "5")])
        .process_batch()
        .await;

    let sys = system_prompts(&llm).await.remove(0);
    let listed: Vec<&str> = sys
        .lines()
        .filter(|l| l.starts_with("- old topic"))
        .collect();
    assert_eq!(
        listed.len(),
        5,
        "expected 5 topics in the prompt, got {}",
        listed.len()
    );
    for i in 25..30 {
        assert!(
            listed.contains(&format!("- old topic {i}").as_str()),
            "most recent topic {i} missing: {listed:?}"
        );
    }
}

#[tokio::test]
async fn topic_names_are_normalised_and_empty_ones_dropped() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    for i in 1..=6 {
        seed(&db, i, CHANNEL, "msg", i as u32).await;
    }
    Mock::given(method("POST"))
        .respond_with(groups(&[
            ("  Rust   Borrow\nChecker ", &[1, 2]),
            ("rust borrow checker", &[3]),
            ("   ", &[4, 5]),
            ("null\u{0000}byte", &[6]),
        ]))
        .mount(&llm)
        .await;

    let _ = worker(&db, &llm, &[]).process_batch().await;

    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM topic ORDER BY name")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        names,
        vec!["null byte".to_string(), "rust borrow checker".to_string()]
    );
    assert_eq!(
        topic_of(&db, 3).await.as_deref(),
        Some("rust borrow checker")
    );
    assert_eq!(topic_of(&db, 4).await, None);
    for i in 1..=6 {
        assert!(attempted(&db, i).await, "message {i} not recorded");
    }
}

#[tokio::test]
async fn llm_requests_time_out() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    seed(&db, 1, CHANNEL, "hello there", 0).await;
    Mock::given(method("POST"))
        .respond_with(groups(&[]).set_delay(Duration::from_secs(10)))
        .mount(&llm)
        .await;
    let w = worker(&db, &llm, &[("LLM_TIMEOUT_SECS", "1")]);
    let res = tokio::time::timeout(Duration::from_secs(5), w.process_batch()).await;
    assert!(
        res.is_ok(),
        "LLM request hung for more than 5s despite LLM_TIMEOUT_SECS=1"
    );
}

// ---------------------------------------------------------------------------
// Process start-up (runs the real binary)
// ---------------------------------------------------------------------------

struct Spawned {
    child: std::process::Child,
    dir: std::path::PathBuf,
}

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn unique() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

fn spawn_binary(dotenv: &str, env: &[(&str, &str)]) -> Spawned {
    let dir = std::env::temp_dir().join(format!("ts-start-{}", unique()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".env"), dotenv).unwrap();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_topic-sorter"));
    cmd.current_dir(&dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    Spawned {
        child: cmd.spawn().expect("spawn topic-sorter"),
        dir,
    }
}

fn wait_for(mut cond: impl FnMut() -> bool, max: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < max {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    cond()
}

fn read_output(child: &mut std::process::Child) -> String {
    use std::io::Read as _;
    let _ = child.wait();
    let mut s = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut s);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut s);
    }
    s
}

#[tokio::test]
async fn ready_file_written_only_after_successful_startup() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    let ready = std::env::temp_dir().join(format!("ts-ready-{}", unique()));
    let ready_s = ready.display().to_string();

    // LLM_MODEL missing: must exit with an error and never become ready
    let dotenv = format!(
        "DATABASE_URL={}\nLLM_BASE_URL={}\nLLM_API_KEY=k\n",
        db.url,
        llm.uri()
    );
    // stale file left by a previous run of the same container
    std::fs::write(&ready, "1").unwrap();
    let mut p = spawn_binary(&dotenv, &[("READY_FILE", &ready_s)]);
    let exited = wait_for(
        || matches!(p.child.try_wait(), Ok(Some(_))),
        Duration::from_secs(10),
    );
    assert!(exited, "process kept running without LLM_MODEL");
    assert!(
        !ready.exists(),
        "ready file written although LLM_MODEL is missing"
    );
    drop(p);

    let p = spawn_binary(
        &format!("{dotenv}LLM_MODEL=m\n"),
        &[("READY_FILE", &ready_s)],
    );
    assert!(
        wait_for(|| ready.exists(), Duration::from_secs(10)),
        "ready file not written at {ready_s} for a valid config"
    );
    drop(p);
    let _ = std::fs::remove_file(&ready);
}

#[tokio::test]
async fn rust_log_from_dotenv_is_honoured() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    let ready = std::env::temp_dir().join(format!("ts-ready-{}", unique()));
    // one message so the worker logs a (debug) "sending classify request" line
    seed(&db, 1, CHANNEL, "hello there", 0).await;
    Mock::given(method("POST"))
        .respond_with(groups(&[]))
        .mount(&llm)
        .await;
    let dotenv = format!(
        "RUST_LOG=topic_sorter=info\nDATABASE_URL={}\nLLM_BASE_URL={}\nLLM_API_KEY=k\nLLM_MODEL=m\nREADY_FILE={}\n",
        db.url,
        llm.uri(),
        ready.display()
    );
    let mut p = spawn_binary(&dotenv, &[]);
    wait_for(|| attempted_blocking(&db.url), Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(300));
    let _ = p.child.kill();
    let out = read_output(&mut p.child);
    let _ = std::fs::remove_file(&ready);
    assert!(
        !out.contains("DEBUG"),
        "RUST_LOG=topic_sorter=info from .env ignored; output:\n{out}"
    );
}

fn attempted_blocking(url: &str) -> bool {
    let url = url.to_string();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let Ok(pool) = sqlx::PgPool::connect(&url).await else {
                    return false;
                };
                let n: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM message_classification_attempts")
                        .fetch_one(&pool)
                        .await
                        .unwrap_or(0);
                pool.close().await;
                n > 0
            })
    })
    .join()
    .unwrap_or(false)
}
