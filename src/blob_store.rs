use std::{future::Future, path::PathBuf};

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
}

pub trait BlobStore: Send + Sync + 'static {
    fn store(&self, data: &[u8]) -> impl Future<Output = Result<BlobId, BlobStoreError>> + Send;
    fn read(
        &self,
        blob_id: &BlobId,
    ) -> impl Future<Output = Result<Vec<u8>, BlobStoreError>> + Send;
    fn delete(&self, blob_id: &BlobId) -> impl Future<Output = Result<(), BlobStoreError>> + Send;
    fn exists(&self, blob_id: &BlobId)
    -> impl Future<Output = Result<bool, BlobStoreError>> + Send;
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
    BlobId(hex::encode(hash))
}

impl BlobStore for LocalBlobStore {
    async fn store(&self, data: &[u8]) -> Result<BlobId, BlobStoreError> {
        let blob_id = compute_blob_id(data);
        let path = self.blob_path(&blob_id);
        if path.exists() {
            return Ok(blob_id);
        }
        let parent = path.parent().expect("blob path must have parent");
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::write(&path, data).await?;
        Ok(blob_id)
    }

    async fn read(&self, blob_id: &BlobId) -> Result<Vec<u8>, BlobStoreError> {
        let path = self.blob_path(blob_id);
        if !path.exists() {
            return Err(BlobStoreError::NotFound(blob_id.to_string()));
        }
        Ok(tokio::fs::read(&path).await?)
    }

    async fn delete(&self, blob_id: &BlobId) -> Result<(), BlobStoreError> {
        let path = self.blob_path(blob_id);
        if path.exists() {
            tokio::fs::remove_file(&path).await?;
        }
        Ok(())
    }

    async fn exists(&self, blob_id: &BlobId) -> Result<bool, BlobStoreError> {
        Ok(self.blob_path(blob_id).exists())
    }
}
