use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub account_id: String,
    pub prefix: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyWithSecret {
    pub id: String,
    pub prefix: String,
    pub secret: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bucket {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobMetadata {
    pub object_id: String,
    pub blob_id: String,
    pub bucket_id: String,
    pub account_id: String,
    pub content_type: String,
    pub size: i64,
    pub auto_extend_duration: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
}

// Request types

#[derive(Debug, Deserialize)]
pub struct CreateBucketRequest {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBlobMetadataRequest {
    pub content_type: Option<String>,
    pub auto_extend_duration: Option<String>,
}

// Response types

#[derive(Debug, Serialize)]
pub struct PaginatedResponse<T: Serialize> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StoreBlobResponse {
    pub object_id: String,
    pub blob_id: String,
    pub size: i64,
    pub created_at: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateAccountResponse {
    pub account_id: String,
    pub api_key: ApiKeyWithSecret,
}

// Query parameter types

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}
