use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Returns `true` if `e` is a foreign-key constraint violation reported by
/// either Postgres (SQLSTATE `23503`) or SQLite (error message containing
/// `"FOREIGN KEY constraint failed"`). Mirrors the
/// `UNIQUE constraint`-detection shape used in
/// `routes/buckets.rs::create_bucket` and `s3.rs::create_bucket`.
///
/// Used by `routes::blobs::store_blob` and `s3::put_object` to map the
/// `blobs_bucket_name_fkey` race (bucket deleted between the pre-check
/// and the post-store insert) onto a 409 / `NoSuchBucket` response
/// instead of an opaque 500.
pub fn is_foreign_key_violation(e: &sqlx::Error) -> bool {
    let sqlx::Error::Database(db_err) = e else {
        return false;
    };
    if db_err.code().is_some_and(|c| c == "23503") {
        return true;
    }
    db_err.message().contains("FOREIGN KEY constraint failed")
}

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
    /// Admin tried to lower `max_unencoded_bytes` to a value that would
    /// orphan currently-stored data — the on-chain `used_encoded_bytes`
    /// already exceeds the encoded threshold for the proposed cap (400).
    #[error(
        "would orphan stored data: used_encoded_bytes {used_encoded_bytes} > \
         encoded threshold {threshold_encoded} for max_unencoded_bytes {max_unencoded_bytes}"
    )]
    MaxStorageWouldOrphan {
        /// Proposed new cap, in unencoded bytes.
        max_unencoded_bytes: i64,
        /// On-chain encoded usage observed at check time.
        used_encoded_bytes: i64,
        /// Encoded-byte threshold computed from `max_unencoded_bytes`
        /// (forward-encoded via the same `f` as upload-side cap
        /// enforcement).
        threshold_encoded: i64,
    },
    /// The on-chain shrink PTB aborted (e.g. a race with a concurrent
    /// upload bumped `used_encoded` above the proposed threshold
    /// between our pre-check and the PTB landing). Surfaces the chain's
    /// MoveAbort context so the admin can retry (400).
    #[error(
        "shrink PTB aborted ({description}); max_unencoded_bytes={max_unencoded_bytes}, \
         extract_size={extract_size}"
    )]
    MaxStorageShrinkAborted {
        /// Human-readable description from the gRPC `ExecutionError`.
        description: String,
        /// Proposed new cap, in unencoded bytes.
        max_unencoded_bytes: i64,
        /// Encoded-byte extract size we attempted to remove from the
        /// on-chain pool.
        extract_size: i64,
    },
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
/// | `BlobStore(Io)`                      | 500, suffixed `[BlobStoreError::Io]` (filesystem-backed `LocalBlobStore` only) |
/// | `BlobStore(Internal)`                | 500, suffixed `[BlobStoreError::Internal]` |
/// | `BlobStore(Upstream)`                | 502, suffixed `[BlobStoreError::Upstream]` (upstream Sui/Walrus call) |
/// | `BlobStore(Unreachable)`             | 502, suffixed `[BlobStoreError::Unreachable]` |
/// | `BlobStore(UpstreamStatus)`          | passthrough 4xx; mask 5xx → 502, suffixed `[BlobStoreError::UpstreamStatus]` |
/// | `BlobStore(PoolCreationFailed)`      | 502, suffixed `[BlobStoreError::PoolCreationFailed]` |
/// | `BlobStore(CapExceeded)`             | 400, body carries `cap_exceeded` block |
/// | `BlobStore(PayloadTooLarge)`         | 413, body carries `payload_too_large` block |
/// | `BlobStore(Database)`                | 500, suffixed `[BlobStoreError::Database]` |
///
/// The `[BlobStoreError::<Variant>]` suffix on masked 5xx/500 messages
/// distinguishes variants that otherwise collapse to the same generic text
/// (e.g. `Upstream` vs. `UpstreamStatus` both say `"upstream blob store
/// error"`; `Internal`/`Io`/`Database` all say `"internal error"`). It is
/// informational, not a stable machine-parseable contract — the upstream's
/// raw error text remains in server logs only.
/// | `MaxStorageWouldOrphan`              | 400, body carries `would_orphan` block |
/// | `MaxStorageShrinkAborted`            | 400, body carries `shrink_aborted` block |
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
            operation,
        }) = &self
        {
            metrics::counter!(
                crate::metrics::INSUFFICIENT_FUNDS_RESPONSES_TOTAL,
                "operation" => *operation,
            )
            .increment(1);
            tracing::warn!(
                error = %message,
                operation = %operation,
                "insufficient balance for blob operation",
            );
            let mut body = serde_json::json!({
                "error": format!("insufficient balance: {message}"),
            });
            if let Some(amount) = funding_required {
                body["funding_required"] =
                    serde_json::to_value(amount).expect("FundingAmount Serialize is infallible");
            }
            return (StatusCode::PAYMENT_REQUIRED, axum::Json(body)).into_response();
        }

        if let AppError::BlobStore(BlobStoreError::PayloadTooLarge {
            unencoded_size,
            n_shards,
            max_unencoded_for_network,
        }) = &self
        {
            metrics::counter!(
                crate::metrics::PAYLOAD_TOO_LARGE_RESPONSES_TOTAL,
                "reason" => "encoder_ceiling",
            )
            .increment(1);
            tracing::warn!(
                unencoded_size,
                n_shards,
                max_unencoded_for_network,
                "payload too large: encoder ceiling exceeded",
            );
            let body = serde_json::json!({
                "error": format!(
                    "payload too large: unencoded_size {unencoded_size} exceeds \
                     per-network max_unencoded_bytes_for_network {max_unencoded_for_network}"
                ),
                "payload_too_large": {
                    "unencoded_size_bytes": unencoded_size,
                    "n_shards": n_shards,
                    "max_unencoded_bytes_for_network": max_unencoded_for_network,
                },
            });
            return (StatusCode::PAYLOAD_TOO_LARGE, axum::Json(body)).into_response();
        }

        // CapExceeded carries a structured `cap_exceeded` block, so it
        // is also handled up-front rather than through the generic
        // `(status, message)` tuple.
        if let AppError::BlobStore(BlobStoreError::CapExceeded {
            message,
            max_unencoded_bytes,
            used_encoded_bytes,
            new_unencoded_bytes,
        }) = &self
        {
            tracing::warn!(
                error = %message,
                max_unencoded_bytes,
                used_encoded_bytes,
                new_unencoded_bytes,
                "storage cap exceeded",
            );
            let body = serde_json::json!({
                "error": format!("storage cap exceeded: {message}"),
                "cap_exceeded": {
                    "max_unencoded_bytes": max_unencoded_bytes,
                    "used_encoded_bytes": used_encoded_bytes,
                    "new_unencoded_bytes": new_unencoded_bytes,
                    "admin_endpoint": "PUT /api/v1/accounts/{account_id}/max-storage",
                },
            });
            return (StatusCode::BAD_REQUEST, axum::Json(body)).into_response();
        }

        // MaxStorageWouldOrphan and MaxStorageShrinkAborted carry
        // structured blocks similar to CapExceeded — handled up-front
        // so the generic `(status, message)` arm doesn't strip them.
        if let AppError::MaxStorageWouldOrphan {
            max_unencoded_bytes,
            used_encoded_bytes,
            threshold_encoded,
        } = &self
        {
            tracing::warn!(
                max_unencoded_bytes,
                used_encoded_bytes,
                threshold_encoded,
                "max-storage update would orphan stored data",
            );
            let body = serde_json::json!({
                "error": self.to_string(),
                "would_orphan": {
                    "max_unencoded_bytes": max_unencoded_bytes,
                    "used_encoded_bytes": used_encoded_bytes,
                    "threshold_encoded": threshold_encoded,
                },
            });
            return (StatusCode::BAD_REQUEST, axum::Json(body)).into_response();
        }

        if let AppError::MaxStorageShrinkAborted {
            description,
            max_unencoded_bytes,
            extract_size,
        } = &self
        {
            tracing::warn!(
                max_unencoded_bytes,
                extract_size,
                description = %description,
                "max-storage shrink PTB aborted",
            );
            let body = serde_json::json!({
                "error": self.to_string(),
                "shrink_aborted": {
                    "move_abort_description": description,
                    "max_unencoded_bytes": max_unencoded_bytes,
                    "extract_size": extract_size,
                },
            });
            return (StatusCode::BAD_REQUEST, axum::Json(body)).into_response();
        }

        let (status, message) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::Forbidden(_) => (StatusCode::FORBIDDEN, self.to_string()),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Conflict(_) => (StatusCode::CONFLICT, self.to_string()),
            AppError::ServiceUnavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            AppError::NotImplemented => (StatusCode::NOT_IMPLEMENTED, self.to_string()),
            AppError::PayloadTooLarge => {
                metrics::counter!(
                    crate::metrics::PAYLOAD_TOO_LARGE_RESPONSES_TOTAL,
                    "reason" => "body_limit",
                )
                .increment(1);
                (StatusCode::PAYLOAD_TOO_LARGE, self.to_string())
            }
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
            AppError::MaxStorageWouldOrphan { .. } => {
                unreachable!(
                    "MaxStorageWouldOrphan handled earlier so we can attach would_orphan block"
                )
            }
            AppError::MaxStorageShrinkAborted { .. } => {
                unreachable!(
                    "MaxStorageShrinkAborted handled earlier so we can attach shrink_aborted block"
                )
            }
            AppError::BlobStore(e) => match e {
                BlobStoreError::InsufficientBalance { .. } => unreachable!(
                    "InsufficientBalance handled earlier so we can attach funding_required"
                ),
                BlobStoreError::CapExceeded { .. } => {
                    unreachable!("CapExceeded handled earlier so we can attach cap_exceeded block")
                }
                BlobStoreError::PayloadTooLarge { .. } => unreachable!(
                    "PayloadTooLarge handled earlier so we can attach payload_too_large block"
                ),
                BlobStoreError::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
                BlobStoreError::InvalidBlobId(_) => (StatusCode::BAD_REQUEST, self.to_string()),
                BlobStoreError::PoolCreationFailed(msg) => {
                    tracing::error!(error = %msg, "pool creation failed");
                    (
                        StatusCode::BAD_GATEWAY,
                        format!(
                            "pool creation failed [BlobStoreError::{}]",
                            e.variant_name()
                        ),
                    )
                }
                BlobStoreError::Unreachable(msg) => {
                    tracing::error!(error = %msg, "upstream blob store unreachable");
                    (
                        StatusCode::BAD_GATEWAY,
                        format!(
                            "upstream blob store unreachable [BlobStoreError::{}]",
                            e.variant_name(),
                        ),
                    )
                }
                BlobStoreError::Upstream(msg) => {
                    tracing::error!(error = %msg, "upstream blob store error");
                    (
                        StatusCode::BAD_GATEWAY,
                        format!(
                            "upstream blob store error [BlobStoreError::{}]",
                            e.variant_name(),
                        ),
                    )
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
                        (
                            StatusCode::BAD_GATEWAY,
                            format!(
                                "upstream blob store error [BlobStoreError::{}]",
                                e.variant_name(),
                            ),
                        )
                    }
                }
                BlobStoreError::Internal(msg) => {
                    tracing::error!(error = %msg, "blob store internal error");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("internal error [BlobStoreError::{}]", e.variant_name()),
                    )
                }
                BlobStoreError::Io(io_err) => {
                    tracing::error!(error = %io_err, "blob store I/O error");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("internal error [BlobStoreError::{}]", e.variant_name()),
                    )
                }
                BlobStoreError::Database(db_err) => {
                    tracing::error!(error = %db_err, "blob store database error");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("internal error [BlobStoreError::{}]", e.variant_name()),
                    )
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
    use crate::{FundingAmount, blob_store::BlobStoreError};

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
            operation: "store_blob",
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
            operation: "store_blob",
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
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body = read_json_body(resp).await;
        let msg = body["error"].as_str().expect("error field present");
        assert!(msg.contains("[BlobStoreError::Upstream]"), "msg = {msg}");
        assert!(
            !msg.contains("walrus 500"),
            "raw upstream text must not leak: {msg}",
        );
    }

    #[tokio::test]
    async fn internal_blob_store_maps_to_500() {
        let err = AppError::BlobStore(BlobStoreError::Internal("invariant".into()));
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = read_json_body(resp).await;
        let msg = body["error"].as_str().expect("error field present");
        assert!(msg.contains("[BlobStoreError::Internal]"), "msg = {msg}");
        assert!(
            !msg.contains("invariant"),
            "raw internal text must not leak: {msg}",
        );
    }

    #[tokio::test]
    async fn pool_creation_failed_maps_to_502() {
        let err = AppError::BlobStore(BlobStoreError::PoolCreationFailed("boom".into()));
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body = read_json_body(resp).await;
        let msg = body["error"].as_str().expect("error field present");
        assert!(
            msg.contains("[BlobStoreError::PoolCreationFailed]"),
            "msg = {msg}",
        );
        assert!(!msg.contains("boom"), "raw text must not leak: {msg}");
    }

    #[tokio::test]
    async fn upstream_status_passthrough_4xx() {
        let err = AppError::BlobStore(BlobStoreError::UpstreamStatus {
            status: 400,
            message: "bad upstream req".into(),
        });
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // 4xx is passthrough — message is intentionally surfaced and not variant-tagged.
        let body = read_json_body(resp).await;
        let msg = body["error"].as_str().expect("error field present");
        assert!(
            !msg.contains("[BlobStoreError::"),
            "4xx passthrough should not be tagged: {msg}",
        );
        assert!(msg.contains("bad upstream req"), "msg = {msg}");
    }

    #[tokio::test]
    async fn upstream_status_masks_5xx_to_502() {
        let err = AppError::BlobStore(BlobStoreError::UpstreamStatus {
            status: 503,
            message: "upstream boom".into(),
        });
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body = read_json_body(resp).await;
        let msg = body["error"].as_str().expect("error field present");
        assert!(
            msg.contains("[BlobStoreError::UpstreamStatus]"),
            "msg = {msg}",
        );
        assert!(
            !msg.contains("upstream boom"),
            "raw upstream text must not leak: {msg}",
        );
    }

    #[tokio::test]
    async fn unreachable_maps_to_502() {
        let err = AppError::BlobStore(BlobStoreError::Unreachable("conn refused".into()));
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body = read_json_body(resp).await;
        let msg = body["error"].as_str().expect("error field present");
        assert!(msg.contains("[BlobStoreError::Unreachable]"), "msg = {msg}");
        assert!(
            !msg.contains("conn refused"),
            "raw upstream text must not leak: {msg}",
        );
    }

    #[tokio::test]
    async fn blob_store_io_maps_to_500_with_variant_suffix() {
        let err = AppError::BlobStore(BlobStoreError::Io(std::io::Error::other("disk gone")));
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = read_json_body(resp).await;
        let msg = body["error"].as_str().expect("error field present");
        assert!(msg.contains("[BlobStoreError::Io]"), "msg = {msg}");
        assert!(!msg.contains("disk gone"), "raw text must not leak: {msg}");
    }

    #[test]
    fn variant_name_covers_all_variants() {
        // If a new BlobStoreError variant is added, this test forces the
        // author to extend variant_name(); a wildcard match would silently
        // fall through and emit a misleading suffix on the wire.
        assert_eq!(
            BlobStoreError::NotFound("x".into()).variant_name(),
            "NotFound",
        );
        assert_eq!(
            BlobStoreError::InvalidBlobId("x".into()).variant_name(),
            "InvalidBlobId",
        );
        assert_eq!(
            BlobStoreError::Upstream("x".into()).variant_name(),
            "Upstream",
        );
        assert_eq!(
            BlobStoreError::Unreachable("x".into()).variant_name(),
            "Unreachable",
        );
        assert_eq!(
            BlobStoreError::UpstreamStatus {
                status: 500,
                message: "x".into(),
            }
            .variant_name(),
            "UpstreamStatus",
        );
        assert_eq!(
            BlobStoreError::PoolCreationFailed("x".into()).variant_name(),
            "PoolCreationFailed",
        );
        assert_eq!(
            BlobStoreError::Internal("x".into()).variant_name(),
            "Internal",
        );
        assert_eq!(
            BlobStoreError::Io(std::io::Error::other("x")).variant_name(),
            "Io",
        );
    }

    #[tokio::test]
    async fn max_storage_would_orphan_maps_to_400_with_block() {
        let err = AppError::MaxStorageWouldOrphan {
            max_unencoded_bytes: 1_000,
            used_encoded_bytes: 9_999,
            threshold_encoded: 5_000,
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = read_json_body(resp).await;
        let block = &body["would_orphan"];
        assert!(!block.is_null(), "missing would_orphan block: {body}");
        assert_eq!(block["max_unencoded_bytes"].as_i64(), Some(1_000));
        assert_eq!(block["used_encoded_bytes"].as_i64(), Some(9_999));
        assert_eq!(block["threshold_encoded"].as_i64(), Some(5_000));
    }

    #[tokio::test]
    async fn payload_too_large_maps_to_413_with_block() {
        let err = AppError::BlobStore(BlobStoreError::PayloadTooLarge {
            unencoded_size: 10_000_000,
            n_shards: 10,
            max_unencoded_for_network: 1_500_000,
        });
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = read_json_body(resp).await;
        let block = &body["payload_too_large"];
        assert!(!block.is_null(), "missing payload_too_large block: {body}");
        assert_eq!(block["unencoded_size_bytes"].as_u64(), Some(10_000_000));
        assert_eq!(block["n_shards"].as_u64(), Some(10));
        assert_eq!(
            block["max_unencoded_bytes_for_network"].as_u64(),
            Some(1_500_000),
        );
    }

    #[tokio::test]
    async fn max_storage_shrink_aborted_maps_to_400_with_block() {
        let err = AppError::MaxStorageShrinkAborted {
            description: "Move Runtime Abort. Abort Code: 6".into(),
            max_unencoded_bytes: 2_000,
            extract_size: 4_096,
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = read_json_body(resp).await;
        let block = &body["shrink_aborted"];
        assert!(!block.is_null(), "missing shrink_aborted block: {body}");
        assert_eq!(
            block["move_abort_description"].as_str(),
            Some("Move Runtime Abort. Abort Code: 6"),
        );
        assert_eq!(block["max_unencoded_bytes"].as_i64(), Some(2_000));
        assert_eq!(block["extract_size"].as_i64(), Some(4_096));
    }

    /// Synthesise a real SQLite FK violation by inserting a `blobs` row
    /// whose `bucket_name` references a bucket that doesn't exist. The
    /// SQLite driver surfaces this as a `sqlx::Error::Database` whose
    /// message contains `"FOREIGN KEY constraint failed"` — which is
    /// exactly the substring `is_foreign_key_violation` looks for. The
    /// Postgres SQLSTATE `23503` branch is exercised end-to-end in
    /// production but not directly here, since `DatabaseError` is a
    /// trait and there's no public constructor for a fake one.
    #[tokio::test]
    async fn is_foreign_key_violation_detects_sqlite_message() {
        let pool = crate::db::create_pool("sqlite::memory:")
            .await
            .expect("pool");
        // The migrations create the `blobs` table with a FK on
        // `bucket_name`. Inserting a row that points at no-such-bucket
        // triggers SQLite's "FOREIGN KEY constraint failed" message.
        let result = sqlx::query(&crate::db::sql(
            "INSERT INTO blobs \
             (key, blob_id, bucket_name, account_id, content_type, size, md5) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        ))
        .bind("k")
        .bind("b")
        .bind("does-not-exist")
        .bind("nope")
        .bind("application/octet-stream")
        .bind(0i64)
        .bind("d41d8cd98f00b204e9800998ecf8427e")
        .execute(&pool)
        .await;
        let err = result.expect_err("expected FK violation");
        assert!(
            is_foreign_key_violation(&err),
            "expected FK violation detection to fire on: {err:?}",
        );
    }

    #[test]
    fn is_foreign_key_violation_returns_false_for_non_db_error() {
        let err = sqlx::Error::RowNotFound;
        assert!(!is_foreign_key_violation(&err));
    }
}
