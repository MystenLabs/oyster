use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// An Oyster account.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Account {
    /// Unique identifier.
    pub id: String,
    /// Associated Pearl wallet account ID, if provisioned.
    pub pearl_account_id: Option<String>,
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
    pub account_id: String,
    /// First 8 characters of the raw key, for identification.
    pub prefix: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 revocation timestamp, if revoked.
    pub revoked_at: Option<String>,
}

/// A newly created API key, including the plaintext secret (shown only once).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyWithSecret {
    /// Unique identifier.
    pub id: String,
    /// First 8 characters of the raw key.
    pub prefix: String,
    /// The plaintext API key secret.
    pub secret: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// A named container for blobs within an account.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Bucket {
    /// Unique identifier.
    pub id: String,
    /// Owning account ID.
    pub account_id: String,
    /// Human-readable bucket name.
    pub name: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// Metadata for a stored blob.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BlobMetadata {
    /// Internal object ID (UUID).
    pub object_id: String,
    /// Content-addressed blob identifier.
    pub blob_id: String,
    /// Containing bucket ID.
    pub bucket_id: String,
    /// Owning account ID.
    pub account_id: String,
    /// MIME content type.
    pub content_type: String,
    /// Size in bytes.
    pub size: i64,
    /// On-chain Sui object ID for the blob, if stored on Walrus.
    pub sui_object_id: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 expiration timestamp, if applicable.
    pub expires_at: Option<String>,
}

/// Internal-only struct for the extension task — contains the minimal fields
/// needed to extend a blob, including the owning account's Pearl wallet ID.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExpiringBlob {
    /// On-chain Sui object ID.
    pub sui_object_id: String,
    /// Blob size in bytes.
    pub size: i64,
    /// ISO 8601 expiration timestamp.
    pub expires_at: String,
    /// Pearl wallet account ID of the blob owner.
    pub pearl_account_id: String,
}

// Request types

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
    /// Internal object ID.
    pub object_id: String,
    /// Content-addressed blob ID.
    pub blob_id: String,
    /// Size in bytes.
    pub size: i64,
    /// On-chain Sui object ID, if applicable.
    pub sui_object_id: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 expiration timestamp, if applicable.
    pub expires_at: Option<String>,
}

/// Response after creating a new account.
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateAccountResponse {
    /// The new account ID.
    pub account_id: String,
    /// The initial API key (with secret).
    pub api_key: ApiKeyWithSecret,
}

/// Generic error response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    /// Human-readable error message.
    pub error: String,
}

/// Response listing wallet information for an account.
#[derive(Debug, Serialize, ToSchema)]
pub struct WalletsResponse {
    /// Whether a Pearl wallet has been provisioned.
    pub provisioned: bool,
    /// List of wallet addresses.
    pub wallets: Vec<WalletInfo>,
}

/// A single wallet's public information.
#[derive(Debug, Serialize, ToSchema)]
pub struct WalletInfo {
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
