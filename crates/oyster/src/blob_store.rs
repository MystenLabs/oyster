use std::{future::Future, path::PathBuf, pin::Pin};

use blake2::{Blake2s256, Digest};

use crate::{AccountId, FundingAmount};

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
///
/// The HTTP mapping for each variant is owned by
/// [`crate::error::AppError::into_response`]; see the doc-comment table
/// immediately above that `impl` for the per-variant rationale.
#[derive(Debug, thiserror::Error)]
pub enum BlobStoreError {
    /// The requested blob was not found. Maps to 404.
    #[error("blob not found: {0}")]
    NotFound(String),
    /// The supplied blob ID could not be parsed by the backing store
    /// (e.g. not a valid Walrus BlobId). Maps to 400.
    #[error("invalid blob_id: {0}")]
    InvalidBlobId(String),
    /// A local I/O error occurred (filesystem-backed `LocalBlobStore` paths
    /// only). Maps to 500.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A genuinely server-internal failure that wasn't an upstream call —
    /// invariant violations, malformed DB values, missing expected events,
    /// race conditions, encoding bugs. Maps to 500.
    #[error("internal error: {0}")]
    Internal(String),
    /// An upstream Sui/Walrus/Pearl-adjacent call failed (sliver upload,
    /// `current_epoch`, PTB build, etc.). Maps to 502.
    #[error("upstream error: {0}")]
    Upstream(String),
    /// The upstream blob store was unreachable before returning a status
    /// (connection refused, DNS failure, request timeout). Maps to 502.
    #[error("upstream blob store unreachable: {0}")]
    Unreachable(String),
    /// The upstream blob store (Walrus aggregator) returned a non-success HTTP
    /// status other than 404. Carries the original status so the HTTP layer can
    /// decide whether to pass it through (4xx) or mask it as 502 (5xx).
    #[error("upstream blob store returned {status}: {message}")]
    UpstreamStatus {
        /// Original HTTP status code returned by the upstream.
        status: u16,
        /// Response body (or empty when the request had no body, e.g. HEAD).
        message: String,
    },
    /// The account has insufficient on-chain balance to complete the
    /// operation. Maps to 402 Payment Required. The optional
    /// `funding_required` block is surfaced in the response body so the
    /// caller (Harbor, the FE) can top up the Pearl-derived wallet without
    /// an extra round-trip to `/account/wallet`.
    ///
    /// For the initial-pool creation path the estimate covers the one-time
    /// `pool_initial_encoded_capacity_bytes * pool_initial_epochs_ahead`
    /// reservation, which may exceed the bytes the user was trying to
    /// upload — that gap is the per-account pool deposit, not blob storage.
    #[error("insufficient balance: {message}")]
    InsufficientBalance {
        /// Raw error message surfaced by the failing PTB-build or submit.
        message: String,
        /// WAL/SUI top-up estimate, best-effort. `None` when the price
        /// lookup itself failed during error formatting.
        funding_required: Option<FundingAmount>,
        /// API surface that produced this 402 (e.g. `"store_blob"`). Used
        /// as the `operation` label on
        /// `oyster_insufficient_funds_responses_total`. Backend
        /// constructors default to `"unknown"`; routes re-tag via
        /// [`BlobStoreError::with_operation`] before returning.
        operation: &'static str,
    },
    /// Lazy creation of a `StoragePool` failed on-chain during PTB build or
    /// submit and wasn't a balance shortfall. Maps to 502 Bad Gateway.
    ///
    /// The "no `StoragePool` object in create_storage_pool response" case
    /// is treated as a server-internal invariant violation and surfaces as
    /// [`BlobStoreError::Internal`] (→ 500) instead.
    #[error("pool creation failed: {0}")]
    PoolCreationFailed(String),
    /// The account's per-account `max_unencoded_bytes` storage cap was
    /// reached. Surfaced before any on-chain work and maps to
    /// 400 Bad Request. The body carries the cap, current on-chain
    /// encoded usage, the new blob's unencoded size, and a hint to the
    /// admin endpoint that raises the cap.
    #[error("storage cap exceeded: {message}")]
    CapExceeded {
        /// Human-readable reason, suitable for surfacing in the JSON
        /// `error` field.
        message: String,
        /// The configured cap, in unencoded bytes.
        max_unencoded_bytes: i64,
        /// On-chain encoded usage observed at check time, in bytes.
        used_encoded_bytes: u64,
        /// Unencoded size of the new blob the caller tried to upload.
        new_unencoded_bytes: u64,
    },
    /// Upload exceeded the Walrus encoder's per-blob ceiling for the
    /// network's `n_shards`. Surfaced from the encode step in
    /// `DirectWalrusBlobStore::store_impl` before any on-chain work,
    /// and maps to 413 Payload Too Large.
    #[error(
        "payload too large: unencoded_size {unencoded_size} > \
         max_unencoded_bytes_for_network {max_unencoded_for_network} (n_shards={n_shards})"
    )]
    PayloadTooLarge {
        /// Size of the blob the caller attempted to upload, in unencoded bytes.
        unencoded_size: u64,
        /// Number of shards in the network's encoding config.
        n_shards: u16,
        /// Per-network maximum unencoded blob size at this `n_shards`
        /// (from `walrus_core::encoding::max_blob_size_for_n_shards`).
        max_unencoded_for_network: u64,
    },
    /// Error bookkeeping pool/blob state in the Oyster database. Maps to 500.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

impl BlobStoreError {
    /// Tag an `InsufficientBalance` error with the API operation that
    /// produced it. No-op for other variants.
    pub fn with_operation(self, op: &'static str) -> Self {
        match self {
            BlobStoreError::InsufficientBalance {
                message,
                funding_required,
                ..
            } => BlobStoreError::InsufficientBalance {
                message,
                funding_required,
                operation: op,
            },
            other => other,
        }
    }
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
