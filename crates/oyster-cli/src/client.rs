use serde::{Deserialize, Serialize};

// Model types mirrored from the server (without utoipa).

#[derive(Debug, Serialize, Deserialize)]
pub struct Bucket {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlobMetadata {
    pub object_id: String,
    pub blob_id: String,
    pub bucket_id: String,
    pub account_id: String,
    pub content_type: String,
    pub size: i64,
    pub sui_object_id: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoreBlobResponse {
    pub object_id: String,
    pub blob_id: String,
    pub size: i64,
    pub sui_object_id: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PaginatedResponse<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiKeyWithSecret {
    pub id: String,
    pub prefix: String,
    pub secret: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletResponse {
    pub provisioned: bool,
    pub wallet: Option<WalletInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletInfo {
    pub address: String,
}

#[derive(Debug, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

// Error type

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("server error ({status}): {message}")]
    Server { status: u16, message: String },
    #[error("connection error: {0}")]
    Connection(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl ApiError {
    pub fn exit_code(&self) -> i32 {
        match self {
            ApiError::Server { status, .. } if *status >= 500 => 2,
            ApiError::Connection(_) => 3,
            _ => 1,
        }
    }
}

// Client

pub struct OysterClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

fn build_url(base: &str, path: &str, cursor: Option<&str>, limit: Option<u32>) -> String {
    let mut params = Vec::new();
    if let Some(c) = cursor {
        params.push(format!("cursor={c}"));
    }
    if let Some(l) = limit {
        params.push(format!("limit={l}"));
    }
    if params.is_empty() {
        format!("{base}{path}")
    } else {
        format!("{base}{path}?{}", params.join("&"))
    }
}

impl OysterClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            api_key,
        }
    }

    fn auth_header(&self) -> Option<String> {
        self.api_key.as_ref().map(|key| format!("Bearer {key}"))
    }

    async fn check_error(&self, resp: reqwest::Response) -> Result<reqwest::Response, ApiError> {
        let status = resp.status().as_u16();
        if status >= 400 {
            let message = match resp.json::<ErrorResponse>().await {
                Ok(body) => body.error,
                Err(_) => format!("HTTP {status}"),
            };
            return Err(ApiError::Server { status, message });
        }
        Ok(resp)
    }

    // Bucket operations

    pub async fn create_bucket(&self, name: &str) -> Result<Bucket, ApiError> {
        let resp = self
            .http
            .post(format!("{}/buckets", self.base_url))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn list_buckets(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<PaginatedResponse<Bucket>, ApiError> {
        let url = build_url(&self.base_url, "/buckets", cursor, limit);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn delete_bucket(&self, bucket_id: &str) -> Result<(), ApiError> {
        let resp = self
            .http
            .delete(format!("{}/buckets/{bucket_id}", self.base_url))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    // Blob operations

    pub async fn store_blob(
        &self,
        bucket_id: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> Result<StoreBlobResponse, ApiError> {
        let resp = self
            .http
            .put(format!("{}/buckets/{bucket_id}/blobs", self.base_url))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .header("Content-Type", content_type)
            .body(data)
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn list_blobs(
        &self,
        bucket_id: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<PaginatedResponse<BlobMetadata>, ApiError> {
        let url = build_url(
            &self.base_url,
            &format!("/buckets/{bucket_id}/blobs"),
            cursor,
            limit,
        );
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn read_blob(&self, object_id: &str) -> Result<(Vec<u8>, String), ApiError> {
        let resp = self
            .http
            .get(format!("{}/blobs/{object_id}", self.base_url))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = resp.bytes().await?.to_vec();
        Ok((bytes, content_type))
    }

    pub async fn delete_blob(&self, object_id: &str) -> Result<(), ApiError> {
        let resp = self
            .http
            .delete(format!("{}/blobs/{object_id}", self.base_url))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    // API key operations

    pub async fn create_api_key(&self) -> Result<ApiKeyWithSecret, ApiError> {
        let resp = self
            .http
            .post(format!("{}/account/api-keys", self.base_url))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn revoke_api_key(&self, key_id: &str) -> Result<(), ApiError> {
        let resp = self
            .http
            .delete(format!("{}/account/api-keys/{key_id}", self.base_url))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    // Wallet operations

    pub async fn get_wallet(&self) -> Result<WalletResponse, ApiError> {
        let resp = self
            .http
            .get(format!("{}/account/wallet", self.base_url))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    // Bucket name resolution

    pub async fn resolve_bucket_name(&self, name: &str) -> Result<String, ApiError> {
        let mut cursor = None;
        loop {
            let page = self.list_buckets(cursor.as_deref(), Some(100)).await?;
            for bucket in &page.data {
                if bucket.name == name {
                    return Ok(bucket.id.clone());
                }
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => {
                    return Err(ApiError::Server {
                        status: 404,
                        message: format!("bucket '{name}' not found"),
                    });
                }
            }
        }
    }
}
