use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Account {
    pub id: String,
    pub pearl_account_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKey {
    pub id: String,
    pub account_id: String,
    pub prefix: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyWithSecret {
    pub id: String,
    pub prefix: String,
    pub secret: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Bucket {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BlobMetadata {
    pub object_id: String,
    pub blob_id: String,
    pub bucket_id: String,
    pub account_id: String,
    pub content_type: String,
    pub size: i64,
    pub auto_extend_duration: Option<String>,
    pub sui_object_id: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
}

/// Internal-only struct for the extension task — contains the minimal fields
/// needed to extend a blob, including the owning account's Pearl wallet ID.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExpiringBlob {
    pub sui_object_id: String,
    pub size: i64,
    pub expires_at: String,
    pub pearl_account_id: String,
}

// Request types

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateBucketRequest {
    pub name: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateBlobMetadataRequest {
    pub content_type: Option<String>,
    pub auto_extend_duration: Option<String>,
}

// Response types

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedResponse<T: Serialize + ToSchema> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StoreBlobResponse {
    pub object_id: String,
    pub blob_id: String,
    pub size: i64,
    pub sui_object_id: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateAccountResponse {
    pub account_id: String,
    pub api_key: ApiKeyWithSecret,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WalletsResponse {
    pub provisioned: bool,
    pub wallets: Vec<WalletInfo>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WalletInfo {
    pub address: String,
}

// Query parameter types

#[derive(Debug, Deserialize, IntoParams)]
pub struct PaginationParams {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}
