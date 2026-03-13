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
    S3Error::with_source(S3ErrorCode::InternalError, Box::new(e))
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

        let deleted_blobs = db::blobs::delete_blobs_in_bucket(&self.state.db, bucket_name)
            .await
            .map_err(internal_error)?;
        for info in &deleted_blobs {
            let count = db::blobs::count_references(&self.state.db, &info.blob_id)
                .await
                .map_err(internal_error)?;
            if count == 0 {
                let _ = self
                    .state
                    .blob_store
                    .delete(
                        &BlobId(info.blob_id.clone()),
                        info.sui_object_id.as_deref(),
                        &account_id,
                    )
                    .await;
            }
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

        db::buckets::get_bucket(&self.state.db, &bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        let md5_digest = format!("{:x}", md5::compute(&body_bytes));

        let result = self
            .state
            .blob_store
            .store(&body_bytes, &account_id)
            .await
            .map_err(|e| internal_error(std::io::Error::other(e.to_string())))?;

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(30))
            .expect("valid date")
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let metadata = db::blobs::insert_blob(
            &self.state.db,
            &key,
            result.blob_id.as_str(),
            &bucket_name,
            &account_id,
            &content_type,
            body_bytes.len() as i64,
            &md5_digest,
            &expires_at,
            result.sui_object_id.as_deref(),
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

        db::buckets::get_bucket(&self.state.db, bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        let metadata = db::blobs::get_blob_by_key(&self.state.db, bucket_name, key)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchKey))?;

        let data = self
            .state
            .blob_store
            .read(&BlobId(metadata.blob_id))
            .await
            .map_err(|e| internal_error(std::io::Error::other(e.to_string())))?;

        let body = StreamingBlob::from(s3s::Body::from(data));

        Ok(S3Response::new(GetObjectOutput {
            body: Some(body),
            content_length: Some(metadata.size),
            content_type: Some(metadata.content_type),
            e_tag: Some(etag_from_md5(&metadata.md5)),
            last_modified: parse_timestamp(&metadata.created_at),
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

        db::buckets::get_bucket(&self.state.db, bucket_name, &account_id)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchBucket))?;

        let metadata = db::blobs::get_blob_by_key(&self.state.db, bucket_name, key)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchKey))?;

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

        if let Some(info) = db::blobs::delete_blob(&self.state.db, bucket_name, key, &account_id)
            .await
            .map_err(internal_error)?
        {
            let count = db::blobs::count_references(&self.state.db, &info.blob_id)
                .await
                .map_err(internal_error)?;
            if count == 0 {
                let _ = self
                    .state
                    .blob_store
                    .delete(
                        &BlobId(info.blob_id),
                        info.sui_object_id.as_deref(),
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
