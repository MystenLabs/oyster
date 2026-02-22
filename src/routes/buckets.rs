use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};

use crate::{
    AppState,
    auth::AuthenticatedAccount,
    blob_store::BlobStore,
    db,
    error::AppError,
    models::{Bucket, CreateBucketRequest, ErrorResponse, PaginatedResponse, PaginationParams},
    pagination,
};

#[utoipa::path(
    post,
    path = "/buckets",
    tag = "Buckets",
    security(("bearer" = [])),
    request_body = CreateBucketRequest,
    responses(
        (status = 201, description = "Bucket created", body = Bucket),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 409, description = "Bucket name already exists", body = ErrorResponse),
    ),
)]
/// Create a new bucket. Bucket names must be unique within an account.
pub async fn create_bucket(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Json(body): Json<CreateBucketRequest>,
) -> Result<(StatusCode, Json<Bucket>), AppError> {
    if body.name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }

    let bucket = db::buckets::create_bucket(&state.db, &auth.account_id, &body.name)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e
                && db_err.message().contains("UNIQUE constraint failed")
            {
                return AppError::Conflict(format!(
                    "bucket with name '{}' already exists",
                    body.name
                ));
            }
            AppError::Database(e)
        })?;

    Ok((StatusCode::CREATED, Json(bucket)))
}

#[utoipa::path(
    get,
    path = "/buckets",
    tag = "Buckets",
    security(("bearer" = [])),
    params(PaginationParams),
    responses(
        (status = 200, description = "List of buckets", body = PaginatedResponse<Bucket>),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// List all buckets owned by the authenticated account, with cursor-based pagination.
pub async fn list_buckets(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<Bucket>>, AppError> {
    let limit = pagination::clamp_limit(params.limit);
    let cursor_data = params
        .cursor
        .as_deref()
        .map(pagination::decode_cursor)
        .transpose()?;

    let buckets = db::buckets::list_buckets(
        &state.db,
        &auth.account_id,
        cursor_data.as_ref().map(|c| c.created_at.as_str()),
        cursor_data.as_ref().map(|c| c.id.as_str()),
        limit + 1, // fetch one extra to determine if there's a next page
    )
    .await?;

    let has_more = buckets.len() as i64 > limit;
    let data: Vec<Bucket> = buckets.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        data.last()
            .map(|b| pagination::encode_cursor(&b.created_at, &b.id))
    } else {
        None
    };

    Ok(Json(PaginatedResponse { data, next_cursor }))
}

#[utoipa::path(
    delete,
    path = "/buckets/{bucket_id}",
    tag = "Buckets",
    security(("bearer" = [])),
    params(("bucket_id" = String, Path, description = "Bucket ID")),
    responses(
        (status = 204, description = "Bucket deleted"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Bucket not found", body = ErrorResponse),
    ),
)]
/// Delete a bucket and all blobs it contains. Unreferenced blob data is cleaned up from storage.
pub async fn delete_bucket(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path(bucket_id): Path<String>,
) -> Result<StatusCode, AppError> {
    // First delete all blobs in the bucket
    let blob_ids = db::blobs::delete_blobs_in_bucket(&state.db, &bucket_id).await?;

    // Clean up unreferenced blob data from the store
    for blob_id in blob_ids {
        let count = db::blobs::count_references(&state.db, &blob_id).await?;
        if count == 0 {
            let _ = state
                .blob_store
                .delete(&crate::blob_store::BlobId(blob_id))
                .await;
        }
    }

    let deleted = db::buckets::delete_bucket(&state.db, &bucket_id, &auth.account_id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}
