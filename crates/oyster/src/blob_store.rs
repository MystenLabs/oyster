use std::{future::Future, path::PathBuf, pin::Pin};

use blake2::{Blake2s256, Digest};

use crate::AccountId;

/// Content-addressed blob identifier (hex-encoded BLAKE2s-256 hash for local store).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobId(pub String);

/// Result of a successful blob store operation.
#[derive(Debug)]
pub struct StoreResult {
    /// The content-addressed blob ID.
    pub blob_id: BlobId,
    /// On-chain Sui object ID of the `PooledBlob`, if the blob was registered on Walrus.
    pub pooled_blob_object_id: Option<String>,
    /// Walrus-encoded size in bytes, when the blob was freshly registered
    /// on-chain. `None` for `LocalBlobStore` and for the dedup short-circuit
    /// path (the original registering row already carries the encoded size).
    pub encoded_size: Option<u64>,
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
    /// The upstream blob store was unreachable before returning a status
    /// (connection refused, DNS failure, request timeout). Surfaces as 502.
    #[error("upstream blob store unreachable: {0}")]
    Unreachable(String),
    /// The upstream blob store (Walrus aggregator) returned a non-success HTTP
    /// status other than 404. Carries the original status so the HTTP layer can
    /// decide whether to pass it through (4xx) or mask it as 502 (5xx).
    #[error("upstream blob store returned {status}: {message}")]
    Upstream {
        /// Original HTTP status code returned by the upstream.
        status: u16,
        /// Response body (or empty when the request had no body, e.g. HEAD).
        message: String,
    },
    /// The account has insufficient on-chain balance to complete the operation.
    #[error("insufficient balance: {0}")]
    InsufficientBalance(String),
    /// Lazy creation of a `StoragePool` failed on-chain. Maps to 502 Bad Gateway.
    #[error("pool creation failed: {0}")]
    PoolCreationFailed(String),
    /// Error bookkeeping pool/blob state in the Oyster database.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait abstracting over different blob storage backends.
pub trait BlobStore: Send + Sync + 'static {
    /// Store blob data and return the resulting blob ID.
    fn store(
        &self,
        data: &[u8],
        account_id: &AccountId,
    ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>>;
    /// Read blob data by its ID.
    fn read(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<Vec<u8>, BlobStoreError>>;
    /// Delete a blob by its ID. `pool_id` is the on-chain `StoragePool`
    /// ObjectID owning the `PooledBlob`; `None` when the backing store has
    /// no on-chain integration (e.g. `LocalBlobStore`). `encoded_size` is
    /// the Walrus-encoded byte count of the blob being deleted; passing 0
    /// tells the backend to skip pool-accounting updates (used by legacy
    /// rows and by backends that don't track encoded size).
    fn delete(
        &self,
        blob_id: &BlobId,
        pool_id: Option<&str>,
        encoded_size: u64,
        account_id: &AccountId,
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
        _account_id: &AccountId,
    ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>> {
        let blob_id = compute_blob_id(data);
        let path = self.blob_path(&blob_id);
        let data = data.to_vec();
        Box::pin(async move {
            if path.exists() {
                return Ok(StoreResult {
                    blob_id,
                    pooled_blob_object_id: None,
                    encoded_size: None,
                });
            }
            let parent = path.parent().expect("blob path must have parent");
            tokio::fs::create_dir_all(parent).await?;
            tokio::fs::write(&path, &data).await?;
            Ok(StoreResult {
                blob_id,
                pooled_blob_object_id: None,
                encoded_size: None,
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
        _pool_id: Option<&str>,
        _encoded_size: u64,
        _account_id: &AccountId,
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
