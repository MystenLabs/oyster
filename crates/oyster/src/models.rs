use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::{AccountId, AppId};

/// An Oyster app — organizational trust boundary for account creation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct App {
    /// Unique identifier.
    pub id: AppId,
    /// Human-readable app name.
    pub name: String,
    /// Contact email for the app owner.
    pub contact_email: String,
    /// Optional webhook URL for extension failure notifications.
    pub webhook_url: Option<String>,
    /// Base64-encoded Ed25519 public key paired with the active webhook URL,
    /// or `None` when no webhook is configured.
    pub webhook_public_key: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// Request body for `PUT /admin/app/webhook`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetWebhookUrlRequest {
    /// The webhook URL to register. Must be `https://`, must not embed
    /// credentials, must have a host. Each call generates a fresh keypair.
    pub webhook_url: String,
}

/// Response shape for app endpoints that return the public key alongside
/// the standard `App` fields.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AppWithPublicKey {
    /// Unique identifier.
    pub id: AppId,
    /// Human-readable app name.
    pub name: String,
    /// Contact email for the app owner.
    pub contact_email: String,
    /// Webhook URL, or `None` when no webhook is configured.
    pub webhook_url: Option<String>,
    /// Base64-encoded Ed25519 public key for verifying webhook deliveries,
    /// or `None` when no webhook is configured.
    pub webhook_public_key: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

impl From<App> for AppWithPublicKey {
    fn from(app: App) -> Self {
        Self {
            id: app.id,
            name: app.name,
            contact_email: app.contact_email,
            webhook_url: app.webhook_url,
            webhook_public_key: app.webhook_public_key,
            created_at: app.created_at,
        }
    }
}

/// An Oyster account.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Account {
    /// Unique identifier.
    pub id: AccountId,
    /// Owning app ID.
    pub app_id: AppId,
    /// Human-readable account name.
    pub name: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 last-update timestamp.
    pub updated_at: String,
}

/// An API key record (without the secret).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKey {
    /// Unique identifier.
    pub id: String,
    /// Owning account ID.
    pub account_id: AccountId,
    /// First 8 characters of the raw key, for identification.
    pub prefix: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 revocation timestamp, if revoked.
    pub revoked_at: Option<String>,
}

/// An S3 access key record (without the secret).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccessKey {
    /// The 20-character access key ID (e.g. "OYAK...").
    pub access_key_id: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 revocation timestamp, if revoked.
    pub revoked_at: Option<String>,
}

/// A newly created S3 access key, including the secret (shown only once).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AccessKeyWithSecret {
    /// The 20-character access key ID.
    pub access_key_id: String,
    /// The 40-character hex secret access key.
    pub secret_access_key: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// A newly created API key, including the Bearer token (shown only once).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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

/// An admin-key record (without the secret).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminKey {
    /// Unique identifier.
    pub id: String,
    /// Owning app ID.
    pub app_id: AppId,
    /// First 8 characters of the raw key, for identification.
    pub prefix: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 revocation timestamp, if revoked.
    pub revoked_at: Option<String>,
}

/// A newly created admin key, including the Bearer token (shown only once).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminKeyWithBearerToken {
    /// Unique identifier.
    pub id: String,
    /// Owning app ID.
    pub app_id: AppId,
    /// First 8 characters of the raw key.
    pub prefix: String,
    /// The Bearer token (shown only once).
    pub bearer_token: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// A named container for blobs within an account.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Bucket {
    /// Globally unique bucket name (primary key).
    pub name: String,
    /// Owning account ID.
    pub account_id: AccountId,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// Metadata for a stored blob.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BlobMetadata {
    /// User-chosen object key (like a file path).
    pub key: String,
    /// Content-addressed blob identifier.
    pub blob_id: String,
    /// Containing bucket name.
    pub bucket_name: String,
    /// Owning account ID.
    pub account_id: AccountId,
    /// MIME content type.
    pub content_type: String,
    /// Size in bytes.
    pub size: i64,
    /// Hex-encoded MD5 digest (S3 ETag).
    pub md5: String,
    /// On-chain Sui object ID of the `PooledBlob`, if stored on Walrus.
    pub pooled_blob_object_id: Option<String>,
    /// Walrus-encoded size in bytes, if registered on-chain. `None` for
    /// local-store blobs and for dedup-skipped duplicate rows (the original
    /// registering row still carries the encoded size).
    pub encoded_size: Option<i64>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

// Request types

/// Request body for creating a new account.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateAccountRequest {
    /// Human-readable account name. Defaults to the account ID if omitted.
    pub name: Option<String>,
    /// Optional note attached to the auto-issued initial API key.
    /// Defaults to "api" when omitted.
    pub note: Option<String>,
}

/// Request body for `POST /accounts/{id}/api-keys`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateApiKeyRequest {
    /// Optional note attached to the new API key. Defaults to "api"
    /// when omitted.
    pub note: Option<String>,
}

/// One-row summary of an account, returned by `GET /accounts`.
#[derive(Debug, Serialize, ToSchema)]
pub struct AccountSummary {
    /// Unique identifier.
    pub id: AccountId,
    /// Human-readable account name.
    pub name: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// Number of active (non-revoked) API keys on this account.
    pub active_api_key_count: i64,
}

/// Public API key metadata. Never includes the bearer secret.
#[derive(Debug, Serialize, ToSchema)]
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

/// Request body for creating a new bucket.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateBucketRequest {
    /// Desired bucket name.
    pub name: String,
}

/// Request body for updating blob metadata.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateBlobMetadataRequest {
    /// New MIME content type.
    pub content_type: Option<String>,
}

/// Request body for a full tag-set replace.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PutTagsRequest {
    /// Complete replacement set of tags.
    pub tags: BTreeMap<String, String>,
}

/// Request body for a partial tag-set merge.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchTagsRequest {
    /// Tags to upsert onto the existing set.
    pub tags: BTreeMap<String, String>,
}

/// Response body for listing a blob's tags.
#[derive(Debug, Serialize, ToSchema)]
pub struct BlobTagsResponse {
    /// Tags for the blob. Deterministically ordered for stable responses.
    pub tags: BTreeMap<String, String>,
}

// Response types

/// A paginated list response.
#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedResponse<T: Serialize + ToSchema> {
    /// The items in this page.
    pub data: Vec<T>,
    /// Opaque cursor for fetching the next page, if more results exist.
    pub next_cursor: Option<String>,
}

/// Response after successfully storing a blob.
#[derive(Debug, Serialize, ToSchema)]
pub struct StoreBlobResponse {
    /// User-chosen object key.
    pub key: String,
    /// Content-addressed blob ID.
    pub blob_id: String,
    /// Size in bytes.
    pub size: i64,
    /// Hex-encoded MD5 digest (S3 ETag).
    pub md5: String,
    /// On-chain Sui object ID of the `PooledBlob`, if applicable.
    pub pooled_blob_object_id: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// Response after creating a new account.
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateAccountResponse {
    /// The new account ID.
    pub account_id: AccountId,
    /// The initial API key (with bearer token).
    pub api_key: ApiKeyWithBearerToken,
}

/// Generic error response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    /// Human-readable error message.
    pub error: String,
}

/// Funding estimate attached to a 402 Payment Required response so the
/// caller knows how much WAL and SUI to top up.
///
/// Amounts are decimal strings to preserve `u64` precision across JSON,
/// matching the `account.funding_required` webhook payload convention.
#[derive(Debug, Serialize, ToSchema)]
pub struct FundingRequiredResponse {
    /// Required WAL, in FROST units (1 WAL = 1e9 FROST).
    pub wal_frost: String,
    /// Required SUI, in MIST units (1 SUI = 1e9 MIST).
    pub sui_mist: String,
}

/// Body returned with a 402 Payment Required when the account's Pearl
/// wallet can't cover the requested operation. `funding_required` is
/// `None` when the price-lookup itself failed.
#[derive(Debug, Serialize, ToSchema)]
pub struct InsufficientBalanceErrorResponse {
    /// Human-readable error message.
    pub error: String,
    /// Best-effort WAL/SUI top-up estimate, or `None` on lookup failure.
    pub funding_required: Option<FundingRequiredResponse>,
}

/// Wallet information for an account.
#[derive(Debug, Serialize, ToSchema)]
pub struct WalletResponse {
    /// Sui address of the wallet.
    pub address: String,
}

// Query parameter types

/// Pagination query parameters.
#[derive(Debug, Deserialize, IntoParams)]
pub struct PaginationParams {
    /// Opaque cursor from a previous response.
    pub cursor: Option<String>,
    /// Maximum number of items to return.
    pub limit: Option<i64>,
}
