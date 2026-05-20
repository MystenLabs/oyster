use std::collections::BTreeSet;

use futures::StreamExt;
use s3s::{
    S3Error,
    S3ErrorCode,
    S3Request,
    S3Response,
    S3Result,
    auth::{S3Auth, SecretKey},
    dto::*,
    service::S3ServiceBuilder,
};

use crate::{AccountId, AppState, blob_store::BlobId, db, validation};

/// S3-compatible API implementation backed by Oyster's DB and blob store.
pub struct OysterS3 {
    pub(crate) state: AppState,
}

/// S3 authentication that looks up access keys from the database.
pub struct OysterS3Auth {
    pub(crate) db: db::DbPool,
}

fn internal_error(e: impl std::error::Error + Send + Sync + 'static) -> S3Error {
    tracing::error!(error = %e, "S3 internal error");
    S3Error::with_source(S3ErrorCode::InternalError, Box::new(e))
}

fn blob_store_error(e: crate::blob_store::BlobStoreError) -> S3Error {
    use crate::blob_store::BlobStoreError;
    match e {
        BlobStoreError::NotFound(ref id) => {
            tracing::warn!(blob = %id, "blob not found");
            S3Error::with_message(S3ErrorCode::NoSuchKey, format!("blob not found: {id}"))
        }
        BlobStoreError::InvalidBlobId(ref msg) => {
            tracing::warn!(error = %msg, "invalid blob_id");
            S3Error::with_message(S3ErrorCode::InvalidRequest, msg.clone())
        }
        BlobStoreError::InsufficientBalance {
            ref message,
            ref funding_required,
            ..
        } => {
            tracing::warn!(error = %message, "insufficient balance for blob operation");
            // S3 doesn't have a first-class structured-extension slot for this,
            // so render `funding_required` into the message body. Mirrors the
            // JSON response from the Axum side as closely as `S3Error` allows.
            let body = match funding_required {
                Some(amount) => format!(
                    "account has insufficient balance: {message}; \
                     funding_required={{wal_frost:{},sui_mist:{}}}",
                    amount.wal_frost, amount.sui_mist,
                ),
                None => format!("account has insufficient balance: {message}"),
            };
            let mut err = S3Error::with_message(
                S3ErrorCode::Custom(bytestring::ByteString::from("InsufficientBalance")),
                body,
            );
            err.set_status_code(hyper::StatusCode::PAYMENT_REQUIRED);
            err
        }
        BlobStoreError::Io(ref io_err) => {
            tracing::error!(error = %io_err, "blob store I/O error");
            S3Error::with_message(S3ErrorCode::InternalError, io_err.to_string())
        }
        BlobStoreError::Internal(ref msg) => {
            tracing::error!(error = %msg, "blob store internal error");
            S3Error::with_message(S3ErrorCode::InternalError, msg.clone())
        }
        BlobStoreError::Upstream(ref msg) => {
            tracing::error!(error = %msg, "blob store upstream error");
            let mut err =
                S3Error::with_message(S3ErrorCode::InternalError, "upstream blob store error");
            err.set_status_code(hyper::StatusCode::BAD_GATEWAY);
            err
        }
        BlobStoreError::UpstreamStatus {
            status,
            ref message,
        } => {
            let code =
                hyper::StatusCode::from_u16(status).unwrap_or(hyper::StatusCode::BAD_GATEWAY);
            let mut err = if code.is_client_error() {
                tracing::debug!(
                    upstream_status = status,
                    upstream_body = %message,
                    "upstream blob store client error",
                );
                S3Error::with_message(S3ErrorCode::InvalidRequest, message.clone())
            } else {
                tracing::error!(
                    upstream_status = status,
                    upstream_body = %message,
                    "upstream blob store error",
                );
                S3Error::with_message(S3ErrorCode::InternalError, "upstream blob store error")
            };
            err.set_status_code(if code.is_client_error() {
                code
            } else {
                hyper::StatusCode::BAD_GATEWAY
            });
            err
        }
        BlobStoreError::Unreachable(ref msg) => {
            tracing::error!(error = %msg, "upstream blob store unreachable");
            let mut err = S3Error::with_message(
                S3ErrorCode::InternalError,
                "upstream blob store unreachable",
            );
            err.set_status_code(hyper::StatusCode::BAD_GATEWAY);
            err
        }
        BlobStoreError::PoolCreationFailed(ref msg) => {
            tracing::error!(error = %msg, "pool creation failed");
            let mut err = S3Error::with_message(S3ErrorCode::InternalError, "pool creation failed");
            err.set_status_code(hyper::StatusCode::BAD_GATEWAY);
            err
        }
        BlobStoreError::Database(ref db_err) => {
            tracing::error!(error = %db_err, "blob store database error");
            S3Error::with_message(S3ErrorCode::InternalError, db_err.to_string())
        }
    }
}

/// Parse an `x-amz-tagging` URL-form value into a validated tag list.
fn parse_amz_tagging(s: &str) -> S3Result<Vec<(String, String)>> {
    let pairs: Vec<(String, String)> = form_urlencoded::parse(s.as_bytes()).into_owned().collect();
    validation::validate_tag_set(&pairs)
        .map_err(|msg| S3Error::with_message(S3ErrorCode::InvalidTag, msg))?;
    Ok(pairs)
}

/// Convert a `Tagging` dto into a validated (key, value) vec.
fn tagging_to_pairs(tagging: &Tagging) -> S3Result<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(tagging.tag_set.len());
    for tag in &tagging.tag_set {
        let k = tag.key.clone().unwrap_or_default();
        let v = tag.value.clone().unwrap_or_default();
        out.push((k, v));
    }
    validation::validate_tag_set(&out)
        .map_err(|msg| S3Error::with_message(S3ErrorCode::InvalidTag, msg))?;
    Ok(out)
}

fn pairs_to_tag_set(pairs: impl IntoIterator<Item = (String, String)>) -> Vec<Tag> {
    pairs
        .into_iter()
        .map(|(k, v)| Tag {
            key: Some(k),
            value: Some(v),
        })
        .collect()
}

fn parse_timestamp(s: &str) -> Option<Timestamp> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .and_then(|dt| {
            let utc = dt.and_utc();
            let epoch = utc.timestamp();
            time::OffsetDateTime::from_unix_timestamp(epoch).ok()
        })
        .map(Timestamp::from)
}

fn etag_from_md5(md5: &str) -> ETag {
    ETag::Strong(md5.to_string())
}

/// Check conditional headers (If-Match / If-None-Match) against the current ETag.
///
/// Returns `Ok(())` if conditions pass, or an appropriate S3 error on failure.
fn check_conditions(
    if_match: &Option<IfMatch>,
    if_none_match: &Option<IfNoneMatch>,
    current_etag: Option<&str>,
    is_safe_method: bool,
) -> S3Result<()> {
    if let Some(condition) = if_match {
        match condition {
            ETagCondition::Any => {
                if current_etag.is_none() {
                    return Err(S3Error::new(S3ErrorCode::PreconditionFailed));
                }
            }
            ETagCondition::ETag(etag) => match current_etag {
                Some(current) => {
                    if !etag.strong_cmp(&etag_from_md5(current)) {
                        return Err(S3Error::new(S3ErrorCode::PreconditionFailed));
                    }
                }
                None => {
                    return Err(S3Error::new(S3ErrorCode::PreconditionFailed));
                }
            },
        }
    }
    if let Some(condition) = if_none_match {
        match condition {
            ETagCondition::Any => {
                if current_etag.is_some() {
                    return if is_safe_method {
                        Err(S3Error::new(S3ErrorCode::NotModified))
                    } else {
                        Err(S3Error::new(S3ErrorCode::PreconditionFailed))
                    };
                }
            }
            ETagCondition::ETag(etag) => {
                if let Some(current) = current_etag
                    && etag.weak_cmp(&etag_from_md5(current))
                {
                    return if is_safe_method {
                        Err(S3Error::new(S3ErrorCode::NotModified))
                    } else {
                        Err(S3Error::new(S3ErrorCode::PreconditionFailed))
                    };
                }
            }
        }
    }
    Ok(())
}

impl OysterS3 {
    /// Create a new OysterS3 from an AppState.
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    async fn account_id<T: Send>(&self, req: &S3Request<T>) -> S3Result<AccountId> {
        let access_key = &req
            .credentials
            .as_ref()
            .ok_or_else(|| S3Error::new(S3ErrorCode::AccessDenied))?
            .access_key;
        let record = db::access_keys::find_by_access_key_id(&self.state.db, access_key)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::InvalidAccessKeyId))?;
        Ok(record.account_id)
    }
}

#[async_trait::async_trait]
impl S3Auth for OysterS3Auth {
    async fn get_secret_key(&self, access_key: &str) -> S3Result<SecretKey> {
        tracing::info!(access_key_id = %access_key, "s3 auth lookup");
        let record = db::access_keys::find_by_access_key_id(&self.db, access_key)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::InvalidAccessKeyId))?;
        Ok(SecretKey::from(record.secret_access_key))
    }
}

#[async_trait::async_trait]
impl s3s::S3 for OysterS3 {
    async fn list_buckets(
        &self,
        req: S3Request<ListBucketsInput>,
    ) -> S3Result<S3Response<ListBucketsOutput>> {
        let account_id = self.account_id(&req).await?;
        tracing::info!(account_id = %account_id, "s3 list_buckets");
        let buckets = db::buckets::list_buckets(&self.state.db, &account_id, None, None, 1000)
            .await
            .map_err(internal_error)?;

        let s3_buckets: Vec<s3s::dto::Bucket> = buckets
            .into_iter()
            .map(|b| s3s::dto::Bucket {
                name: Some(b.name),
                creation_date: parse_timestamp(&b.created_at),
                bucket_region: None,
            })
            .collect();

        Ok(S3Response::new(ListBucketsOutput {
            buckets: Some(s3_buckets),
            continuation_token: None,
            owner: None,
            prefix: None,
        }))
    }

    async fn create_bucket(
        &self,
        req: S3Request<CreateBucketInput>,
    ) -> S3Result<S3Response<CreateBucketOutput>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = &req.input.bucket;
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, "s3 create_bucket");

        validation::validate_bucket_name(bucket_name)
            .map_err(|e| S3Error::with_message(S3ErrorCode::InvalidBucketName, e))?;

        db::buckets::create_bucket(&self.state.db, &account_id, bucket_name)
            .await
            .map_err(|e| {
                if let sqlx::Error::Database(ref db_err) = e
                    && db_err.message().contains("UNIQUE constraint failed")
                {
                    return S3Error::new(S3ErrorCode::BucketAlreadyOwnedByYou);
                }
                internal_error(e)
            })?;

        Ok(S3Response::new(CreateBucketOutput {
            location: Some(format!("/{bucket_name}")),
        }))
    }

    async fn delete_bucket(
        &self,
        req: S3Request<DeleteBucketInput>,
    ) -> S3Result<S3Response<DeleteBucketOutput>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = &req.input.bucket;
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, "s3 delete_bucket");

        let blob_count = db::blobs::count_blobs_in_bucket(&self.state.db, bucket_name)
            .await
            .map_err(internal_error)?;
        if blob_count > 0 {
            return Err(S3Error::new(S3ErrorCode::BucketNotEmpty));
        }

        let deleted = db::buckets::delete_bucket(&self.state.db, bucket_name, &account_id)
            .await
            .map_err(internal_error)?;
        if !deleted {
            return Err(S3Error::new(S3ErrorCode::NoSuchBucket));
        }

        Ok(S3Response::new(DeleteBucketOutput {}))
    }

    async fn head_bucket(
        &self,
        req: S3Request<HeadBucketInput>,
    ) -> S3Result<S3Response<HeadBucketOutput>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = &req.input.bucket;
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, "s3 head_bucket");

        db::buckets::get_bucket(&self.state.db, bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        Ok(S3Response::new(HeadBucketOutput {
            access_point_alias: None,
            bucket_location_name: None,
            bucket_location_type: None,
            bucket_region: None,
        }))
    }

    async fn put_object(
        &self,
        req: S3Request<PutObjectInput>,
    ) -> S3Result<S3Response<PutObjectOutput>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = req.input.bucket.clone();
        let key = req.input.key.clone();
        let content_type = req
            .input
            .content_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, key = %key, content_type = %content_type, "s3 put_object");

        // Collect body bytes
        let body_bytes = match req.input.body {
            Some(blob) => {
                let mut data = Vec::new();
                let mut stream = blob;
                while let Some(chunk) = stream.next().await {
                    let bytes =
                        chunk.map_err(|e| internal_error(std::io::Error::other(e.to_string())))?;
                    data.extend_from_slice(&bytes);
                }
                data
            }
            None => Vec::new(),
        };
        tracing::info!(body_size = body_bytes.len(), "s3 put_object body collected");

        db::buckets::get_bucket(&self.state.db, &bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        let existing = db::blobs::get_blob_by_key(&self.state.db, &bucket_name, &key)
            .await
            .map_err(internal_error)?;
        check_conditions(
            &req.input.if_match,
            &req.input.if_none_match,
            existing.as_ref().map(|m| m.md5.as_str()),
            false,
        )?;

        let md5_digest = format!("{:x}", md5::compute(&body_bytes));

        let result = self
            .state
            .blob_store
            .store(&body_bytes, &account_id)
            .await
            .map_err(blob_store_error)?;

        // Validate tagging header before we write anything, so a malformed
        // tag set fails fast without side effects.
        let initial_tags = match req.input.tagging.as_deref() {
            Some(raw) if !raw.is_empty() => parse_amz_tagging(raw)?,
            _ => Vec::new(),
        };

        let metadata = db::blobs::insert_blob(
            &self.state.db,
            &key,
            result.blob_id.as_str(),
            &bucket_name,
            &account_id,
            &content_type,
            body_bytes.len() as i64,
            &md5_digest,
            result.pooled_blob_object_id.as_deref(),
            result.encoded_size.map(|e| e as i64),
        )
        .await
        .map_err(internal_error)?;

        db::blob_tags::replace_all_tags(
            &self.state.db,
            &account_id,
            &bucket_name,
            &key,
            &initial_tags,
        )
        .await
        .map_err(internal_error)?;

        Ok(S3Response::new(PutObjectOutput {
            e_tag: Some(etag_from_md5(&metadata.md5)),
            ..Default::default()
        }))
    }

    async fn get_object(
        &self,
        req: S3Request<GetObjectInput>,
    ) -> S3Result<S3Response<GetObjectOutput>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = &req.input.bucket;
        let key = &req.input.key;
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, key = %key, "s3 get_object");

        db::buckets::get_bucket(&self.state.db, bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        let metadata = db::blobs::get_blob_by_key(&self.state.db, bucket_name, key)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchKey))?;

        check_conditions(
            &req.input.if_match,
            &req.input.if_none_match,
            Some(&metadata.md5),
            true,
        )?;

        let data = self
            .state
            .blob_store
            .read(&BlobId(metadata.blob_id))
            .await
            .map_err(blob_store_error)?;

        let body = StreamingBlob::from(s3s::Body::from(data));

        let tag_count = db::blob_tags::count_tags(&self.state.db, bucket_name, key)
            .await
            .map_err(internal_error)? as i32;

        Ok(S3Response::new(GetObjectOutput {
            body: Some(body),
            content_length: Some(metadata.size),
            content_type: Some(metadata.content_type),
            e_tag: Some(etag_from_md5(&metadata.md5)),
            last_modified: parse_timestamp(&metadata.created_at),
            tag_count: Some(tag_count),
            ..Default::default()
        }))
    }

    async fn head_object(
        &self,
        req: S3Request<HeadObjectInput>,
    ) -> S3Result<S3Response<HeadObjectOutput>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = &req.input.bucket;
        let key = &req.input.key;
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, key = %key, "s3 head_object");

        db::buckets::get_bucket(&self.state.db, bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        let metadata = db::blobs::get_blob_by_key(&self.state.db, bucket_name, key)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchKey))?;

        check_conditions(
            &req.input.if_match,
            &req.input.if_none_match,
            Some(&metadata.md5),
            true,
        )?;

        Ok(S3Response::new(HeadObjectOutput {
            content_length: Some(metadata.size),
            content_type: Some(metadata.content_type),
            e_tag: Some(etag_from_md5(&metadata.md5)),
            last_modified: parse_timestamp(&metadata.created_at),
            ..Default::default()
        }))
    }

    async fn delete_object(
        &self,
        req: S3Request<DeleteObjectInput>,
    ) -> S3Result<S3Response<DeleteObjectOutput>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = &req.input.bucket;
        let key = &req.input.key;
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, key = %key, "s3 delete_object");

        let existing = db::blobs::get_blob_by_key(&self.state.db, bucket_name, key)
            .await
            .map_err(internal_error)?;
        check_conditions(
            &req.input.if_match,
            &None,
            existing.as_ref().map(|m| m.md5.as_str()),
            false,
        )?;

        if let Some(info) = db::blobs::delete_blob(&self.state.db, bucket_name, key, &account_id)
            .await
            .map_err(internal_error)?
        {
            let count = db::blobs::count_references(&self.state.db, &info.blob_id)
                .await
                .map_err(internal_error)?;
            if count == 0 {
                let pool_id = db::accounts::get_storage_pool(&self.state.db, &account_id)
                    .await
                    .map_err(internal_error)?
                    .map(|s| s.object_id);
                let _ = self
                    .state
                    .blob_store
                    .delete(
                        &BlobId(info.blob_id),
                        pool_id.as_deref(),
                        info.encoded_size.unwrap_or(0) as u64,
                        &account_id,
                    )
                    .await;
            }
        }

        Ok(S3Response::new(DeleteObjectOutput {
            ..Default::default()
        }))
    }

    async fn list_objects_v2(
        &self,
        req: S3Request<ListObjectsV2Input>,
    ) -> S3Result<S3Response<ListObjectsV2Output>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = &req.input.bucket;
        let prefix = req.input.prefix.as_deref().unwrap_or("");
        let delimiter = req.input.delimiter.as_deref();
        let max_keys = req.input.max_keys.unwrap_or(1000);
        let start_after = req.input.start_after.as_deref();
        let continuation_token = req.input.continuation_token.as_deref();
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, prefix = %prefix, max_keys, "s3 list_objects_v2");

        db::buckets::get_bucket(&self.state.db, bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        let all_blobs = db::blobs::list_blobs_in_bucket(
            &self.state.db,
            bucket_name,
            &account_id,
            None,
            None,
            i64::MAX,
        )
        .await
        .map_err(internal_error)?;

        let start_key = continuation_token.or(start_after).unwrap_or("");

        let mut matching: Vec<_> = all_blobs
            .into_iter()
            .filter(|b| b.key.starts_with(prefix) && b.key.as_str() > start_key)
            .collect();
        matching.sort_by(|a, b| a.key.cmp(&b.key));

        if let Some(delim) = delimiter {
            let mut contents = Vec::new();
            let mut common_prefixes_set = BTreeSet::new();

            for blob in &matching {
                let after_prefix = &blob.key[prefix.len()..];
                if let Some(pos) = after_prefix.find(delim) {
                    let cp = format!("{}{}", prefix, &after_prefix[..pos + delim.len()]);
                    common_prefixes_set.insert(cp);
                } else {
                    contents.push(blob.clone());
                }
            }

            let total_count = contents.len() + common_prefixes_set.len();
            let is_truncated = total_count > max_keys as usize;
            contents.truncate(max_keys as usize);

            let common_prefixes: Vec<CommonPrefix> = common_prefixes_set
                .into_iter()
                .map(|p| CommonPrefix { prefix: Some(p) })
                .collect();

            let key_count = contents.len() + common_prefixes.len();
            let next_token = if is_truncated {
                contents.last().map(|b| b.key.clone())
            } else {
                None
            };

            let objects: Vec<Object> = contents.into_iter().map(|b| blob_to_object(&b)).collect();

            Ok(S3Response::new(ListObjectsV2Output {
                name: Some(bucket_name.clone()),
                prefix: Some(prefix.to_string()),
                max_keys: Some(max_keys),
                key_count: Some(key_count as i32),
                is_truncated: Some(is_truncated),
                next_continuation_token: next_token,
                contents: Some(objects),
                common_prefixes: Some(common_prefixes),
                delimiter: delimiter.map(|s| s.to_string()),
                ..Default::default()
            }))
        } else {
            let is_truncated = matching.len() > max_keys as usize;
            matching.truncate(max_keys as usize);

            let key_count = matching.len();
            let next_token = if is_truncated {
                matching.last().map(|b| b.key.clone())
            } else {
                None
            };

            let objects: Vec<Object> = matching.iter().map(blob_to_object).collect();

            Ok(S3Response::new(ListObjectsV2Output {
                name: Some(bucket_name.clone()),
                prefix: Some(prefix.to_string()),
                max_keys: Some(max_keys),
                key_count: Some(key_count as i32),
                is_truncated: Some(is_truncated),
                next_continuation_token: next_token,
                contents: Some(objects),
                common_prefixes: Some(Vec::new()),
                delimiter: None,
                ..Default::default()
            }))
        }
    }

    async fn get_object_tagging(
        &self,
        req: S3Request<GetObjectTaggingInput>,
    ) -> S3Result<S3Response<GetObjectTaggingOutput>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = &req.input.bucket;
        let key = &req.input.key;
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, key = %key, "s3 get_object_tagging");

        db::buckets::get_bucket(&self.state.db, bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        db::blobs::get_blob_by_key(&self.state.db, bucket_name, key)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchKey))?;

        let tags = db::blob_tags::list_tags(&self.state.db, &account_id, bucket_name, key)
            .await
            .map_err(internal_error)?;

        Ok(S3Response::new(GetObjectTaggingOutput {
            tag_set: pairs_to_tag_set(tags),
            ..Default::default()
        }))
    }

    async fn put_object_tagging(
        &self,
        req: S3Request<PutObjectTaggingInput>,
    ) -> S3Result<S3Response<PutObjectTaggingOutput>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = req.input.bucket.clone();
        let key = req.input.key.clone();
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, key = %key, "s3 put_object_tagging");

        db::buckets::get_bucket(&self.state.db, &bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        db::blobs::get_blob_by_key(&self.state.db, &bucket_name, &key)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchKey))?;

        let tags = tagging_to_pairs(&req.input.tagging)?;
        db::blob_tags::replace_all_tags(&self.state.db, &account_id, &bucket_name, &key, &tags)
            .await
            .map_err(internal_error)?;

        Ok(S3Response::new(PutObjectTaggingOutput {
            ..Default::default()
        }))
    }

    async fn delete_object_tagging(
        &self,
        req: S3Request<DeleteObjectTaggingInput>,
    ) -> S3Result<S3Response<DeleteObjectTaggingOutput>> {
        let account_id = self.account_id(&req).await?;
        let bucket_name = &req.input.bucket;
        let key = &req.input.key;
        tracing::info!(account_id = %account_id, bucket_name = %bucket_name, key = %key, "s3 delete_object_tagging");

        db::buckets::get_bucket(&self.state.db, bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        db::blobs::get_blob_by_key(&self.state.db, bucket_name, key)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchKey))?;

        db::blob_tags::clear_tags(&self.state.db, &account_id, bucket_name, key)
            .await
            .map_err(internal_error)?;

        Ok(S3Response::new(DeleteObjectTaggingOutput {
            ..Default::default()
        }))
    }
}

fn blob_to_object(b: &crate::models::BlobMetadata) -> Object {
    Object {
        key: Some(b.key.clone()),
        e_tag: Some(etag_from_md5(&b.md5)),
        last_modified: parse_timestamp(&b.created_at),
        size: Some(b.size),
        storage_class: Some(ObjectStorageClass::from(
            ObjectStorageClass::STANDARD.to_string(),
        )),
        ..Default::default()
    }
}

/// Build the S3 service from application state.
pub fn build_s3_service(state: &AppState) -> s3s::service::S3Service {
    let s3 = OysterS3::new(state.clone());
    let auth = OysterS3Auth {
        db: state.db.clone(),
    };
    let mut builder = S3ServiceBuilder::new(s3);
    builder.set_auth(auth);
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FundingAmount, blob_store::BlobStoreError};

    #[test]
    fn insufficient_balance_returns_402_with_funding_in_message() {
        let err = blob_store_error(BlobStoreError::InsufficientBalance {
            message: "could not find WAL coins".into(),
            funding_required: Some(FundingAmount {
                wal_frost: 12_345,
                sui_mist: 10_000_000,
            }),
            operation: "store_blob",
        });
        assert_eq!(err.status_code(), Some(hyper::StatusCode::PAYMENT_REQUIRED));
        let rendered = format!("{err}");
        assert!(
            rendered.contains("wal_frost:12345"),
            "expected wal_frost in rendered S3Error, got: {rendered}",
        );
        assert!(
            rendered.contains("sui_mist:10000000"),
            "expected sui_mist in rendered S3Error, got: {rendered}",
        );
    }

    #[test]
    fn insufficient_balance_without_funding_still_402() {
        let err = blob_store_error(BlobStoreError::InsufficientBalance {
            message: "no balance".into(),
            funding_required: None,
            operation: "store_blob",
        });
        assert_eq!(err.status_code(), Some(hyper::StatusCode::PAYMENT_REQUIRED));
    }
}
