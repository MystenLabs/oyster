use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not implemented")]
    NotImplemented,
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("internal error: {0}")]
    Internal(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
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
            AppError::NotImplemented => (StatusCode::NOT_IMPLEMENTED, self.to_string()),
            AppError::PayloadTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, self.to_string()),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into()),
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
