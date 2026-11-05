//! End-to-end tests of the fact-extractor worker against a real Postgres
//! (fresh database per test, unidb schema loaded) and a mocked LLM.
//!
//! Skipped when `DATABASE_URL` is not set (see llm_worker_common::test_support).

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, TimeZone as _, Utc};
use fact_extractor::{
    config::Config,
    llm,
    worker::{BatchOutcome, Worker},
};
use llm_worker_common::test_support::{SeedMessage, TestDb, chat_completion};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHANNEL: i64 = 10;
const OTHER_CHANNEL: i64 = 11;
const AUTHOR: i64 = 1000;

fn t(min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 8, 12, min, 0).unwrap()
}

fn config(db: &TestDb, llm: &MockServer, extra: &[(&str, &str)]) -> Config {
    let mut vars: HashMap<String, String> = [
        ("DATABASE_URL", db.url.as_str()),
        ("LLM_BASE_URL", llm.uri().as_str()),
        ("LLM_API_KEY", "test-key"),
        ("LLM_MODEL", "test-model"),
        ("DISCORD_SERVER_NAME", "Test Server"),
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

fn record(kind: &str, ty: &str, value: &str, confidence: f64) -> Value {
    json!({
        "kind": kind, "type": ty, "value": value, "confidence": confidence,
        "reasoning": "test", "started_at": null, "ended_at": null,
        "level": null, "is_current": null
    })
}

fn llm_reply(records: Vec<Value>) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(chat_completion(&json!({ "records": records }).to_string()))
}

async fn attempted(db: &TestDb, message_id: i64) -> bool {
    db.count(&format!(
        "SELECT count(*) FROM fact_extraction_attempts WHERE message_id = {message_id}"
    ))
    .await
        == 1
}

#[tokio::test]
async fn happy_path_stores_facts_evidence_and_attempt() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    db.seed_message(SeedMessage::new(
        1,
        CHANNEL,
        AUTHOR,
        "I live in Berlin and write Rust at work",
        t(0),
    ))
    .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(llm_reply(vec![
            record("fact", "location", "Berlin", 0.95),
            {
                let mut r = record("skill", "programming_language", "Rust", 0.9);
                r["level"] = json!(6);
                r
            },
        ]))
        .expect(1)
        .mount(&llm)
        .await;

    let w = worker(&db, &llm, &[]);
    let outcome = w.process_batch().await.unwrap();
    assert_eq!(
        outcome,
        BatchOutcome {
            fetched: 1,
            attempted: 1,
            ..Default::default()
        }
    );
    // Nothing left to do: must not call the LLM again.
    let outcome = w.process_batch().await.unwrap();
    assert_eq!(outcome, BatchOutcome::default());

    assert_eq!(
        db.count("SELECT count(*) FROM user_facts_and_activities")
            .await,
        2
    );
    assert_eq!(
        db.count("SELECT count(*) FROM fact_and_activity_evidence WHERE message_id = 1")
            .await,
        2
    );
    assert!(attempted(&db, 1).await);
    let skill_level: Option<i16> = sqlx::query_scalar(
        "SELECT level FROM user_facts_and_activities WHERE record_type = 'skill'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(skill_level, Some(6));

    let reqs = llm.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["model"], "test-model");
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("\"Test Server\""));
    assert!(system.contains("\"general\""));
    assert_eq!(
        body["messages"][1]["content"],
        "I live in Berlin and write Rust at work"
    );
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
}

#[tokio::test]
async fn deleted_and_short_messages_are_not_sent_to_the_llm() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    db.seed_message(SeedMessage {
        deleted_at: Some(t(5)),
        ..SeedMessage::new(1, CHANNEL, AUTHOR, "I am a deleted message body", t(0))
    })
    .await;
    db.seed_message(SeedMessage::new(2, CHANNEL, AUTHOR, "short", t(1)))
        .await;
    db.seed_message(SeedMessage::new(
        3,
        CHANNEL,
        AUTHOR,
        "[deleted message]",
        t(2),
    ))
    .await;
    Mock::given(method("POST"))
        .respond_with(llm_reply(vec![]))
        .expect(0)
        .mount(&llm)
        .await;

    let _ = worker(&db, &llm, &[]).process_batch().await;
    assert_eq!(
        db.count("SELECT count(*) FROM fact_extraction_attempts")
            .await,
        0
    );
}

/// A response that is not valid JSON must not be retried forever: after
/// MAX_ATTEMPTS failures the message is recorded as attempted (with no facts).
#[tokio::test]
async fn malformed_json_is_retried_then_given_up() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    db.seed_message(SeedMessage::new(
        1,
        CHANNEL,
        AUTHOR,
        "I live in Vienna, Austria",
        t(0),
    ))
    .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_completion("{ this is not json")),
        )
        .mount(&llm)
        .await;

    let w = worker(
        &db,
        &llm,
        &[("MAX_ATTEMPTS", "2"), ("RETRY_BASE_DELAY_SECS", "0")],
    );
    let mut outcomes = Vec::new();
    for _ in 0..4 {
        outcomes.push(w.process_batch().await.unwrap());
    }
    // a bad answer is the message's fault: no global back-off
    assert_eq!(
        outcomes,
        vec![
            BatchOutcome {
                fetched: 1,
                failed: 1,
                ..Default::default()
            },
            BatchOutcome {
                fetched: 1,
                attempted: 1,
                given_up: 1,
                ..Default::default()
            },
            BatchOutcome::default(),
            BatchOutcome::default(),
        ]
    );

    assert!(
        attempted(&db, 1).await,
        "message with a permanently malformed LLM response was never recorded as attempted"
    );
    assert_eq!(
        db.count("SELECT count(*) FROM user_facts_and_activities")
            .await,
        0
    );
    assert_eq!(
        llm.received_requests().await.unwrap().len(),
        2,
        "LLM must be called exactly MAX_ATTEMPTS times"
    );
}

/// Head-of-line blocking: a full batch of always-failing messages must not
/// stop later messages from being processed.
#[tokio::test]
async fn failing_messages_do_not_block_the_queue() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    db.seed_message(SeedMessage::new(
        1,
        CHANNEL,
        AUTHOR,
        "POISON message one",
        t(0),
    ))
    .await;
    db.seed_message(SeedMessage::new(
        2,
        CHANNEL,
        AUTHOR,
        "POISON message two",
        t(1),
    ))
    .await;
    db.seed_message(SeedMessage::new(
        3,
        OTHER_CHANNEL,
        AUTHOR,
        "I moved to Lisbon",
        t(2),
    ))
    .await;
    Mock::given(method("POST"))
        .and(body_string_contains("POISON"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_completion("garbage")))
        .with_priority(1)
        .mount(&llm)
        .await;
    Mock::given(method("POST"))
        .respond_with(llm_reply(vec![record("fact", "location", "Lisbon", 0.9)]))
        .with_priority(2)
        .mount(&llm)
        .await;

    let w = worker(&db, &llm, &[("BATCH_SIZE", "2")]);
    let first = w.process_batch().await.unwrap();
    assert_eq!(
        first,
        BatchOutcome {
            fetched: 2,
            failed: 2,
            ..Default::default()
        }
    );
    for _ in 0..2 {
        let _ = w.process_batch().await;
    }

    assert!(
        attempted(&db, 3).await,
        "message 3 was never processed: the queue is stalled behind failing messages 1 and 2"
    );
    assert_eq!(
        db.count("SELECT count(*) FROM user_facts_and_activities")
            .await,
        1
    );
    // Still within the retry budget: the failing messages are not given up yet.
    assert!(!attempted(&db, 1).await);
}

/// Provider/infrastructure errors (HTTP 5xx) say nothing about the message:
/// they must not burn the per-message retry budget.
#[tokio::test]
async fn server_errors_do_not_consume_the_retry_budget() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    db.seed_message(SeedMessage::new(
        1,
        CHANNEL,
        AUTHOR,
        "I live in Vienna, Austria",
        t(0),
    ))
    .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream overloaded"))
        .mount(&llm)
        .await;

    let w = worker(
        &db,
        &llm,
        &[("MAX_ATTEMPTS", "1"), ("RETRY_BASE_DELAY_SECS", "0")],
    );
    for _ in 0..3 {
        let outcome = w.process_batch().await.unwrap();
        assert_eq!(outcome.fetched, 1);
        assert_eq!((outcome.failed, outcome.attempted), (1, 0));
        assert!(outcome.backoff.is_some(), "provider outage must back off");
    }
    assert!(
        !attempted(&db, 1).await,
        "message given up because of a provider outage"
    );
    // (async-openai itself retries 5xx up to 3 times per call)
    assert!(llm.received_requests().await.unwrap().len() >= 3);
}

/// Simulate the process dying right when the attempt row is written: after a
/// restart the message must not end up with duplicated facts.
#[tokio::test]
async fn crash_before_attempt_row_does_not_duplicate_facts() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    db.seed_message(SeedMessage::new(
        1,
        CHANNEL,
        AUTHOR,
        "I live in Berlin and I am a nurse",
        t(0),
    ))
    .await;
    Mock::given(method("POST"))
        .respond_with(llm_reply(vec![
            record("fact", "location", "Berlin", 0.95),
            record("fact", "occupation", "nurse", 0.95),
        ]))
        .mount(&llm)
        .await;
    db.fail_inserts_into("fact_extraction_attempts", 1).await;

    let cfg = [("RETRY_BASE_DELAY_SECS", "0")];
    // first run "crashes" while writing the attempt row
    let _ = worker(&db, &llm, &cfg).process_batch().await;
    // restarted process
    let w = worker(&db, &llm, &cfg);
    let _ = w.process_batch().await;

    assert!(attempted(&db, 1).await);
    assert_eq!(
        db.count("SELECT count(*) FROM user_facts_and_activities")
            .await,
        2,
        "facts were duplicated after a crash between the facts commit and the attempt insert"
    );
}

/// The replied-to message is shown in its own section, not again among the
/// surrounding messages.
#[tokio::test]
async fn reply_target_is_not_repeated_in_context() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    db.seed_user(AUTHOR + 1).await;
    db.seed_user(AUTHOR + 2).await;
    db.seed_message(SeedMessage::new(
        1,
        CHANNEL,
        AUTHOR + 1,
        "UNIQUEMARKER does anyone here climb?",
        t(0),
    ))
    .await;
    db.seed_message(SeedMessage::new(
        2,
        CHANNEL,
        AUTHOR + 2,
        "context message",
        t(1),
    ))
    .await;
    let mut reply = SeedMessage::new(3, CHANNEL, AUTHOR, "yes, I boulder twice a week", t(2));
    reply.in_reply_to = Some(1);
    db.seed_message(reply).await;
    // messages 1 and 2 are already done; only 3 is processed
    sqlx::query("INSERT INTO fact_extraction_attempts (message_id) VALUES (1), (2)")
        .execute(&db.pool)
        .await
        .unwrap();
    Mock::given(method("POST"))
        .respond_with(llm_reply(vec![]))
        .mount(&llm)
        .await;

    let outcome = worker(&db, &llm, &[]).process_batch().await.unwrap();
    assert_eq!(outcome.attempted, 1);
    let reqs = llm.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert_eq!(system.matches("UNIQUEMARKER").count(), 1, "{system}");
    assert!(system.contains("# Replying to\n"));
    assert!(system.contains("context message"));
}

#[tokio::test]
async fn llm_requests_time_out() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    db.seed_message(SeedMessage::new(
        1,
        CHANNEL,
        AUTHOR,
        "I live in Vienna, Austria",
        t(0),
    ))
    .await;
    Mock::given(method("POST"))
        .respond_with(llm_reply(vec![]).set_delay(Duration::from_secs(10)))
        .mount(&llm)
        .await;

    let w = worker(&db, &llm, &[("LLM_TIMEOUT_SECS", "1")]);
    let res = tokio::time::timeout(Duration::from_secs(5), w.process_batch()).await;
    assert!(
        res.is_ok(),
        "LLM request hung for more than 5s despite LLM_TIMEOUT_SECS=1"
    );
    assert!(!attempted(&db, 1).await);
}

/// Out-of-range / malformed values from the LLM must be clamped or dropped,
/// never stored as-is, and must not make the whole message fail.
#[tokio::test]
async fn llm_output_is_validated_and_clamped() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    db.seed_message(SeedMessage::new(
        1,
        CHANNEL,
        AUTHOR,
        "a long enough message body",
        t(0),
    ))
    .await;
    let mut over = record("fact", "location", "Berlin", 1.5);
    over["reasoning"] = json!("contains a NUL \u{0000} byte");
    let under = record("fact", "timezone", "UTC+1", -0.3);
    let empty_type = record("fact", "   ", "something", 0.9);
    let empty_value = record("fact", "employer", "", 0.9);
    let mut skill = record("skill", "instrument", "guitar\u{0000}", 0.8);
    skill["level"] = json!(15);
    let mut fractional_level = record("skill", "sport", "climbing", 0.8);
    fractional_level["level"] = json!(3.6);
    let mut inverted = record("activity", "travel", "trip to Rome", 0.9);
    inverted["started_at"] = json!("2026-04-10T00:00:00Z");
    inverted["ended_at"] = json!("2026-04-01T00:00:00Z");
    let mut fact_with_level = record("fact", "language_spoken", "German", 0.9);
    fact_with_level["level"] = json!(7);
    Mock::given(method("POST"))
        .respond_with(llm_reply(vec![
            over,
            under,
            empty_type,
            empty_value,
            skill,
            fractional_level,
            inverted,
            fact_with_level,
        ]))
        .mount(&llm)
        .await;

    let _ = worker(&db, &llm, &[]).process_batch().await;

    assert!(
        attempted(&db, 1).await,
        "message failed instead of dropping bad records"
    );
    assert_eq!(
        db.count("SELECT count(*) FROM user_facts_and_activities")
            .await,
        6,
        "records with an empty type/value must be dropped, the rest kept"
    );
    assert_eq!(
        db.count(
            "SELECT count(*) FROM user_facts_and_activities WHERE confidence < 0 OR confidence > 1"
        )
        .await,
        0,
        "confidence outside [0, 1] stored"
    );
    assert_eq!(
        db.count("SELECT count(*) FROM fact_and_activity_evidence WHERE weight < 0 OR weight > 1")
            .await,
        0,
        "evidence weight outside [0, 1] stored"
    );
    assert_eq!(
        db.count("SELECT count(*) FROM user_facts_and_activities WHERE ended_at < started_at")
            .await,
        0,
        "ended_at before started_at stored"
    );
    assert_eq!(
        db.count(
            "SELECT count(*) FROM user_facts_and_activities
             WHERE level IS NOT NULL AND (record_type <> 'skill' OR level NOT BETWEEN 0 AND 10)"
        )
        .await,
        0
    );
    // 1.5 is stored clamped to 1.0, on the fact and on its evidence weight
    let (confidence, weight): (f32, f32) = sqlx::query_as(
        "SELECT f.confidence, e.weight FROM user_facts_and_activities f
         JOIN fact_and_activity_evidence e ON e.fact_or_activity_id = f.id
         WHERE f.type = 'location'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((confidence, weight), (1.0, 1.0));
    let (confidence, weight): (f32, f32) = sqlx::query_as(
        "SELECT f.confidence, e.weight FROM user_facts_and_activities f
         JOIN fact_and_activity_evidence e ON e.fact_or_activity_id = f.id
         WHERE f.type = 'timezone'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((confidence, weight), (0.0, 0.0), "-0.3 clamped to 0");
    let levels: Vec<Option<i16>> = sqlx::query_scalar(
        "SELECT level FROM user_facts_and_activities WHERE record_type = 'skill' ORDER BY value",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        levels,
        vec![Some(4), Some(10)],
        "climbing (3.6 -> 4), guitar (15 -> 10)"
    );
}

/// The prompt asks for `is_current` on activities; it must be stored.
#[tokio::test]
async fn activity_is_current_is_kept() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    db.seed_message(SeedMessage::new(
        1,
        CHANNEL,
        AUTHOR,
        "Just got back from my trip to Rome",
        t(0),
    ))
    .await;
    let mut trip = record("activity", "travel", "trip to Rome", 0.95);
    trip["is_current"] = json!(false);
    trip["started_at"] = json!("2026-04-01T00:00:00Z");
    trip["ended_at"] = json!("2026-04-10T00:00:00Z");
    Mock::given(method("POST"))
        .respond_with(llm_reply(vec![trip]))
        .mount(&llm)
        .await;

    let _ = worker(&db, &llm, &[]).process_batch().await;

    let is_current: Option<bool> =
        sqlx::query_scalar("SELECT is_current FROM user_facts_and_activities")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(is_current, Some(false));
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

/// Start the binary in a temp dir containing `dotenv` as `.env`.
fn spawn_binary(dotenv: &str, env: &[(&str, &str)]) -> Spawned {
    let dir = std::env::temp_dir().join(format!("fe-start-{}", uuid_like()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".env"), dotenv).unwrap();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fact-extractor"));
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
        child: cmd.spawn().expect("spawn fact-extractor"),
        dir,
    }
}

fn uuid_like() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
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

/// RUST_LOG from `.env` must be honoured (tracing used to be initialised
/// before `.env` was loaded).
#[tokio::test]
async fn rust_log_from_dotenv_is_honoured() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    let ready = std::env::temp_dir().join(format!("fe-ready-{}", uuid_like()));
    let dotenv = format!(
        "RUST_LOG=fact_extractor=info\nDATABASE_URL={}\nLLM_BASE_URL={}\nLLM_API_KEY=k\nLLM_MODEL=m\nDISCORD_SERVER_NAME=s\nREADY_FILE={}\n",
        db.url,
        llm.uri(),
        ready.display()
    );
    let mut p = spawn_binary(&dotenv, &[]);
    // give it time to start up and log its start-up debug lines
    wait_for(|| ready.exists(), Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(500));
    let _ = p.child.kill();
    let out = p.child.wait_with_output_ref();
    let _ = std::fs::remove_file(&ready);
    assert!(
        !out.contains("DEBUG"),
        "RUST_LOG=fact_extractor=info from .env ignored; output:\n{out}"
    );
}

/// The readiness file may only appear once the config is valid and the
/// database is reachable.
#[tokio::test]
async fn ready_file_written_only_after_successful_startup() {
    let Some((db, llm)) = setup().await else {
        return;
    };
    let ready = std::env::temp_dir().join(format!("fe-ready-{}", uuid_like()));
    let ready_s = ready.display().to_string();
    let base = |base_url: &str| {
        format!(
            "DATABASE_URL={}\nLLM_BASE_URL={base_url}\nLLM_API_KEY=k\nLLM_MODEL=m\nDISCORD_SERVER_NAME=s\n",
            db.url
        )
    };

    // invalid LLM_BASE_URL: must exit with an error and never become ready,
    // even with a stale file left by a previous run of the same container
    std::fs::write(&ready, "1").unwrap();
    let mut p = spawn_binary(&base("not-a-url"), &[("READY_FILE", &ready_s)]);
    let exited = wait_for(
        || matches!(p.child.try_wait(), Ok(Some(_))),
        Duration::from_secs(10),
    );
    assert!(exited, "process kept running with an invalid LLM_BASE_URL");
    assert!(!ready.exists(), "ready file written despite invalid config");
    drop(p);

    // valid config: becomes ready
    let mut p = spawn_binary(&base(&llm.uri()), &[("READY_FILE", &ready_s)]);
    assert!(
        wait_for(|| ready.exists(), Duration::from_secs(10)),
        "ready file not written at {ready_s} for a valid config"
    );
    // ... and keeps polling instead of exiting
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        matches!(p.child.try_wait(), Ok(None)),
        "worker exited after becoming ready"
    );
    drop(p);
    let _ = std::fs::remove_file(&ready);
}

trait OutputRef {
    fn wait_with_output_ref(&mut self) -> String;
}

impl OutputRef for std::process::Child {
    fn wait_with_output_ref(&mut self) -> String {
        use std::io::Read as _;
        let _ = self.wait();
        let mut s = String::new();
        if let Some(mut o) = self.stdout.take() {
            let _ = o.read_to_string(&mut s);
        }
        if let Some(mut e) = self.stderr.take() {
            let _ = e.read_to_string(&mut s);
        }
        s
    }
}
