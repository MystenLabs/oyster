use std::{future::Future, path::PathBuf, pin::Pin};

use blake2::{Blake2s256, Digest};

/// Content-addressed blob identifier (hex-encoded BLAKE2s-256 hash for local store).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobId(pub String);

/// Result of a successful blob store operation.
#[derive(Debug)]
pub struct StoreResult {
    /// The content-addressed blob ID.
    pub blob_id: BlobId,
    /// On-chain Sui object ID, if the blob was registered on Walrus.
    pub sui_object_id: Option<String>,
}

impl BlobId {
    /// Return the blob ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BlobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Errors that can occur during blob storage operations.
#[derive(Debug, thiserror::Error)]
pub enum BlobStoreError {
    /// The requested blob was not found.
    #[error("blob not found: {0}")]
    NotFound(String),
    /// An I/O error occurred.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// An HTTP or network error occurred.
    #[error("http error: {0}")]
    Http(String),
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait abstracting over different blob storage backends.
pub trait BlobStore: Send + Sync + 'static {
    /// Store blob data and return the resulting blob ID.
    fn store(
        &self,
        data: &[u8],
        account_id: Option<&str>,
    ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>>;
    /// Read blob data by its ID.
    fn read(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<Vec<u8>, BlobStoreError>>;
    /// Delete a blob by its ID.
    fn delete(
        &self,
        blob_id: &BlobId,
        sui_object_id: Option<&str>,
        account_id: Option<&str>,
    ) -> BoxFuture<'_, Result<(), BlobStoreError>>;
    /// Check whether a blob exists.
    fn exists(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<bool, BlobStoreError>>;
}

/// Filesystem-backed blob store for local development and testing.
#[derive(Debug, Clone)]
pub struct LocalBlobStore {
    base_dir: PathBuf,
}

impl LocalBlobStore {
    /// Create a new local blob store rooted at `base_dir`, creating the directory if needed.
    pub async fn new(base_dir: PathBuf) -> Result<Self, BlobStoreError> {
        tokio::fs::create_dir_all(&base_dir).await?;
        Ok(Self { base_dir })
    }

    fn blob_path(&self, blob_id: &BlobId) -> PathBuf {
        let hash = blob_id.as_str();
        let prefix = &hash[..2];
        self.base_dir.join(prefix).join(hash)
    }
}

fn compute_blob_id(data: &[u8]) -> BlobId {
    let mut hasher = Blake2s256::new();
    hasher.update(data);
    let hash = hasher.finalize();
    // TODO: use something like Base64Display::new(hash, &URL_SAFE_NO_PAD).fmt(f) instead
    // of hex-encoding the hash.
    BlobId(hex::encode(hash))
}

impl BlobStore for LocalBlobStore {
    fn store(
        &self,
        data: &[u8],
        _account_id: Option<&str>,
    ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>> {
        let blob_id = compute_blob_id(data);
        let path = self.blob_path(&blob_id);
        let data = data.to_vec();
        Box::pin(async move {
            if path.exists() {
                return Ok(StoreResult {
                    blob_id,
                    sui_object_id: None,
                });
            }
            let parent = path.parent().expect("blob path must have parent");
            tokio::fs::create_dir_all(parent).await?;
            tokio::fs::write(&path, &data).await?;
            Ok(StoreResult {
                blob_id,
                sui_object_id: None,
            })
        })
    }

    fn read(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<Vec<u8>, BlobStoreError>> {
        let path = self.blob_path(blob_id);
        let blob_id_str = blob_id.to_string();
        Box::pin(async move {
            if !path.exists() {
                return Err(BlobStoreError::NotFound(blob_id_str));
            }
            Ok(tokio::fs::read(&path).await?)
        })
    }

    fn delete(
        &self,
        blob_id: &BlobId,
        _sui_object_id: Option<&str>,
        _account_id: Option<&str>,
    ) -> BoxFuture<'_, Result<(), BlobStoreError>> {
        let path = self.blob_path(blob_id);
        Box::pin(async move {
            if path.exists() {
                tokio::fs::remove_file(&path).await?;
            }
            Ok(())
        })
    }

    fn exists(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<bool, BlobStoreError>> {
        let path = self.blob_path(blob_id);
        Box::pin(async move { Ok(path.exists()) })
    }
}
