//! Media asset storage (bug #9) and the HTTP media fetcher.
use std::sync::atomic::Ordering;

use crate::common::*;
use ingest_vestibule_retriever::handlers;
use ingest_vestibule_retriever::media::{HttpFetcher, MediaFetcher};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const AVATAR_A: &str = "0123456789abcdef0123456789abcdef";
const AVATAR_B: &str = "fedcba9876543210fedcba9876543210";

fn user_with_avatar(id: u64, hash: &str) -> serenity::all::User {
    let mut v = user_json(id, &format!("u{id}"));
    v["avatar"] = json!(hash);
    serde_json::from_value(v).unwrap()
}

#[tokio::test]
async fn avatar_asset_not_recorded_when_upload_fails() {
    let h = Harness::new().await;
    h.store.fail.store(true, Ordering::SeqCst);
    handlers::ensure_user(&h.ctx, &user_with_avatar(7, AVATAR_A))
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM media_assets WHERE object_key LIKE 'discord/avatars/%'")
            .await,
        0,
        "media_assets row points at an object that was never uploaded"
    );
}

#[tokio::test]
async fn identical_avatars_are_stored_once() {
    let h = Harness::new().await;
    // two users with byte-identical avatars (e.g. the same picture uploaded)
    for (id, hash) in [(7u64, AVATAR_A), (8, AVATAR_B)] {
        let u = user_with_avatar(id, hash);
        let url = ingest_vestibule_retriever::mapping::avatar_spec(&u)
            .unwrap()
            .url;
        h.fetcher
            .bodies
            .lock()
            .unwrap()
            .insert(url, bytes::Bytes::from_static(b"same pixels"));
        handlers::ensure_user(&h.ctx, &u).await.unwrap();
    }
    assert_eq!(h.count("SELECT count(*) FROM media_assets").await, 1);
    assert_eq!(
        h.store.objects.lock().unwrap().len(),
        1,
        "second identical avatar was uploaded without a media_assets row (stray object)"
    );
}

fn message_with_attachment(id: u64, att: u64, url: &str) -> serenity::all::Message {
    let mut v = message_json(id, CHANNEL, user_json(7, "u7"), "");
    v["attachments"] = json!([{
        "id": att.to_string(), "filename": "a.png", "size": 3,
        "url": url, "proxy_url": url, "content_type": "image/png"
    }]);
    message(v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_identical_attachments_all_get_linked() {
    let h = Harness::new().await;
    handlers::ensure_message(&h.ctx, &simple_message(1, 7, "warm caches"))
        .await
        .unwrap();
    let tasks: Vec<_> = (0..32u64)
        .map(|i| {
            let ctx = h.ctx.clone();
            let url = format!("https://cdn.test/{i}.png");
            h.fetcher
                .bodies
                .lock()
                .unwrap()
                .insert(url.clone(), bytes::Bytes::from_static(b"meme"));
            let m = message_with_attachment(2000 + i, 3000 + i, &url);
            tokio::spawn(async move { handlers::ensure_message(&ctx, &m).await })
        })
        .collect();
    for t in tasks {
        t.await.unwrap().unwrap();
    }
    assert_eq!(h.count("SELECT count(*) FROM media_assets").await, 1);
    assert_eq!(
        h.count(
            "SELECT count(DISTINCT message_id) FROM message_attachments WHERE message_id >= 2000"
        )
        .await,
        32,
        "some messages lost their attachment to a content_hash unique violation"
    );
}

#[tokio::test]
async fn same_message_processed_twice_links_attachment_once() {
    let h = Harness::new().await;
    let m = message_with_attachment(2100, 3100, "https://cdn.test/x.png");
    handlers::ensure_message(&h.ctx, &m).await.unwrap();
    // e.g. live event and historical scan racing with separate cache views
    handlers::ensure_message(&h.uncached_ctx(), &m)
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM message_attachments WHERE message_id = 2100")
            .await,
        1
    );
}

#[tokio::test]
async fn attachment_upload_failure_records_nothing() {
    let h = Harness::new().await;
    h.store.fail.store(true, Ordering::SeqCst);
    handlers::ensure_message(
        &h.ctx,
        &message_with_attachment(2200, 3200, "https://cdn.test/y.png"),
    )
    .await
    .unwrap();
    assert_eq!(h.count("SELECT count(*) FROM media_assets").await, 0);
    assert_eq!(h.count("SELECT count(*) FROM message_attachments").await, 0);
    // the message itself is still stored
    assert_eq!(
        h.count("SELECT count(*) FROM messages WHERE message_id = 2200")
            .await,
        1
    );
}

// ---------------------------------------------------------------------------------------------
// HttpFetcher against a local loopback server (never leaves 127.0.0.1)
// ---------------------------------------------------------------------------------------------

/// Serves: /ok (5 bytes), /missing (404), /huge (30 MiB with Content-Length),
/// /huge-chunked (30 MiB chunked), /hang (accepts, never answers).
pub async fn local_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                const HUGE: usize = 30 * 1024 * 1024;
                let chunk = vec![b'x'; 1024 * 1024];
                let _ = match path.as_str() {
                    "/ok-chunked" => sock.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n").await,
                    "/ok" => sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").await,
                    "/missing" => sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found").await,
                    "/huge" => {
                        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {HUGE}\r\nConnection: close\r\n\r\n");
                        let _ = sock.write_all(head.as_bytes()).await;
                        let mut r = Ok(());
                        for _ in 0..30 { r = sock.write_all(&chunk).await; if r.is_err() { break } }
                        r
                    }
                    "/huge-chunked" => {
                        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await;
                        let mut r = Ok(());
                        for _ in 0..30 {
                            let _ = sock.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await;
                            r = sock.write_all(&chunk).await;
                            let _ = sock.write_all(b"\r\n").await;
                            if r.is_err() { break }
                        }
                        let _ = sock.write_all(b"0\r\n\r\n").await;
                        r
                    }
                    "/hang" => { tokio::time::sleep(std::time::Duration::from_secs(600)).await; Ok(()) }
                    _ => sock.write_all(b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n").await,
                };
            });
        }
    });
    format!("http://{addr}")
}

fn local_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn fetcher_returns_body() {
    let base = local_server().await;
    let f = HttpFetcher::new(local_client());
    assert_eq!(&f.fetch(&format!("{base}/ok")).await.unwrap()[..], b"hello");
}

#[tokio::test]
async fn fetcher_rejects_http_errors() {
    let base = local_server().await;
    let f = HttpFetcher::new(local_client());
    assert!(
        f.fetch(&format!("{base}/missing")).await.is_err(),
        "404 body would be stored as the media asset"
    );
}

#[tokio::test]
async fn fetcher_rejects_oversized_bodies_by_default() {
    let base = local_server().await;
    let f = HttpFetcher::new(local_client());
    assert!(
        f.fetch(&format!("{base}/huge")).await.is_err(),
        "30 MiB body accepted"
    );
    assert!(
        f.fetch(&format!("{base}/huge-chunked")).await.is_err(),
        "30 MiB chunked body accepted"
    );
}

#[tokio::test]
async fn fetcher_honours_configured_limits() {
    use ingest_vestibule_retriever::media::MediaLimits;
    let base = local_server().await;
    let limits = MediaLimits {
        max_bytes: 4,
        timeout: std::time::Duration::from_millis(500),
        connect_timeout: std::time::Duration::from_millis(500),
    };
    let f = HttpFetcher::from_builder(reqwest::Client::builder().no_proxy(), &limits).unwrap();
    assert!(
        f.fetch(&format!("{base}/ok")).await.is_err(),
        "5 bytes > 4 byte limit"
    );
    let started = std::time::Instant::now();
    assert!(f.fetch(&format!("{base}/hang")).await.is_err());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "timeout not applied"
    );
}

#[test]
fn media_limits_from_env_lookup() {
    use ingest_vestibule_retriever::media::MediaLimits;
    let none = MediaLimits::from_lookup(|_| None).unwrap();
    assert_eq!(none, MediaLimits::default());
    let set = MediaLimits::from_lookup(|k| match k {
        "MEDIA_MAX_BYTES" => Some("1024".into()),
        "MEDIA_DOWNLOAD_TIMEOUT_SECS" => Some(" 7 ".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(set.max_bytes, 1024);
    assert_eq!(set.timeout, std::time::Duration::from_secs(7));
    assert!(MediaLimits::from_lookup(|_| Some("lots".into())).is_err());
}

/// Size-limit boundaries (from mutation testing): a body of exactly `max_bytes` is accepted, one
/// byte more is rejected, with and without Content-Length.
#[tokio::test]
async fn fetcher_limit_is_inclusive_with_and_without_content_length() {
    use ingest_vestibule_retriever::media::MediaLimits;
    let base = local_server().await;
    let limits = |max_bytes| MediaLimits {
        max_bytes,
        ..MediaLimits::default()
    };
    let client = |max| {
        HttpFetcher::from_builder(reqwest::Client::builder().no_proxy(), &limits(max)).unwrap()
    };
    for path in ["/ok", "/ok-chunked"] {
        let url = format!("{base}{path}");
        assert_eq!(
            &client(5).fetch(&url).await.unwrap()[..],
            b"hello",
            "{path} at limit"
        );
        assert!(client(4).fetch(&url).await.is_err(), "{path} over limit");
    }
}
