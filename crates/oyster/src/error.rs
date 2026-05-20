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
    /// Authorization succeeded but the action is not permitted (403).
    #[error("forbidden: {0}")]
    Forbidden(String),
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
    /// Precondition failed — conditional header mismatch (412).
    #[error("precondition failed")]
    PreconditionFailed,
    /// Not modified — conditional GET/HEAD matched (304).
    #[error("not modified")]
    NotModified,
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

/// HTTP-status mapping table for [`AppError`] and its nested
/// [`crate::blob_store::BlobStoreError`]. Keep in sync with the variants'
/// own doc comments.
///
/// | Variant                              | Status |
/// |---|---|
/// | `NotFound`                           | 404 |
/// | `Unauthorized`                       | 401 |
/// | `Forbidden`                          | 403 |
/// | `BadRequest`                         | 400 |
/// | `Conflict`                           | 409 |
/// | `ServiceUnavailable`                 | 503 |
/// | `NotImplemented`                     | 501 |
/// | `PayloadTooLarge`                    | 413 |
/// | `PreconditionFailed`                 | 412 |
/// | `NotModified`                        | 304 (no body) |
/// | `Internal`                           | 500 |
/// | `Database`                           | 500 |
/// | `BlobStore(NotFound)`                | 404 |
/// | `BlobStore(InvalidBlobId)`           | 400 |
/// | `BlobStore(InsufficientBalance)`     | 402, body carries `funding_required` |
/// | `BlobStore(Io)`                      | 500 (filesystem-backed `LocalBlobStore` only) |
/// | `BlobStore(Internal)`                | 500 (server-internal invariant violation) |
/// | `BlobStore(Upstream)`                | 502 (upstream Sui/Walrus call) |
/// | `BlobStore(Unreachable)`             | 502 |
/// | `BlobStore(UpstreamStatus)`          | passthrough 4xx, mask 5xx → 502 |
/// | `BlobStore(PoolCreationFailed)`      | 502 |
/// | `BlobStore(Database)`                | 500 |
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        use crate::blob_store::BlobStoreError;

        // 304 Not Modified must have no body per HTTP spec.
        if matches!(self, AppError::NotModified) {
            return StatusCode::NOT_MODIFIED.into_response();
        }

        // The InsufficientBalance branch needs to attach a structured
        // `funding_required` block to the JSON body, so we handle it
        // up-front rather than fitting it into the `(status, message)`
        // tuple all the other arms use.
        if let AppError::BlobStore(BlobStoreError::InsufficientBalance {
            message,
            funding_required,
        }) = &self
        {
            tracing::warn!(error = %message, "insufficient balance for blob operation");
            let mut body = serde_json::json!({
                "error": format!("insufficient balance: {message}"),
            });
            if let Some(amount) = funding_required {
                body["funding_required"] = serde_json::json!({
                    "wal_frost": amount.wal_frost.to_string(),
                    "sui_mist": amount.sui_mist.to_string(),
                });
            }
            return (StatusCode::PAYMENT_REQUIRED, axum::Json(body)).into_response();
        }

        let (status, message) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::Forbidden(_) => (StatusCode::FORBIDDEN, self.to_string()),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Conflict(_) => (StatusCode::CONFLICT, self.to_string()),
            AppError::ServiceUnavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            AppError::NotImplemented => (StatusCode::NOT_IMPLEMENTED, self.to_string()),
            AppError::PayloadTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, self.to_string()),
            AppError::PreconditionFailed => (StatusCode::PRECONDITION_FAILED, self.to_string()),
            AppError::NotModified => unreachable!(),
            AppError::Internal(e) => {
                tracing::error!("internal error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
            AppError::Database(e) => {
                tracing::error!("database error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
            AppError::BlobStore(e) => match e {
                BlobStoreError::InsufficientBalance { .. } => unreachable!(
                    "InsufficientBalance handled earlier so we can attach funding_required"
                ),
                BlobStoreError::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
                BlobStoreError::InvalidBlobId(_) => (StatusCode::BAD_REQUEST, self.to_string()),
                BlobStoreError::PoolCreationFailed(msg) => {
                    tracing::error!(error = %msg, "pool creation failed");
                    (StatusCode::BAD_GATEWAY, "pool creation failed".into())
                }
                BlobStoreError::Unreachable(msg) => {
                    tracing::error!(error = %msg, "upstream blob store unreachable");
                    (
                        StatusCode::BAD_GATEWAY,
                        "upstream blob store unreachable".into(),
                    )
                }
                BlobStoreError::Upstream(msg) => {
                    tracing::error!(error = %msg, "upstream blob store error");
                    (StatusCode::BAD_GATEWAY, "upstream blob store error".into())
                }
                BlobStoreError::UpstreamStatus { status, message } => {
                    let code = StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY);
                    if code.is_client_error() {
                        tracing::debug!(
                            upstream_status = status,
                            upstream_body = %message,
                            "upstream blob store client error",
                        );
                        (code, format!("upstream blob store: {message}"))
                    } else {
                        tracing::error!(
                            upstream_status = status,
                            upstream_body = %message,
                            "upstream blob store error",
                        );
                        (StatusCode::BAD_GATEWAY, "upstream blob store error".into())
                    }
                }
                BlobStoreError::Internal(msg) => {
                    tracing::error!(error = %msg, "blob store internal error");
                    (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
                }
                BlobStoreError::Io(io_err) => {
                    tracing::error!(error = %io_err, "blob store I/O error");
                    (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
                }
                BlobStoreError::Database(db_err) => {
                    tracing::error!(error = %db_err, "blob store database error");
                    (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
                }
            },
        };
        (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, http::StatusCode};

    use super::*;
    use crate::blob_store::{BlobStoreError, FundingAmount};

    async fn read_json_body(resp: Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        serde_json::from_slice(&bytes).expect("body is valid JSON")
    }

    #[tokio::test]
    async fn insufficient_balance_without_funding_maps_to_402() {
        let err = AppError::BlobStore(BlobStoreError::InsufficientBalance {
            message: "could not find WAL coins with sufficient balance".into(),
            funding_required: None,
        });
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
        let body = read_json_body(resp).await;
        let msg = body["error"].as_str().expect("error field present");
        assert!(
            msg.contains("insufficient balance"),
            "body error did not contain 'insufficient balance': {msg}",
        );
        assert!(
            body.get("funding_required")
                .map(|v| v.is_null())
                .unwrap_or(true),
            "expected no funding_required when None, body = {body}",
        );
    }

    #[tokio::test]
    async fn insufficient_balance_with_funding_serializes_decimal_strings() {
        let err = AppError::BlobStore(BlobStoreError::InsufficientBalance {
            message: "could not find WAL coins...".into(),
            funding_required: Some(FundingAmount {
                wal_frost: 1_234,
                sui_mist: 10_000_000,
            }),
        });
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
        let body = read_json_body(resp).await;
        assert_eq!(body["funding_required"]["wal_frost"], "1234");
        assert_eq!(body["funding_required"]["sui_mist"], "10000000");
    }

    #[tokio::test]
    async fn not_found_maps_to_404() {
        let err = AppError::BlobStore(BlobStoreError::NotFound("x".into()));
        assert_eq!(err.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_blob_id_maps_to_400() {
        let err = AppError::BlobStore(BlobStoreError::InvalidBlobId("x".into()));
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn upstream_maps_to_502() {
        let err = AppError::BlobStore(BlobStoreError::Upstream("walrus 500".into()));
        assert_eq!(err.into_response().status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn internal_blob_store_maps_to_500() {
        let err = AppError::BlobStore(BlobStoreError::Internal("invariant".into()));
        assert_eq!(
            err.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    #[tokio::test]
    async fn pool_creation_failed_maps_to_502() {
        let err = AppError::BlobStore(BlobStoreError::PoolCreationFailed("boom".into()));
        assert_eq!(err.into_response().status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn upstream_status_passthrough_4xx() {
        let err = AppError::BlobStore(BlobStoreError::UpstreamStatus {
            status: 400,
            message: "bad upstream req".into(),
        });
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn upstream_status_masks_5xx_to_502() {
        let err = AppError::BlobStore(BlobStoreError::UpstreamStatus {
            status: 500,
            message: "upstream boom".into(),
        });
        assert_eq!(err.into_response().status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn unreachable_maps_to_502() {
        let err = AppError::BlobStore(BlobStoreError::Unreachable("conn refused".into()));
        assert_eq!(err.into_response().status(), StatusCode::BAD_GATEWAY);
    }
}
