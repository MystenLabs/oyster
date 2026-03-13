use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Application-level error type, mapped to HTTP status codes.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Resource not found (404).
    #[error("not found")]
    NotFound,
    /// Authentication failed (401).
    #[error("unauthorized")]
    Unauthorized,
    /// Invalid request parameters (400).
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Resource conflict (409).
    #[error("conflict: {0}")]
    Conflict(String),
    /// Service unavailable (503).
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
    /// Feature not yet implemented (501).
    #[error("not implemented")]
    NotImplemented,
    /// Request body exceeds size limit (413).
    #[error("payload too large")]
    PayloadTooLarge,
    /// Unexpected internal error (500).
    #[error("internal error: {0}")]
    Internal(String),
    /// Database error (500).
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    /// Blob store error (500).
    #[error("blob store error: {0}")]
    BlobStore(#[from] crate::blob_store::BlobStoreError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Conflict(_) => (StatusCode::CONFLICT, self.to_string()),
            AppError::ServiceUnavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            AppError::NotImplemented => (StatusCode::NOT_IMPLEMENTED, self.to_string()),
            AppError::PayloadTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, self.to_string()),
            AppError::Internal(e) => {
                tracing::error!("internal error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
            AppError::Database(e) => {
                tracing::error!("database error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
            AppError::BlobStore(e) => {
                tracing::error!("blob store error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        };
        (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
    }
}
