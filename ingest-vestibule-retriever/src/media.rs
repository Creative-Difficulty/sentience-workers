//! Media download + object storage abstractions.

use bytes::Bytes;
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

/// reqwest-backed [`MediaFetcher`].
#[derive(Clone, Default)]
pub struct HttpFetcher {
    client: reqwest::Client,
}

impl HttpFetcher {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MediaFetcher for HttpFetcher {
    async fn fetch(&self, url: &str) -> color_eyre::Result<Bytes> {
        let resp = self.client.get(url).send().await?;
        Ok(resp.bytes().await?)
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
