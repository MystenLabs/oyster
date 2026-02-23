use std::{future::Future, path::PathBuf, pin::Pin};

use blake2::{Blake2s256, Digest};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobId(pub String);

impl BlobId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BlobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlobStoreError {
    #[error("blob not found: {0}")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("http error: {0}")]
    Http(String),
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait BlobStore: Send + Sync + 'static {
    fn store(&self, data: &[u8]) -> BoxFuture<'_, Result<BlobId, BlobStoreError>>;
    fn read(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<Vec<u8>, BlobStoreError>>;
    fn delete(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<(), BlobStoreError>>;
    fn exists(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<bool, BlobStoreError>>;
}

#[derive(Debug, Clone)]
pub struct LocalBlobStore {
    base_dir: PathBuf,
}

impl LocalBlobStore {
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
    fn store(&self, data: &[u8]) -> BoxFuture<'_, Result<BlobId, BlobStoreError>> {
        let blob_id = compute_blob_id(data);
        let path = self.blob_path(&blob_id);
        let data = data.to_vec();
        Box::pin(async move {
            if path.exists() {
                return Ok(blob_id);
            }
            let parent = path.parent().expect("blob path must have parent");
            tokio::fs::create_dir_all(parent).await?;
            tokio::fs::write(&path, &data).await?;
            Ok(blob_id)
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

    fn delete(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<(), BlobStoreError>> {
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
