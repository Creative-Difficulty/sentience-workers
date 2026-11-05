//! Media download + object storage abstractions.

use std::time::Duration;

use bytes::{Bytes, BytesMut};
use serenity::async_trait;

/// Downloads media from a URL.
#[async_trait]
pub trait MediaFetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> color_eyre::Result<Bytes>;
}

/// Stores objects (S3 in production).
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn put(&self, key: &str, body: Bytes, content_type: &str) -> color_eyre::Result<()>;
}

/// Limits for media downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaLimits {
    /// Maximum body size; larger downloads are aborted (attachments can be up to 500 MiB on
    /// boosted servers and were previously buffered fully in memory).
    pub max_bytes: u64,
    /// Total timeout of one download, including the body.
    pub timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for MediaLimits {
    fn default() -> Self {
        Self {
            max_bytes: 25 * 1024 * 1024,
            timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(10),
        }
    }
}

impl MediaLimits {
    /// Reads `MEDIA_MAX_BYTES` and `MEDIA_DOWNLOAD_TIMEOUT_SECS` through `lookup` (normally
    /// `std::env::var`), falling back to the defaults for unset variables.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> color_eyre::Result<MediaLimits> {
        let mut limits = MediaLimits::default();
        if let Some(v) = lookup("MEDIA_MAX_BYTES") {
            limits.max_bytes = v
                .trim()
                .parse()
                .map_err(|e| color_eyre::eyre::eyre!("invalid MEDIA_MAX_BYTES {v:?}: {e}"))?;
        }
        if let Some(v) = lookup("MEDIA_DOWNLOAD_TIMEOUT_SECS") {
            let secs: u64 = v.trim().parse().map_err(|e| {
                color_eyre::eyre::eyre!("invalid MEDIA_DOWNLOAD_TIMEOUT_SECS {v:?}: {e}")
            })?;
            limits.timeout = Duration::from_secs(secs);
        }
        Ok(limits)
    }

    pub fn from_env() -> color_eyre::Result<MediaLimits> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// A reqwest client with these timeouts; build once and share.
    pub fn client_builder(&self) -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            .timeout(self.timeout)
            .connect_timeout(self.connect_timeout)
    }
}

/// reqwest-backed [`MediaFetcher`]: rejects non-2xx responses and bodies above `max_bytes`.
#[derive(Clone)]
pub struct HttpFetcher {
    client: reqwest::Client,
    max_bytes: u64,
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new(reqwest::Client::new())
    }
}

impl HttpFetcher {
    /// Uses `client` as is, with the default size limit.
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            max_bytes: MediaLimits::default().max_bytes,
        }
    }

    /// Shared client with the timeouts and size limit of `limits`.
    pub fn with_limits(limits: &MediaLimits) -> color_eyre::Result<Self> {
        Self::from_builder(limits.client_builder(), limits)
    }

    /// Like [`Self::with_limits`] but starting from a caller-provided builder.
    pub fn from_builder(
        builder: reqwest::ClientBuilder,
        limits: &MediaLimits,
    ) -> color_eyre::Result<Self> {
        Ok(Self {
            client: builder
                .timeout(limits.timeout)
                .connect_timeout(limits.connect_timeout)
                .build()?,
            max_bytes: limits.max_bytes,
        })
    }
}

#[async_trait]
impl MediaFetcher for HttpFetcher {
    async fn fetch(&self, url: &str) -> color_eyre::Result<Bytes> {
        let mut resp = self.client.get(url).send().await?.error_for_status()?;
        if let Some(len) = resp.content_length()
            && len > self.max_bytes
        {
            color_eyre::eyre::bail!(
                "media too large: Content-Length {len} > limit {}",
                self.max_bytes
            );
        }
        let mut body = BytesMut::new();
        while let Some(chunk) = resp.chunk().await? {
            if (body.len() + chunk.len()) as u64 > self.max_bytes {
                color_eyre::eyre::bail!("media too large: body exceeds limit {}", self.max_bytes);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body.freeze())
    }
}

/// S3-backed [`ObjectStore`].
#[derive(Clone)]
pub struct S3Store {
    pub client: aws_sdk_s3::Client,
    pub bucket: String,
}

#[async_trait]
impl ObjectStore for S3Store {
    async fn put(&self, key: &str, body: Bytes, content_type: &str) -> color_eyre::Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(aws_sdk_s3::primitives::ByteStream::from(body))
            .content_type(content_type)
            .send()
            .await?;
        Ok(())
    }
}
