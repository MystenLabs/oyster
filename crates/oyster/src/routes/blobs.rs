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
        BlobTagsResponse,
        ErrorResponse,
        PaginatedResponse,
        PaginationParams,
        PatchTagsRequest,
        PutTagsRequest,
        StoreBlobResponse,
        UpdateBlobMetadataRequest,
    },
    pagination,
    validation,
};

const MAX_BLOB_SIZE: usize = 1_073_741_824; // 1 GB

/// Parse all `x-oyster-tag` header values into a tag list.
///
/// Each header value must be exactly one `key=value` pair (no `&`-joined
/// sets). Keys and values are percent-decoded via `form_urlencoded`.
fn parse_oyster_tag_headers(headers: &HeaderMap) -> Result<Vec<(String, String)>, AppError> {
    let mut tags = Vec::new();
    for value in headers.get_all("x-oyster-tag") {
        let raw = value
            .to_str()
            .map_err(|_| AppError::BadRequest("invalid x-oyster-tag header".into()))?;
        if raw.contains('&') {
            return Err(AppError::BadRequest(
                "x-oyster-tag must contain a single key=value pair (no '&')".into(),
            ));
        }
        let pairs: Vec<(String, String)> = form_urlencoded::parse(raw.as_bytes())
            .into_owned()
            .collect();
        match pairs.as_slice() {
            [(k, v)] if !k.is_empty() => tags.push((k.clone(), v.clone())),
            _ => {
                return Err(AppError::BadRequest(
                    "x-oyster-tag must be a single non-empty key=value pair".into(),
                ));
            }
        }
    }
    Ok(tags)
}

/// Check If-Match / If-None-Match headers against the current md5.
fn check_json_conditions(
    headers: &HeaderMap,
    current_md5: Option<&str>,
    is_safe_method: bool,
) -> Result<(), AppError> {
    if let Some(value) = headers.get("if-match") {
        let value = value
            .to_str()
            .map_err(|_| AppError::BadRequest("invalid If-Match header".into()))?;
        if value == "*" {
            if current_md5.is_none() {
                return Err(AppError::PreconditionFailed);
            }
        } else {
            let stripped = value.trim_matches('"');
            match current_md5 {
                Some(current) if current == stripped => {}
                _ => return Err(AppError::PreconditionFailed),
            }
        }
    }
    if let Some(value) = headers.get("if-none-match") {
        let value = value
            .to_str()
            .map_err(|_| AppError::BadRequest("invalid If-None-Match header".into()))?;
        if value == "*" {
            if current_md5.is_some() {
                return if is_safe_method {
                    Err(AppError::NotModified)
                } else {
                    Err(AppError::PreconditionFailed)
                };
            }
        } else {
            let stripped = value.trim_matches('"');
            if let Some(current) = current_md5
                && current == stripped
            {
                return if is_safe_method {
                    Err(AppError::NotModified)
                } else {
                    Err(AppError::PreconditionFailed)
                };
            }
        }
    }
    Ok(())
}

#[utoipa::path(
    put,
    path = "/buckets/{bucket_name}/blobs/{key}",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        ("key" = String, Path, description = "Object key"),
    ),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses(
        (status = 201, description = "Blob stored", body = StoreBlobResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Bucket not found", body = ErrorResponse),
        (status = 413, description = "Payload too large", body = ErrorResponse),
    ),
)]
/// Upload a blob into a bucket at the given key. The request body is the raw binary content. Uploading to the same key overwrites.
pub async fn store_blob(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path((bucket_name, key)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    if body.len() > MAX_BLOB_SIZE {
        return Err(AppError::PayloadTooLarge);
    }

    // Parse + validate initial tags early so we fail fast without a store round-trip.
    let initial_tags = parse_oyster_tag_headers(&headers)?;
    validation::validate_tag_set(&initial_tags).map_err(AppError::BadRequest)?;

    // Verify bucket exists and belongs to the account
    let _bucket = db::buckets::get_bucket(&state.db, &bucket_name, &auth.account_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let existing = db::blobs::get_blob_by_key(&state.db, &bucket_name, &key).await?;
    check_json_conditions(&headers, existing.as_ref().map(|m| m.md5.as_str()), false)?;
    tracing::debug!(
        "bucket {} found for account {}",
        bucket_name,
        auth.account_id
    );

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");
    tracing::debug!(
        "storing blob for account {}, content-type: {}",
        auth.account_id,
        content_type
    );

    let md5_digest = format!("{:x}", md5::compute(&body));

    let result = match state.blob_store.store(&body, &auth.account_id).await {
        Ok(r) => {
            metrics::counter!(crate::metrics::BLOB_STORE_OPS_TOTAL,
                "operation" => "store", "result" => "ok"
            )
            .increment(1);
            r
        }
        Err(e) => {
            metrics::counter!(crate::metrics::BLOB_STORE_OPS_TOTAL,
                "operation" => "store", "result" => "error"
            )
            .increment(1);
            return Err(e.into());
        }
    };

    let metadata = db::blobs::insert_blob(
        &state.db,
        &key,
        result.blob_id.as_str(),
        &bucket_name,
        &auth.account_id,
        content_type,
        body.len() as i64,
        &md5_digest,
        result.pooled_blob_object_id.as_deref(),
        result.encoded_size.map(|e| e as i64),
    )
    .await?;

    // Replace tags on every PUT (including overwrite) so stale tags don't leak.
    db::blob_tags::replace_all_tags(
        &state.db,
        &auth.account_id,
        &bucket_name,
        &key,
        &initial_tags,
    )
    .await?;

    let etag = format!("\"{}\"", metadata.md5);
    Ok((
        StatusCode::CREATED,
        [("etag", etag)],
        Json(StoreBlobResponse {
            key: metadata.key,
            blob_id: metadata.blob_id,
            size: metadata.size,
            md5: metadata.md5,
            pooled_blob_object_id: metadata.pooled_blob_object_id,
            created_at: metadata.created_at,
        }),
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/buckets/{bucket_name}/blobs",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        PaginationParams,
    ),
    responses(
        (status = 200, description = "List of blobs", body = PaginatedResponse<BlobMetadata>),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Bucket not found", body = ErrorResponse),
    ),
)]
/// List all blobs in a bucket, with cursor-based pagination.
pub async fn list_blobs(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path(bucket_name): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<BlobMetadata>>, AppError> {
    db::buckets::get_bucket(&state.db, &bucket_name, &auth.account_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let limit = pagination::clamp_limit(params.limit)?;
    let cursor_data = params
        .cursor
        .as_deref()
        .map(pagination::decode_cursor)
        .transpose()?;

    let blobs = db::blobs::list_blobs_in_bucket(
        &state.db,
        &bucket_name,
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
            .map(|b| pagination::encode_cursor(&b.created_at, &b.key))
    } else {
        None
    };

    Ok(Json(PaginatedResponse { data, next_cursor }))
}

#[utoipa::path(
    get,
    path = "/buckets/{bucket_name}/blobs/{key}",
    tag = "Blobs",
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        ("key" = String, Path, description = "Object key"),
    ),
    responses(
        (status = 200, description = "Blob data", content_type = "application/octet-stream"),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Read a blob's content by bucket name and key. No authentication required.
pub async fn read_blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((bucket_name, key)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let metadata = db::blobs::get_blob_by_key(&state.db, &bucket_name, &key)
        .await?
        .ok_or(AppError::NotFound)?;

    check_json_conditions(&headers, Some(&metadata.md5), true)?;

    let data = match state.blob_store.read(&BlobId(metadata.blob_id)).await {
        Ok(d) => {
            metrics::counter!(crate::metrics::BLOB_STORE_OPS_TOTAL,
                "operation" => "read", "result" => "ok"
            )
            .increment(1);
            d
        }
        Err(e) => {
            metrics::counter!(crate::metrics::BLOB_STORE_OPS_TOTAL,
                "operation" => "read", "result" => "error"
            )
            .increment(1);
            return Err(e.into());
        }
    };

    let etag = format!("\"{}\"", metadata.md5);
    Ok((
        StatusCode::OK,
        [
            ("content-type", metadata.content_type.as_str()),
            ("etag", etag.as_str()),
            ("x-content-type-options", "nosniff"),
        ],
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
    let data = match state.blob_store.read(&BlobId(blob_id)).await {
        Ok(d) => {
            metrics::counter!(crate::metrics::BLOB_STORE_OPS_TOTAL,
                "operation" => "read", "result" => "ok"
            )
            .increment(1);
            d
        }
        Err(e) => {
            metrics::counter!(crate::metrics::BLOB_STORE_OPS_TOTAL,
                "operation" => "read", "result" => "error"
            )
            .increment(1);
            return Err(e.into());
        }
    };

    Ok((
        StatusCode::OK,
        [
            ("content-type", "application/octet-stream"),
            ("x-content-type-options", "nosniff"),
        ],
        data,
    )
        .into_response())
}

#[utoipa::path(
    patch,
    path = "/buckets/{bucket_name}/blobs/{key}/metadata",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        ("key" = String, Path, description = "Object key"),
    ),
    request_body = UpdateBlobMetadataRequest,
    responses(
        (status = 200, description = "Metadata updated", body = BlobMetadata),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Update a blob's content type.
pub async fn update_blob_metadata(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path((bucket_name, key)): Path<(String, String)>,
    Json(body): Json<UpdateBlobMetadataRequest>,
) -> Result<Json<BlobMetadata>, AppError> {
    let content_type = body
        .content_type
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("content_type must be provided".into()))?;

    let metadata = db::blobs::update_blob_metadata(
        &state.db,
        &bucket_name,
        &key,
        &auth.account_id,
        content_type,
    )
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(metadata))
}

#[utoipa::path(
    delete,
    path = "/buckets/{bucket_name}/blobs/{key}",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        ("key" = String, Path, description = "Object key"),
    ),
    responses(
        (status = 204, description = "Blob deleted"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Delete a blob by its bucket name and key. The underlying data is only removed when no other objects reference it.
pub async fn delete_blob(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path((bucket_name, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let existing = db::blobs::get_blob_by_key(&state.db, &bucket_name, &key).await?;
    check_json_conditions(&headers, existing.as_ref().map(|m| m.md5.as_str()), false)?;

    let info = db::blobs::delete_blob(&state.db, &bucket_name, &key, &auth.account_id)
        .await?
        .ok_or(AppError::NotFound)?;

    // Reference-counted deletion: only delete from store if no more references
    let count = db::blobs::count_references(&state.db, &info.blob_id).await?;
    if count == 0 {
        let pool_id = db::accounts::get_storage_pool(&state.db, &auth.account_id)
            .await?
            .map(|s| s.object_id);
        match state
            .blob_store
            .delete(
                &BlobId(info.blob_id),
                pool_id.as_deref(),
                info.encoded_size.unwrap_or(0) as u64,
                &auth.account_id,
            )
            .await
        {
            Ok(()) => {
                metrics::counter!(crate::metrics::BLOB_STORE_OPS_TOTAL,
                    "operation" => "delete", "result" => "ok"
                )
                .increment(1);
            }
            Err(_) => {
                metrics::counter!(crate::metrics::BLOB_STORE_OPS_TOTAL,
                    "operation" => "delete", "result" => "error"
                )
                .increment(1);
            }
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Ensure a blob exists for the account; returns `AppError::NotFound` otherwise.
async fn ensure_blob_exists(
    state: &AppState,
    account_id: &crate::AccountId,
    bucket_name: &str,
    key: &str,
) -> Result<(), AppError> {
    let metadata = db::blobs::get_blob_by_key(&state.db, bucket_name, key)
        .await?
        .ok_or(AppError::NotFound)?;
    if &metadata.account_id != account_id {
        return Err(AppError::NotFound);
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "/buckets/{bucket_name}/blobs/{key}/tags",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        ("key" = String, Path, description = "Object key"),
    ),
    responses(
        (status = 200, description = "Tags for the blob", body = BlobTagsResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// List all tags on a blob.
pub async fn list_blob_tags(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path((bucket_name, key)): Path<(String, String)>,
) -> Result<Json<BlobTagsResponse>, AppError> {
    ensure_blob_exists(&state, &auth.account_id, &bucket_name, &key).await?;
    let tags = db::blob_tags::list_tags(&state.db, &auth.account_id, &bucket_name, &key).await?;
    Ok(Json(BlobTagsResponse { tags }))
}

#[utoipa::path(
    put,
    path = "/buckets/{bucket_name}/blobs/{key}/tags",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        ("key" = String, Path, description = "Object key"),
    ),
    request_body = PutTagsRequest,
    responses(
        (status = 204, description = "Tag set replaced"),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Replace the entire tag set on a blob (S3 `PutObjectTagging`-equivalent).
pub async fn replace_blob_tags(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path((bucket_name, key)): Path<(String, String)>,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let req: PutTagsRequest = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("invalid JSON body: {e}")))?;
    ensure_blob_exists(&state, &auth.account_id, &bucket_name, &key).await?;
    let tags: Vec<(String, String)> = req.tags.into_iter().collect();
    validation::validate_tag_set(&tags).map_err(AppError::BadRequest)?;
    db::blob_tags::replace_all_tags(&state.db, &auth.account_id, &bucket_name, &key, &tags).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    patch,
    path = "/buckets/{bucket_name}/blobs/{key}/tags",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        ("key" = String, Path, description = "Object key"),
    ),
    request_body = PatchTagsRequest,
    responses(
        (status = 204, description = "Tags merged"),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Merge a partial tag set into a blob's existing tags (upsert per key).
pub async fn patch_blob_tags(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path((bucket_name, key)): Path<(String, String)>,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let req: PatchTagsRequest = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("invalid JSON body: {e}")))?;
    ensure_blob_exists(&state, &auth.account_id, &bucket_name, &key).await?;
    let tags: Vec<(String, String)> = req.tags.into_iter().collect();
    // Validate individual entries; full-set cap is enforced against the merged result.
    for (k, v) in &tags {
        validation::validate_tag_kv(k, v).map_err(AppError::BadRequest)?;
    }
    // Post-merge size check: fetch existing and compute the would-be resulting set.
    let mut merged =
        db::blob_tags::list_tags(&state.db, &auth.account_id, &bucket_name, &key).await?;
    for (k, v) in &tags {
        merged.insert(k.clone(), v.clone());
    }
    let merged_vec: Vec<(String, String)> = merged.into_iter().collect();
    validation::validate_tag_set(&merged_vec).map_err(AppError::BadRequest)?;
    db::blob_tags::merge_tags(&state.db, &auth.account_id, &bucket_name, &key, &tags).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put,
    path = "/buckets/{bucket_name}/blobs/{key}/tags/{tag_key}",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        ("key" = String, Path, description = "Object key"),
        ("tag_key" = String, Path, description = "Tag key"),
    ),
    request_body(content = String, content_type = "text/plain"),
    responses(
        (status = 204, description = "Tag upserted"),
        (status = 400, description = "Bad request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Upsert a single tag on a blob (body is the raw value).
pub async fn put_blob_tag(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path((bucket_name, key, tag_key)): Path<(String, String, String)>,
    body: String,
) -> Result<StatusCode, AppError> {
    ensure_blob_exists(&state, &auth.account_id, &bucket_name, &key).await?;
    validation::validate_tag_kv(&tag_key, &body).map_err(AppError::BadRequest)?;
    // Ensure the post-upsert tag count doesn't exceed the per-blob cap.
    let existing =
        db::blob_tags::list_tags(&state.db, &auth.account_id, &bucket_name, &key).await?;
    let would_add = !existing.contains_key(&tag_key);
    if would_add && existing.len() >= validation::MAX_TAGS {
        return Err(AppError::BadRequest(format!(
            "at most {} tags allowed",
            validation::MAX_TAGS
        )));
    }
    db::blob_tags::upsert_tag(
        &state.db,
        &auth.account_id,
        &bucket_name,
        &key,
        &tag_key,
        &body,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/buckets/{bucket_name}/blobs/{key}/tags/{tag_key}",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        ("key" = String, Path, description = "Object key"),
        ("tag_key" = String, Path, description = "Tag key"),
    ),
    responses(
        (status = 204, description = "Tag removed (or did not exist)"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Delete a single tag from a blob. Idempotent.
pub async fn delete_blob_tag(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path((bucket_name, key, tag_key)): Path<(String, String, String)>,
) -> Result<StatusCode, AppError> {
    ensure_blob_exists(&state, &auth.account_id, &bucket_name, &key).await?;
    db::blob_tags::delete_tag(&state.db, &auth.account_id, &bucket_name, &key, &tag_key).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/buckets/{bucket_name}/blobs/{key}/tags",
    tag = "Blobs",
    security(("bearer" = [])),
    params(
        ("bucket_name" = String, Path, description = "Bucket name"),
        ("key" = String, Path, description = "Object key"),
    ),
    responses(
        (status = 204, description = "All tags removed"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Blob not found", body = ErrorResponse),
    ),
)]
/// Remove every tag from a blob (S3 `DeleteObjectTagging`-equivalent).
pub async fn clear_blob_tags(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path((bucket_name, key)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    ensure_blob_exists(&state, &auth.account_id, &bucket_name, &key).await?;
    db::blob_tags::clear_tags(&state.db, &auth.account_id, &bucket_name, &key).await?;
    Ok(StatusCode::NO_CONTENT)
}
