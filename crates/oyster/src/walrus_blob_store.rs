use std::{future::Future, pin::Pin};

use crate::blob_store::{BlobId, BlobStore, BlobStoreError, StoreResult};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Blob store backed by the Walrus publisher/aggregator HTTP API.
pub struct WalrusBlobStore {
    client: reqwest::Client,
    publisher_url: String,
    aggregator_url: String,
    default_epochs: u32,
}

impl WalrusBlobStore {
    /// Create a new Walrus HTTP blob store.
    pub fn new(publisher_url: String, aggregator_url: String, default_epochs: u32) -> Self {
        Self {
            client: reqwest::Client::new(),
            publisher_url,
            aggregator_url,
            default_epochs,
        }
    }
}

impl BlobStore for WalrusBlobStore {
    fn store(
        &self,
        data: &[u8],
        _pearl_account_id: Option<&str>,
    ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>> {
        let data = data.to_vec();
        Box::pin(async move {
            let url = format!(
                "{}/v1/blobs?epochs={}&deletable=true",
                self.publisher_url, self.default_epochs
            );
            let resp = self
                .client
                .put(&url)
                .body(data)
                .send()
                .await
                .map_err(|e| BlobStoreError::Http(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(BlobStoreError::Http(format!(
                    "publisher returned {}",
                    resp.status()
                )));
            }

            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| BlobStoreError::Http(e.to_string()))?;

            let blob_id = body["newlyCreated"]["blobObject"]["blobId"]
                .as_str()
                .or_else(|| body["alreadyCertified"]["blobId"].as_str())
                .ok_or_else(|| {
                    BlobStoreError::Http(format!("unexpected publisher response: {body}"))
                })?;

            Ok(StoreResult {
                blob_id: BlobId(blob_id.to_string()),
                sui_object_id: None,
            })
        })
    }

    fn read(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<Vec<u8>, BlobStoreError>> {
        let url = format!("{}/v1/blobs/{}", self.aggregator_url, blob_id);
        Box::pin(async move {
            let resp = self
                .client
                .get(&url)
                .send()
                .await
                .map_err(|e| BlobStoreError::Http(e.to_string()))?;

            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Err(BlobStoreError::NotFound(url));
            }

            if !resp.status().is_success() {
                return Err(BlobStoreError::Http(format!(
                    "aggregator returned {}",
                    resp.status()
                )));
            }

            resp.bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| BlobStoreError::Http(e.to_string()))
        })
    }

    fn delete(
        &self,
        _blob_id: &BlobId,
        _sui_object_id: Option<&str>,
        _pearl_account_id: Option<&str>,
    ) -> BoxFuture<'_, Result<(), BlobStoreError>> {
        Box::pin(async move {
            tracing::warn!(
                "walrus blob deletion requires Sui transaction signing (not yet implemented)"
            );
            Ok(())
        })
    }

    fn exists(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<bool, BlobStoreError>> {
        let url = format!("{}/v1/blobs/{}", self.aggregator_url, blob_id);
        Box::pin(async move {
            let resp = self
                .client
                .head(&url)
                .send()
                .await
                .map_err(|e| BlobStoreError::Http(e.to_string()))?;

            match resp.status() {
                reqwest::StatusCode::OK => Ok(true),
                reqwest::StatusCode::NOT_FOUND => Ok(false),
                status => Err(BlobStoreError::Http(format!(
                    "aggregator returned {status}"
                ))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use wiremock::{
        Mock,
        MockServer,
        ResponseTemplate,
        matchers::{method, path_regex},
    };

    use super::*;

    #[tokio::test]
    async fn store_newly_created() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(r"^/v1/blobs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "newlyCreated": {
                    "blobObject": {
                        "blobId": "blob-abc-123"
                    }
                }
            })))
            .mount(&server)
            .await;

        let store = WalrusBlobStore::new(server.uri(), "http://unused".into(), 5);
        let result = store.store(b"hello walrus", None).await.unwrap();
        assert_eq!(result.blob_id.as_str(), "blob-abc-123");
        assert!(result.sui_object_id.is_none());
    }

    #[tokio::test]
    async fn store_already_certified() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(r"^/v1/blobs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "alreadyCertified": {
                    "blobId": "blob-existing-456"
                }
            })))
            .mount(&server)
            .await;

        let store = WalrusBlobStore::new(server.uri(), "http://unused".into(), 5);
        let result = store.store(b"duplicate data", None).await.unwrap();
        assert_eq!(result.blob_id.as_str(), "blob-existing-456");
        assert!(result.sui_object_id.is_none());
    }

    #[tokio::test]
    async fn store_publisher_error() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_regex(r"^/v1/blobs"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let store = WalrusBlobStore::new(server.uri(), "http://unused".into(), 5);
        let err = store.store(b"fail", None).await.unwrap_err();
        assert!(matches!(err, BlobStoreError::Http(_)));
    }

    #[tokio::test]
    async fn read_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v1/blobs/"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"walrus bytes".to_vec()))
            .mount(&server)
            .await;

        let store = WalrusBlobStore::new("http://unused".into(), server.uri(), 5);
        let data = store.read(&BlobId("some-blob-id".into())).await.unwrap();
        assert_eq!(data, b"walrus bytes");
    }

    #[tokio::test]
    async fn read_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v1/blobs/"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let store = WalrusBlobStore::new("http://unused".into(), server.uri(), 5);
        let err = store
            .read(&BlobId("missing-blob".into()))
            .await
            .unwrap_err();
        assert!(matches!(err, BlobStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn delete_is_noop() {
        let store = WalrusBlobStore::new("http://unused".into(), "http://unused".into(), 5);
        // Should succeed without making any HTTP calls.
        store
            .delete(&BlobId("any-blob".into()), None, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn exists_true() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path_regex(r"^/v1/blobs/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let store = WalrusBlobStore::new("http://unused".into(), server.uri(), 5);
        assert!(store.exists(&BlobId("present-blob".into())).await.unwrap());
    }

    #[tokio::test]
    async fn exists_false() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path_regex(r"^/v1/blobs/"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let store = WalrusBlobStore::new("http://unused".into(), server.uri(), 5);
        assert!(!store.exists(&BlobId("absent-blob".into())).await.unwrap());
    }
}
