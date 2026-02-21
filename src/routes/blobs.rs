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
    blob_store::{BlobId, BlobStore},
    db,
    error::AppError,
    models::{
        BlobMetadata,
        PaginatedResponse,
        PaginationParams,
        StoreBlobResponse,
        UpdateBlobMetadataRequest,
    },
    pagination,
};

const MAX_BLOB_SIZE: usize = 1_073_741_824; // 1 GB
const DEFAULT_DURATION_DAYS: i64 = 30;

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

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");

    let blob_id = state.blob_store.store(&body).await?;

    let expires_at = chrono::Utc::now()
        .checked_add_days(chrono::Days::new(DEFAULT_DURATION_DAYS as u64))
        .expect("valid date")
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    let metadata = db::blobs::insert_blob(
        &state.db,
        blob_id.as_str(),
        &bucket_id,
        &auth.account_id,
        content_type,
        body.len() as i64,
        &expires_at,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(StoreBlobResponse {
            object_id: metadata.object_id,
            blob_id: metadata.blob_id,
            size: metadata.size,
            created_at: metadata.created_at,
            expires_at: metadata.expires_at,
        }),
    ))
}

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
