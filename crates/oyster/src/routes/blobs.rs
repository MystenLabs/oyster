use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

use crate::{
    AppState,
    auth::AuthenticatedAccount,
    blob_store::BlobId,
    db,
    error::AppError,
    models::{
        BlobMetadata,
        ErrorResponse,
        PaginatedResponse,
        PaginationParams,
        StoreBlobResponse,
        UpdateBlobMetadataRequest,
    },
    pagination,
};

const MAX_BLOB_SIZE: usize = 1_073_741_824; // 1 GB
const DEFAULT_DURATION_DAYS: i64 = 30;

#[utoipa::path(
    put,
    path = "/buckets/{bucket_id}/blobs",
    tag = "Blobs",
    security(("bearer" = [])),
    params(("bucket_id" = String, Path, description = "Bucket ID")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses(
        (status = 201, description = "Blob stored", body = StoreBlobResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Bucket not found", body = ErrorResponse),
        (status = 413, description = "Payload too large", body = ErrorResponse),
    ),
)]
/// Upload a blob into a bucket. The request body is the raw binary content. Content is deduplicated by hash.
pub async fn store_blob(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path(bucket_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<StoreBlobResponse>), AppError> {
    if body.len() > MAX_BLOB_SIZE {
        return Err(AppError::PayloadTooLarge);
    }

    // Verify bucket exists and belongs to the account
    let _bucket = db::buckets::get_bucket(&state.db, &bucket_id, &auth.account_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let account = db::accounts::get_account(&state.db, &auth.account_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");

    let result = state
        .blob_store
        .store(&body, account.pearl_account_id.as_deref())
        .await?;

    let expires_at = chrono::Utc::now()
        .checked_add_days(chrono::Days::new(DEFAULT_DURATION_DAYS as u64))
        .expect("valid date")
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    let metadata = db::blobs::insert_blob(
        &state.db,
        result.blob_id.as_str(),
        &bucket_id,
        &auth.account_id,
        content_type,
        body.len() as i64,
        &expires_at,
        result.sui_object_id.as_deref(),
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(StoreBlobResponse {
            object_id: metadata.object_id,
            blob_id: metadata.blob_id,
            size: metadata.size,
            sui_object_id: metadata.sui_object_id,
            created_at: metadata.created_at,
            expires_at: metadata.expires_at,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/buckets/{bucket_id}/blobs",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_id" = String, Path, description = "Bucket ID"),
        PaginationParams,
    ),
    responses(
        (status = 200, description = "List of blobs", body = PaginatedResponse<BlobMetadata>),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// List all blobs in a bucket, with cursor-based pagination.
pub async fn list_blobs(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path(bucket_id): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<BlobMetadata>>, AppError> {
    let limit = pagination::clamp_limit(params.limit);
    let cursor_data = params
        .cursor
        .as_deref()
        .map(pagination::decode_cursor)
        .transpose()?;

    let blobs = db::blobs::list_blobs_in_bucket(
        &state.db,
        &bucket_id,
        &auth.account_id,
        cursor_data.as_ref().map(|c| c.created_at.as_str()),
        cursor_data.as_ref().map(|c| c.id.as_str()),
        limit + 1,
    )
    .await?;

    let has_more = blobs.len() as i64 > limit;
    let data: Vec<BlobMetadata> = blobs.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        data.last()
            .map(|b| pagination::encode_cursor(&b.created_at, &b.object_id))
    } else {
        None
    };

    Ok(Json(PaginatedResponse { data, next_cursor }))
}

#[utoipa::path(
    get,
    path = "/blobs/{object_id}",
    tag = "Blobs",
    params(("object_id" = String, Path, description = "Object ID")),
    responses(
        (status = 200, description = "Blob data", content_type = "application/octet-stream"),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Read a blob's content by its object ID. No authentication required.
pub async fn read_blob(
    State(state): State<AppState>,
    Path(object_id): Path<String>,
) -> Result<Response, AppError> {
    let metadata = db::blobs::get_blob_by_object_id(&state.db, &object_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let data = state.blob_store.read(&BlobId(metadata.blob_id)).await?;

    Ok((
        StatusCode::OK,
        [("content-type", metadata.content_type.as_str())],
        data,
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/blobs/by-blob-id/{blob_id}",
    tag = "Blobs",
    params(("blob_id" = String, Path, description = "Blob content-hash ID")),
    responses(
        (status = 200, description = "Blob data", content_type = "application/octet-stream"),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Read a blob's content by its content-addressed blob ID. No authentication required.
pub async fn read_blob_by_blob_id(
    State(state): State<AppState>,
    Path(blob_id): Path<String>,
) -> Result<Response, AppError> {
    let exists = state.blob_store.exists(&BlobId(blob_id.clone())).await?;
    if !exists {
        return Err(AppError::NotFound);
    }

    let data = state.blob_store.read(&BlobId(blob_id)).await?;

    Ok((
        StatusCode::OK,
        [("content-type", "application/octet-stream")],
        data,
    )
        .into_response())
}

#[utoipa::path(
    patch,
    path = "/blobs/{object_id}/metadata",
    tag = "Blobs",
    security(("bearer" = [])),
    params(("object_id" = String, Path, description = "Object ID")),
    request_body = UpdateBlobMetadataRequest,
    responses(
        (status = 200, description = "Metadata updated", body = BlobMetadata),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Update a blob's metadata (content type or auto-extend duration). At least one field must be provided.
pub async fn update_blob_metadata(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path(object_id): Path<String>,
    Json(body): Json<UpdateBlobMetadataRequest>,
) -> Result<Json<BlobMetadata>, AppError> {
    if body.content_type.is_none() && body.auto_extend_duration.is_none() {
        return Err(AppError::BadRequest(
            "at least one field must be provided".into(),
        ));
    }

    let metadata = db::blobs::update_blob_metadata(
        &state.db,
        &object_id,
        &auth.account_id,
        body.content_type.as_deref(),
        body.auto_extend_duration.as_deref(),
    )
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(metadata))
}

#[utoipa::path(
    delete,
    path = "/blobs/{object_id}",
    tag = "Blobs",
    security(("bearer" = [])),
    params(("object_id" = String, Path, description = "Object ID")),
    responses(
        (status = 204, description = "Blob deleted"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Delete a blob by its object ID. The underlying data is only removed when no other objects reference it.
pub async fn delete_blob(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path(object_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let blob_id = db::blobs::delete_blob(&state.db, &object_id, &auth.account_id)
        .await?
        .ok_or(AppError::NotFound)?;

    // Reference-counted deletion: only delete from store if no more references
    let count = db::blobs::count_references(&state.db, &blob_id).await?;
    if count == 0 {
        let _ = state.blob_store.delete(&BlobId(blob_id)).await;
    }

    Ok(StatusCode::NO_CONTENT)
}
