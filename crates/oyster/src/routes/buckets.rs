use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};

use crate::{
    AppState,
    auth::AuthenticatedAccount,
    db,
    error::AppError,
    models::{Bucket, CreateBucketRequest, ErrorResponse, PaginatedResponse, PaginationParams},
    pagination,
    validation,
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
/// Create a new bucket. Bucket names must be globally unique.
pub async fn create_bucket(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Json(body): Json<CreateBucketRequest>,
) -> Result<(StatusCode, Json<Bucket>), AppError> {
    validation::validate_bucket_name(&body.name).map_err(AppError::BadRequest)?;

    let bucket = db::buckets::create_bucket(&state.db, &auth.account_id, &body.name)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                let is_unique_violation = db_err.code().is_some_and(|c| c == "23505")
                    || db_err.message().contains("UNIQUE constraint failed");
                if is_unique_violation {
                    return AppError::Conflict(format!(
                        "bucket with name '{}' already exists",
                        body.name
                    ));
                }
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
    let limit = pagination::clamp_limit(params.limit)?;
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
            .map(|b| pagination::encode_cursor(&b.created_at, &b.name))
    } else {
        None
    };

    Ok(Json(PaginatedResponse { data, next_cursor }))
}

#[utoipa::path(
    delete,
    path = "/buckets/{bucket_name}",
    tag = "Buckets",
    security(("bearer" = [])),
    params(("bucket_name" = String, Path, description = "Bucket name")),
    responses(
        (status = 204, description = "Bucket deleted"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Bucket not found", body = ErrorResponse),
        (status = 409, description = "Bucket is not empty", body = ErrorResponse),
    ),
)]
/// Delete an empty bucket. Returns 409 Conflict if the bucket still contains blobs.
pub async fn delete_bucket(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path(bucket_name): Path<String>,
) -> Result<StatusCode, AppError> {
    // Ownership check first so a caller who does not own the bucket cannot
    // distinguish "bucket exists but is non-empty" (409) from "bucket does not
    // exist for me" (404).
    if db::buckets::get_bucket(&state.db, &bucket_name, &auth.account_id)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound);
    }

    let blob_count = db::blobs::count_blobs_in_bucket(&state.db, &bucket_name).await?;
    if blob_count > 0 {
        return Err(AppError::Conflict("bucket is not empty".to_string()));
    }

    let deleted = db::buckets::delete_bucket(&state.db, &bucket_name, &auth.account_id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}
