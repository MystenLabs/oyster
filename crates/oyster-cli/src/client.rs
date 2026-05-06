use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// Model types mirrored from the server (without utoipa).

#[derive(Debug, Serialize, Deserialize)]
pub struct Bucket {
    pub name: String,
    pub account_id: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlobMetadata {
    pub key: String,
    pub blob_id: String,
    pub bucket_name: String,
    pub account_id: String,
    pub content_type: String,
    pub size: i64,
    pub md5: String,
    pub pooled_blob_object_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoreBlobResponse {
    pub key: String,
    pub blob_id: String,
    pub size: i64,
    pub md5: String,
    pub pooled_blob_object_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PaginatedResponse<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletResponse {
    pub address: String,
}

#[derive(Debug, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Debug, Deserialize)]
pub struct BlobTagsResponse {
    pub tags: BTreeMap<String, String>,
}

/// One-row summary of an account, returned by `GET /accounts`.
#[derive(Debug, Serialize, Deserialize)]
pub struct AccountSummary {
    /// Unique identifier.
    pub id: String,
    /// Human-readable account name.
    pub name: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// Number of active (non-revoked) API keys on this account.
    pub active_api_key_count: i64,
}

/// Public API key metadata. Never includes the bearer secret.
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiKeyMetadata {
    /// Unique identifier.
    pub id: String,
    /// First 8 characters of the raw key.
    pub prefix: String,
    /// Human-readable note (defaults to "api").
    pub note: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 revocation timestamp, if revoked.
    pub revoked_at: Option<String>,
}

/// A newly minted API key, including the Bearer token (shown only once).
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiKeyWithBearerToken {
    /// Unique identifier.
    pub id: String,
    /// First 8 characters of the raw key.
    pub prefix: String,
    /// The Bearer token (shown only once).
    pub bearer_token: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// App metadata returned by `GET /admin/app` and the webhook PUT/DELETE
/// endpoints. `webhook_public_key` is only `Some` after a `set` (or `get`
/// when a webhook is currently configured).
#[derive(Debug, Serialize, Deserialize)]
pub struct AppWithPublicKey {
    pub id: String,
    pub name: String,
    pub contact_email: String,
    pub webhook_url: Option<String>,
    pub webhook_public_key: Option<String>,
    pub created_at: String,
}

/// Response after successfully creating a new account, including its
/// initial API key.
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateAccountResponse {
    /// The new account's unique identifier.
    pub account_id: String,
    /// The initial API key (with bearer token).
    pub api_key: ApiKeyWithBearerToken,
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
    admin_key: Option<String>,
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
            admin_key: None,
        }
    }

    /// Construct an `OysterClient` configured with an app admin-key Bearer
    /// token for admin-scoped routes (`/accounts`, `/accounts/{id}/api-keys`).
    pub fn with_admin_key(base_url: String, admin_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            api_key: None,
            admin_key: Some(admin_key),
        }
    }

    fn auth_header(&self) -> Option<String> {
        self.api_key.as_ref().map(|key| format!("Bearer {key}"))
    }

    fn admin_auth_header(&self) -> Result<String, ApiError> {
        self.admin_key
            .as_ref()
            .map(|k| format!("Bearer {k}"))
            .ok_or_else(|| ApiError::Server {
                status: 0,
                message: "admin key not configured".into(),
            })
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

    pub async fn delete_bucket(&self, bucket_name: &str) -> Result<(), ApiError> {
        let resp = self
            .http
            .delete(format!("{}/buckets/{bucket_name}", self.base_url))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    // Blob operations

    pub async fn store_blob(
        &self,
        bucket_name: &str,
        key: &str,
        data: Vec<u8>,
        content_type: &str,
        tags: &[(String, String)],
    ) -> Result<StoreBlobResponse, ApiError> {
        let mut req = self
            .http
            .put(format!(
                "{}/buckets/{bucket_name}/blobs/{key}",
                self.base_url
            ))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .header("Content-Type", content_type);
        for (k, v) in tags {
            req = req.header("x-oyster-tag", format!("{k}={v}"));
        }
        let resp = req.body(data).send().await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn list_blobs(
        &self,
        bucket_name: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<PaginatedResponse<BlobMetadata>, ApiError> {
        let url = build_url(
            &self.base_url,
            &format!("/buckets/{bucket_name}/blobs"),
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

    pub async fn read_blob(
        &self,
        bucket_name: &str,
        key: &str,
    ) -> Result<(Vec<u8>, String), ApiError> {
        let resp = self
            .http
            .get(format!(
                "{}/buckets/{bucket_name}/blobs/{key}",
                self.base_url
            ))
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

    pub async fn delete_blob(&self, bucket_name: &str, key: &str) -> Result<(), ApiError> {
        let resp = self
            .http
            .delete(format!(
                "{}/buckets/{bucket_name}/blobs/{key}",
                self.base_url
            ))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    // Tag operations

    pub async fn list_blob_tags(
        &self,
        bucket_name: &str,
        key: &str,
    ) -> Result<BlobTagsResponse, ApiError> {
        let resp = self
            .http
            .get(format!(
                "{}/buckets/{bucket_name}/blobs/{key}/tags",
                self.base_url
            ))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn set_blob_tags_full(
        &self,
        bucket_name: &str,
        key: &str,
        tags: &BTreeMap<String, String>,
    ) -> Result<(), ApiError> {
        let resp = self
            .http
            .put(format!(
                "{}/buckets/{bucket_name}/blobs/{key}/tags",
                self.base_url
            ))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .json(&serde_json::json!({ "tags": tags }))
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    pub async fn patch_blob_tags(
        &self,
        bucket_name: &str,
        key: &str,
        tags: &BTreeMap<String, String>,
    ) -> Result<(), ApiError> {
        let resp = self
            .http
            .patch(format!(
                "{}/buckets/{bucket_name}/blobs/{key}/tags",
                self.base_url
            ))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .json(&serde_json::json!({ "tags": tags }))
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    pub async fn put_blob_tag(
        &self,
        bucket_name: &str,
        key: &str,
        tag_key: &str,
        tag_value: &str,
    ) -> Result<(), ApiError> {
        let resp = self
            .http
            .put(format!(
                "{}/buckets/{bucket_name}/blobs/{key}/tags/{tag_key}",
                self.base_url
            ))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .header("Content-Type", "text/plain")
            .body(tag_value.to_string())
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    pub async fn delete_blob_tag(
        &self,
        bucket_name: &str,
        key: &str,
        tag_key: &str,
    ) -> Result<(), ApiError> {
        let resp = self
            .http
            .delete(format!(
                "{}/buckets/{bucket_name}/blobs/{key}/tags/{tag_key}",
                self.base_url
            ))
            .header("Authorization", self.auth_header().unwrap_or_default())
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    pub async fn clear_blob_tags(&self, bucket_name: &str, key: &str) -> Result<(), ApiError> {
        let resp = self
            .http
            .delete(format!(
                "{}/buckets/{bucket_name}/blobs/{key}/tags",
                self.base_url
            ))
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

    // Admin-keyed account/key management

    /// List every account owned by the admin-key's app.
    pub async fn list_accounts(&self) -> Result<Vec<AccountSummary>, ApiError> {
        let resp = self
            .http
            .get(format!("{}/accounts", self.base_url))
            .header("Authorization", self.admin_auth_header()?)
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    /// List API keys (active and revoked) on an account.
    pub async fn list_api_keys(&self, account_id: &str) -> Result<Vec<ApiKeyMetadata>, ApiError> {
        let resp = self
            .http
            .get(format!("{}/accounts/{account_id}/api-keys", self.base_url))
            .header("Authorization", self.admin_auth_header()?)
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    /// Create a new account on the admin-key's app, returning the initial
    /// API key (one-time bearer included).
    pub async fn create_account(
        &self,
        name: Option<&str>,
        note: Option<&str>,
    ) -> Result<CreateAccountResponse, ApiError> {
        let mut body = serde_json::Map::new();
        if let Some(n) = name {
            body.insert("name".into(), n.into());
        }
        if let Some(n) = note {
            body.insert("note".into(), n.into());
        }
        let resp = self
            .http
            .post(format!("{}/accounts", self.base_url))
            .header("Authorization", self.admin_auth_header()?)
            .json(&serde_json::Value::Object(body))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    /// Mint a new API key on an existing account. Returns 409 from the
    /// server when the per-account active-key cap is reached.
    pub async fn mint_api_key(
        &self,
        account_id: &str,
        note: Option<&str>,
    ) -> Result<ApiKeyWithBearerToken, ApiError> {
        let mut body = serde_json::Map::new();
        if let Some(n) = note {
            body.insert("note".into(), n.into());
        }
        let resp = self
            .http
            .post(format!("{}/accounts/{account_id}/api-keys", self.base_url))
            .header("Authorization", self.admin_auth_header()?)
            .json(&serde_json::Value::Object(body))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    /// Revoke a specific API key on an account.
    pub async fn revoke_api_key(&self, account_id: &str, key_id: &str) -> Result<(), ApiError> {
        let resp = self
            .http
            .delete(format!(
                "{}/accounts/{account_id}/api-keys/{key_id}",
                self.base_url
            ))
            .header("Authorization", self.admin_auth_header()?)
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    /// Fetch the authenticated app, including its current webhook URL and
    /// public key.
    pub async fn get_app(&self) -> Result<AppWithPublicKey, ApiError> {
        let resp = self
            .http
            .get(format!("{}/admin/app", self.base_url))
            .header("Authorization", self.admin_auth_header()?)
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    /// Register or rotate the webhook URL for the authenticated app.
    /// Returns the freshly-generated public key.
    pub async fn set_webhook_url(&self, webhook_url: &str) -> Result<AppWithPublicKey, ApiError> {
        let resp = self
            .http
            .put(format!("{}/admin/app/webhook", self.base_url))
            .header("Authorization", self.admin_auth_header()?)
            .json(&serde_json::json!({ "webhook_url": webhook_url }))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    /// Clear the webhook URL and discard the keypair.
    pub async fn clear_webhook_url(&self) -> Result<AppWithPublicKey, ApiError> {
        let resp = self
            .http
            .delete(format!("{}/admin/app/webhook", self.base_url))
            .header("Authorization", self.admin_auth_header()?)
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }
}
