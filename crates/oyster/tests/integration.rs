#![allow(missing_docs)]

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use oyster::{
    AccountId,
    AppState,
    auth,
    blob_store::{BlobId, BlobStore, BlobStoreError, LocalBlobStore, StoreResult},
    config::Config,
    db,
    routes,
    s3::OysterS3,
};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;
use walrus_sui::utils::BYTES_PER_UNIT_SIZE;

// ---------------------------------------------------------------------------
// SpyBlobStore – wraps LocalBlobStore and records account_id from store()
// ---------------------------------------------------------------------------

type DeleteCall = (String, Option<String>, AccountId);

struct SpyBlobStore {
    inner: LocalBlobStore,
    /// Each `store()` call appends the `account_id` argument here.
    calls: Mutex<Vec<AccountId>>,
    /// Each `delete()` call appends (blob_id, pool_id, account_id) here.
    delete_calls: Mutex<Vec<DeleteCall>>,
}

impl SpyBlobStore {
    fn new(inner: LocalBlobStore) -> Self {
        Self {
            inner,
            calls: Mutex::new(Vec::new()),
            delete_calls: Mutex::new(Vec::new()),
        }
    }

    fn recorded_calls(&self) -> Vec<AccountId> {
        self.calls.lock().unwrap().clone()
    }

    fn recorded_delete_calls(&self) -> Vec<DeleteCall> {
        self.delete_calls.lock().unwrap().clone()
    }
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

impl BlobStore for SpyBlobStore {
    fn store(
        &self,
        data: &[u8],
        account_id: &AccountId,
    ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>> {
        self.calls.lock().unwrap().push(*account_id);
        self.inner.store(data, account_id)
    }

    fn read(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<Vec<u8>, BlobStoreError>> {
        self.inner.read(blob_id)
    }

    fn delete(
        &self,
        blob_id: &BlobId,
        pool_id: Option<&str>,
        encoded_size: u64,
        account_id: &AccountId,
    ) -> BoxFuture<'_, Result<(), BlobStoreError>> {
        self.delete_calls.lock().unwrap().push((
            blob_id.0.clone(),
            pool_id.map(|s| s.to_string()),
            *account_id,
        ));
        self.inner
            .delete(blob_id, pool_id, encoded_size, account_id)
    }

    fn exists(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<bool, BlobStoreError>> {
        self.inner.exists(blob_id)
    }
}

/// Build a fresh app with an in-memory SQLite DB and a temp blob store directory.
/// Returns `(Router, TempDir, DbPool)` — hold onto the TempDir so it isn't dropped mid-test.
async fn test_app() -> (Router, TempDir, db::DbPool) {
    let tmp = TempDir::new().unwrap();
    let blob_path = tmp.path().join("blobs");

    let config = Config {
        bind_addr: "unused".into(),
        database_url: "sqlite::memory:".into(),
        blob_store_path: blob_path.clone(),
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),

        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,

        pool_initial_epochs_ahead: 5,
        pool_initial_encoded_capacity_bytes: BYTES_PER_UNIT_SIZE,
        pool_extend_epochs: 5,
        pool_extend_lookahead_epochs: 7,
        extension_idle_sleep_secs: 30,
        extension_busy_sleep_ms: 250,
        extension_claim_batch_size: 100,
        extension_claim_cooldown_secs: 60,
        extension_metrics_bind_addr: "unused".into(),
        allow_http_webhook_scheme: true,
    };

    let pool = db::create_pool(&config.database_url).await.unwrap();
    let blob_store = LocalBlobStore::new(blob_path).await.unwrap();

    let state = AppState {
        db: pool.clone(),
        blob_store: Arc::new(blob_store),
        pearl: None,
        config,
        metrics_handle: None,
    };

    (routes::build_router(state), tmp, pool)
}

/// Like `test_app()` but accepts an externally-created blob store and also returns the `DbPool`
/// (needed to create accounts with specific `account_id` values directly via the DB).
async fn test_app_with_spy(blob_store: Arc<SpyBlobStore>) -> (Router, TempDir, db::DbPool) {
    let tmp = TempDir::new().unwrap();
    let blob_path = tmp.path().join("blobs");

    let config = Config {
        bind_addr: "unused".into(),
        database_url: "sqlite::memory:".into(),
        blob_store_path: blob_path,
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),

        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,

        pool_initial_epochs_ahead: 5,
        pool_initial_encoded_capacity_bytes: BYTES_PER_UNIT_SIZE,
        pool_extend_epochs: 5,
        pool_extend_lookahead_epochs: 7,
        extension_idle_sleep_secs: 30,
        extension_busy_sleep_ms: 250,
        extension_claim_batch_size: 100,
        extension_claim_cooldown_secs: 60,
        extension_metrics_bind_addr: "unused".into(),
        allow_http_webhook_scheme: true,
    };

    let pool = db::create_pool(&config.database_url).await.unwrap();

    let state = AppState {
        db: pool.clone(),
        blob_store: blob_store as Arc<dyn BlobStore>,
        pearl: None,
        config,
        metrics_handle: None,
    };

    (routes::build_router(state), tmp, pool)
}

/// Helper: send a request and return (StatusCode, body as Value).
async fn json_response(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// Helper: send a request and return (StatusCode, raw bytes).
async fn raw_response(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

/// Helper: send a request and return (StatusCode, headers, raw bytes).
async fn full_response(
    app: &Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

/// Helper: create an account directly via DB, returns (account_id, api_key_secret).
async fn create_test_account(pool: &db::DbPool) -> (String, String) {
    let account = db::accounts::create_account(pool, &oyster::AppId::INTERNAL, None)
        .await
        .unwrap();
    let raw_key = auth::generate_api_key();
    let key_hash = auth::hash_api_key(&raw_key);
    let prefix = auth::key_prefix(&raw_key);
    db::api_keys::create_api_key(pool, &account.id, &key_hash, &prefix, &raw_key, "api")
        .await
        .unwrap();
    (account.id.to_string(), raw_key)
}

/// Helper: create a bucket, returns the bucket name.
async fn create_test_bucket(app: &Router, api_key: &str, name: &str) -> String {
    let req = Request::post("/api/v1/buckets")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"name":"{name}"}}"#)))
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, StatusCode::CREATED);
    body["name"].as_str().unwrap().to_string()
}

/// Helper: store a blob, returns (key, blob_id).
async fn store_test_blob(
    app: &Router,
    api_key: &str,
    bucket_name: &str,
    key: &str,
    content_type: &str,
    data: &[u8],
) -> (String, String) {
    let req = Request::put(format!("/api/v1/buckets/{bucket_name}/blobs/{key}"))
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", content_type)
        .body(Body::from(data.to_vec()))
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(
        body.get("pooled_blob_object_id").is_some(),
        "response should include pooled_blob_object_id field"
    );
    assert!(
        body.get("md5").is_some(),
        "response should include md5 field"
    );
    let resp_key = body["key"].as_str().unwrap().to_string();
    let blob_id = body["blob_id"].as_str().unwrap().to_string();
    (resp_key, blob_id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_returns_ok() {
    let (app, _tmp, _pool) = test_app().await;
    let req = Request::get("/health").body(Body::empty()).unwrap();
    let (status, body) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn ready_returns_ok_without_pearl() {
    let (app, _tmp, _pool) = test_app().await;
    let req = Request::get("/ready").body(Body::empty()).unwrap();
    let (status, body) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ready"], true);
}

#[tokio::test]
async fn full_lifecycle() {
    let (app, _tmp, pool) = test_app().await;

    // 1. Create account
    let (_account_id, key) = create_test_account(&pool).await;

    // 2. Create bucket
    let bucket_name = create_test_bucket(&app, &key, "my-bucket").await;

    // 3. Store blob
    let (blob_key, blob_id) = store_test_blob(
        &app,
        &key,
        &bucket_name,
        "hello.txt",
        "text/plain",
        b"hello oyster",
    )
    .await;

    // 4. Read blob by bucket+key (no auth)
    let (status, body) = raw_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket_name}/blobs/{blob_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"hello oyster");

    // 5. Read blob by blob_id (no auth)
    let (status, body) = raw_response(
        &app,
        Request::get(format!("/api/v1/blobs/by-blob-id/{blob_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"hello oyster");

    // 6. List blobs in bucket
    let (status, body) = json_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket_name}/blobs"))
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let blobs = body["data"].as_array().unwrap();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0]["key"].as_str().unwrap(), blob_key);
    assert!(blobs[0].get("pooled_blob_object_id").is_some());
    assert!(blobs[0].get("md5").is_some());

    // 7. Update blob metadata
    let (status, body) = json_response(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!(
                "/api/v1/buckets/{bucket_name}/blobs/{blob_key}/metadata"
            ))
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"content_type":"text/html"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["content_type"].as_str().unwrap(), "text/html");

    // 8. Delete blob
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket_name}/blobs/{blob_key}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 9. Blob is gone
    let (status, _) = raw_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket_name}/blobs/{blob_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 10. Delete bucket
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket_name}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn stubs_return_501() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;

    let cases = vec![
        ("PUT", "/api/v1/account/billing"),
        ("GET", "/api/v1/account/report"),
        ("POST", "/api/v1/account/transfer"),
    ];

    for (method, path) in cases {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap();
        let (status, _) = json_response(&app, req).await;
        assert_eq!(
            status,
            StatusCode::NOT_IMPLEMENTED,
            "expected 501 for {method} {path}"
        );
    }
}

#[tokio::test]
async fn auth_required() {
    let (app, _tmp, _pool) = test_app().await;

    // Endpoints that require auth should reject unauthenticated requests.
    let cases = vec![
        Request::get("/api/v1/account/wallet")
            .body(Body::empty())
            .unwrap(),
        Request::post("/api/v1/buckets")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"x"}"#))
            .unwrap(),
        Request::get("/api/v1/buckets").body(Body::empty()).unwrap(),
    ];

    for req in cases {
        let (status, _) = json_response(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // Bad bearer token
    let req = Request::get("/api/v1/buckets")
        .header("authorization", "Bearer bogus_key_value")
        .body(Body::empty())
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn duplicate_bucket_name_conflict() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;

    create_test_bucket(&app, &key, "dup-name").await;

    // Second bucket with same name should conflict
    let req = Request::post("/api/v1/buckets")
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"dup-name"}"#))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn read_blob_by_nonexistent_blob_id() {
    let (app, _tmp, _pool) = test_app().await;
    let req = Request::get("/api/v1/blobs/by-blob-id/does-not-exist")
        .body(Body::empty())
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_blobs_nonexistent_bucket() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;
    let req = Request::get("/api/v1/buckets/no-such-bucket/blobs")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pagination_limit_zero_returns_400() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;
    create_test_bucket(&app, &key, "pag-test").await;
    let req = Request::get("/api/v1/buckets/pag-test/blobs?limit=0")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn pagination_limit_negative_returns_400() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;
    let req = Request::get("/api/v1/buckets?limit=-1")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn read_blob_includes_nosniff_header() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;
    create_test_bucket(&app, &key, "sniff-test").await;
    store_test_blob(&app, &key, "sniff-test", "doc.txt", "text/plain", b"hello").await;
    let req = Request::get("/api/v1/buckets/sniff-test/blobs/doc.txt")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = full_response(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
}

#[tokio::test]
async fn different_accounts_same_bucket_name() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key1) = create_test_account(&pool).await;
    let (_, key2) = create_test_account(&pool).await;

    create_test_bucket(&app, &key1, "shared-name").await;

    // Bucket names are globally unique — different account cannot reuse the name
    let req = Request::post("/api/v1/buckets")
        .header("authorization", format!("Bearer {key2}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"shared-name"}"#))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn not_found_cases() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;
    let bucket_name = create_test_bucket(&app, &key, "nf-test").await;

    // Read non-existent blob
    let (status, _) = raw_response(
        &app,
        Request::get(format!(
            "/api/v1/buckets/{bucket_name}/blobs/nonexistent-key"
        ))
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Read non-existent blob by blob_id
    let (status, _) = raw_response(
        &app,
        Request::get(
            "/api/v1/blobs/by-blob-id/0000000000000000000000000000000000000000000000000000000000000000",
        )
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Delete non-existent blob
    let (status, _) = json_response(
        &app,
        Request::delete(format!(
            "/api/v1/buckets/{bucket_name}/blobs/nonexistent-key"
        ))
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Delete non-existent bucket
    let (status, _) = json_response(
        &app,
        Request::delete("/api/v1/buckets/nonexistent-id")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn store_blob_to_nonexistent_bucket() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;

    let req = Request::put("/api/v1/buckets/nonexistent-bucket/blobs/test.txt")
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/octet-stream")
        .body(Body::from(b"data".to_vec()))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn content_addressed_dedup() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;
    let bucket_name = create_test_bucket(&app, &key, "dedup-test").await;

    let data = b"identical content";
    let (key1, bid1) =
        store_test_blob(&app, &key, &bucket_name, "file1.txt", "text/plain", data).await;
    let (key2, bid2) =
        store_test_blob(&app, &key, &bucket_name, "file2.txt", "text/plain", data).await;

    // Same content -> same blob_id, different keys
    assert_eq!(bid1, bid2);
    assert_ne!(key1, key2);

    // Delete one — the other should still be readable
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket_name}/blobs/{key1}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let (status, body) = raw_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket_name}/blobs/{key2}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, data);
}

#[tokio::test]
async fn bucket_pagination() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;

    // Create 3 buckets
    for i in 0..3 {
        create_test_bucket(&app, &key, &format!("bucket-{i}")).await;
    }

    // Fetch with limit=2
    let (status, body) = json_response(
        &app,
        Request::get("/api/v1/buckets?limit=2")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page1 = body["data"].as_array().unwrap();
    assert_eq!(page1.len(), 2);
    let cursor = body["next_cursor"].as_str().unwrap();

    // Fetch next page
    let (status, body) = json_response(
        &app,
        Request::get(format!("/api/v1/buckets?limit=2&cursor={cursor}"))
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page2 = body["data"].as_array().unwrap();
    assert_eq!(page2.len(), 1);
    assert!(body["next_cursor"].is_null());
}

#[tokio::test]
async fn delete_bucket_rejects_when_not_empty() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;
    let bucket_name = create_test_bucket(&app, &key, "notempty-test").await;

    let (blob_key, _) = store_test_blob(
        &app,
        &key,
        &bucket_name,
        "keep.bin",
        "application/octet-stream",
        b"keep me",
    )
    .await;

    // Deleting a non-empty bucket should return 409 Conflict
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket_name}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // Delete the blob first
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket_name}/blobs/{blob_key}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Now deleting the empty bucket should succeed
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket_name}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn cross_account_isolation() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key1) = create_test_account(&pool).await;
    let (_, key2) = create_test_account(&pool).await;

    let bucket_name = create_test_bucket(&app, &key1, "private").await;

    // Account 2 cannot store to a bucket they don't own (not found).
    let req = Request::put(format!("/api/v1/buckets/{bucket_name}/blobs/intruder.txt"))
        .header("authorization", format!("Bearer {key2}"))
        .header("content-type", "text/plain")
        .body(Body::from(b"intruder".to_vec()))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Account 2 cannot delete account 1's bucket
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket_name}"))
                .header("authorization", format!("Bearer {key2}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_bucket_hides_non_empty_from_other_accounts() {
    // A caller who does not own a bucket must not be able to distinguish
    // "exists but non-empty" (409) from "does not exist for me" (404).
    let (app, _tmp, pool) = test_app().await;
    let (_, key1) = create_test_account(&pool).await;
    let (_, key2) = create_test_account(&pool).await;
    let bucket_name = create_test_bucket(&app, &key1, "leak-probe").await;
    store_test_blob(
        &app,
        &key1,
        &bucket_name,
        "keep.bin",
        "application/octet-stream",
        b"keep me",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket_name}"))
                .header("authorization", format!("Bearer {key2}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn blob_content_type_preserved() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;
    let bucket_name = create_test_bucket(&app, &key, "ct-test").await;

    let (blob_key, _) = store_test_blob(
        &app,
        &key,
        &bucket_name,
        "image.png",
        "image/png",
        b"\x89PNG fake",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/buckets/{bucket_name}/blobs/{blob_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "image/png"
    );
}

#[tokio::test]
async fn wallet_returns_503_without_pearl() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;

    let (status, _body) = json_response(
        &app,
        Request::get("/api/v1/account/wallet")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

// ---------------------------------------------------------------------------
// Oyster–Pearl integration (4.5.3)
// ---------------------------------------------------------------------------

/// Stand up Pearl's gRPC server in-process and return a connected `PearlConnection`.
async fn start_pearl() -> oyster::pearl_client::PearlConnection {
    use pearl::{
        auth::check_service_secret,
        config::Config,
        grpc::{PearlService, proto::pearl_server::PearlServer},
    };

    const PEARL_SECRET: &str = "oyster-pearl-test-secret";

    let config = Config {
        bind_addr: "127.0.0.1:0".into(),
        service_secret: PEARL_SECRET.into(),
        master_seed: zeroize::Zeroizing::new(hex::decode("ab".repeat(32)).expect("valid hex seed")),
        tls_cert_path: None,
        tls_key_path: None,
        metrics_bind_addr: "127.0.0.1:0".into(),
    };

    let service = PearlService { config };
    let interceptor = check_service_secret(PEARL_SECRET.to_string());
    let svc = PearlServer::with_interceptor(service, interceptor);

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<PearlServer<PearlService>>()
        .await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(health_service)
            .add_service(svc)
            .serve_with_incoming(incoming)
            .await
            .expect("pearl server error");
    });

    let url = format!("http://{addr}");
    for _ in 0..20 {
        if let Ok(conn) =
            oyster::pearl_client::PearlConnection::connect(&url, PEARL_SECRET.to_string()).await
        {
            return conn;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("could not connect to Pearl gRPC server at {url}");
}

#[tokio::test]
async fn pearl_client_get_address() {
    let pearl = start_pearl().await;

    let account_id = AccountId::new();

    // Fetch address through the wrapper — any account_id works now (stateless).
    let address = pearl.get_address(&account_id).await.unwrap();
    assert!(address.starts_with("0x"));
    assert_eq!(
        address.len(),
        66,
        "Sui address should be 66 chars (0x + 64 hex)"
    );

    // Same account_id returns the same address (deterministic).
    let address2 = pearl.get_address(&account_id).await.unwrap();
    assert_eq!(address, address2);
}

#[tokio::test]
async fn pearl_client_sign_transaction_success() {
    use sui_types::{
        base_types::{ObjectDigest, ObjectID, SuiAddress},
        programmable_transaction_builder::ProgrammableTransactionBuilder,
        transaction::TransactionData,
    };

    let pearl = start_pearl().await;

    let account_id = AccountId::new();
    let address = pearl.get_address(&account_id).await.unwrap();

    let sender: SuiAddress = address.parse().expect("valid SuiAddress");
    let gas_ref = (
        ObjectID::random(),
        sui_types::base_types::SequenceNumber::new(),
        ObjectDigest::random(),
    );
    let pt = ProgrammableTransactionBuilder::new().finish();
    let tx_data = TransactionData::new_programmable(sender, vec![gas_ref], pt, 5_000_000, 1_000);
    let tx_data_bytes = bcs::to_bytes(&tx_data).unwrap();

    let signed_bytes = pearl
        .sign_transaction(&account_id, tx_data_bytes)
        .await
        .unwrap();

    assert!(!signed_bytes.is_empty());

    // Verify the response deserializes back into a valid Transaction.
    let _tx: sui_types::transaction::Transaction =
        bcs::from_bytes(&signed_bytes).expect("valid Transaction");
}

#[tokio::test]
async fn pearl_client_sign_transaction_invalid_tx_data() {
    let pearl = start_pearl().await;

    let err = pearl
        .sign_transaction(&AccountId::new(), vec![1, 2, 3])
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

/// Build a fresh app backed by an in-process Pearl server and `LocalBlobStore`.
/// Returns `(Router, TempDir, DbPool)` — hold onto the TempDir so it isn't dropped mid-test.
async fn test_app_with_pearl() -> (Router, TempDir, db::DbPool) {
    let pearl = start_pearl().await;
    let tmp = TempDir::new().unwrap();
    let blob_path = tmp.path().join("blobs");

    let config = Config {
        bind_addr: "unused".into(),
        database_url: "sqlite::memory:".into(),
        blob_store_path: blob_path.clone(),
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),

        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,
        pool_initial_epochs_ahead: 5,
        pool_initial_encoded_capacity_bytes: BYTES_PER_UNIT_SIZE,
        pool_extend_epochs: 5,
        pool_extend_lookahead_epochs: 7,
        extension_idle_sleep_secs: 30,
        extension_busy_sleep_ms: 250,
        extension_claim_batch_size: 100,
        extension_claim_cooldown_secs: 60,
        extension_metrics_bind_addr: "unused".into(),
        allow_http_webhook_scheme: true,
    };

    let pool = db::create_pool(&config.database_url).await.unwrap();
    let blob_store = LocalBlobStore::new(blob_path).await.unwrap();

    let state = AppState {
        db: pool.clone(),
        blob_store: Arc::new(blob_store),
        pearl: Some(pearl),
        config,
        metrics_handle: None,
    };

    (routes::build_router(state), tmp, pool)
}

#[tokio::test]
async fn wallet_with_pearl_returns_address() {
    let (app, _tmp, pool) = test_app_with_pearl().await;

    // Create account — Pearl is connected so it provisions a wallet.
    let (_account_id, api_key) = create_test_account(&pool).await;

    let (status, body) = json_response(
        &app,
        Request::get("/api/v1/account/wallet")
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let address = body["address"].as_str().unwrap();
    assert!(
        address.starts_with("0x"),
        "address should start with 0x, got: {address}"
    );
    assert_eq!(
        address.len(),
        66,
        "Sui address should be 66 chars (0x + 64 hex), got: {address}"
    );
}

// ---------------------------------------------------------------------------
// Per-account account_id threading through BlobStore::store()
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_blob_passes_account_id() {
    let tmp = TempDir::new().unwrap();
    let local = LocalBlobStore::new(tmp.path().join("blobs")).await.unwrap();
    let spy = Arc::new(SpyBlobStore::new(local));

    let (app, _tmp, pool) = test_app_with_spy(spy.clone()).await;
    let (_, key) = create_test_account(&pool).await;
    let bucket_name = create_test_bucket(&app, &key, "test-bucket").await;

    store_test_blob(&app, &key, &bucket_name, "test.txt", "text/plain", b"data").await;

    let calls = spy.recorded_calls();
    assert_eq!(calls.len(), 1);
}

#[tokio::test]
async fn store_blob_distinguishes_accounts() {
    let tmp = TempDir::new().unwrap();
    let local = LocalBlobStore::new(tmp.path().join("blobs")).await.unwrap();
    let spy = Arc::new(SpyBlobStore::new(local));

    let (app, _tmp, pool) = test_app_with_spy(spy.clone()).await;

    let (_, key_a) = create_test_account(&pool).await;
    let (_, key_b) = create_test_account(&pool).await;

    let bucket_a = create_test_bucket(&app, &key_a, "bucket-a").await;
    let bucket_b = create_test_bucket(&app, &key_b, "bucket-b").await;

    store_test_blob(&app, &key_a, &bucket_a, "a.txt", "text/plain", b"aaa").await;
    store_test_blob(&app, &key_b, &bucket_b, "b.txt", "text/plain", b"bbb").await;

    let calls = spy.recorded_calls();
    assert_eq!(calls.len(), 2);
    assert_ne!(
        calls[0], calls[1],
        "different accounts should have different IDs"
    );
}

#[tokio::test]
async fn delete_blob_threads_account_id() {
    let tmp = TempDir::new().unwrap();
    let local = LocalBlobStore::new(tmp.path().join("blobs")).await.unwrap();
    let spy = Arc::new(SpyBlobStore::new(local));

    let (app, _tmp, pool) = test_app_with_spy(spy.clone()).await;
    let (_, key) = create_test_account(&pool).await;
    let bucket_name = create_test_bucket(&app, &key, "delete-test").await;

    let (blob_key, _blob_id) = store_test_blob(
        &app,
        &key,
        &bucket_name,
        "delete-me.txt",
        "text/plain",
        b"delete me",
    )
    .await;

    // Delete the blob via the API.
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket_name}/blobs/{blob_key}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let delete_calls = spy.recorded_delete_calls();
    assert_eq!(delete_calls.len(), 1, "expected exactly one delete call");
    let (ref _blob_id, ref pool_id, ref _account_id) = delete_calls[0];
    // pool_id is None because LocalBlobStore has no on-chain pool; the account
    // never lazy-created one, so routes/blobs.rs threads through None.
    assert_eq!(*pool_id, None);
}

// ---------------------------------------------------------------------------
// Metrics endpoint tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_endpoint_without_setup_returns_not_found() {
    let (app, _tmp, _pool) = test_app().await;
    let req = Request::get("/metrics").body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_format() {
    let tmp = TempDir::new().unwrap();
    let blob_path = tmp.path().join("blobs");

    let metrics_handle = oyster::metrics::setup();

    let config = Config {
        bind_addr: "unused".into(),
        database_url: "sqlite::memory:".into(),
        blob_store_path: blob_path.clone(),
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),
        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,
        pool_initial_epochs_ahead: 5,
        pool_initial_encoded_capacity_bytes: BYTES_PER_UNIT_SIZE,
        pool_extend_epochs: 5,
        pool_extend_lookahead_epochs: 7,
        extension_idle_sleep_secs: 30,
        extension_busy_sleep_ms: 250,
        extension_claim_batch_size: 100,
        extension_claim_cooldown_secs: 60,
        extension_metrics_bind_addr: "unused".into(),
        allow_http_webhook_scheme: true,
    };

    let pool = db::create_pool(&config.database_url).await.unwrap();
    let blob_store = oyster::blob_store::LocalBlobStore::new(blob_path)
        .await
        .unwrap();

    let state = AppState {
        db: pool,
        blob_store: Arc::new(blob_store),
        pearl: None,
        config,
        metrics_handle: Some(metrics_handle),
    };

    let app = routes::build_router(state);

    // Make a request so that metrics are populated.
    let req = Request::get("/health").body(Body::empty()).unwrap();
    let _ = app.clone().oneshot(req).await.unwrap();

    let req = Request::get("/metrics").body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(bytes.to_vec()).unwrap();

    assert!(
        body.contains("oyster_active_accounts"),
        "should contain active accounts gauge"
    );
    assert!(
        body.contains("oyster_active_blobs"),
        "should contain active blobs gauge"
    );
}

// ---------------------------------------------------------------------------
// S3 trait-level integration tests
// ---------------------------------------------------------------------------

use s3s::{
    S3,
    S3Request,
    auth::{Credentials, SecretKey},
    dto::*,
};

/// Build an OysterS3 with an in-memory DB and local blob store, plus an account
/// with an access key. Returns (OysterS3, access_key_id, TempDir).
async fn test_s3_with_account() -> (OysterS3, String, TempDir) {
    let tmp = TempDir::new().unwrap();
    let blob_path = tmp.path().join("blobs");

    let config = Config {
        bind_addr: "unused".into(),
        database_url: "sqlite::memory:".into(),
        blob_store_path: blob_path.clone(),
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),
        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,
        pool_initial_epochs_ahead: 5,
        pool_initial_encoded_capacity_bytes: BYTES_PER_UNIT_SIZE,
        pool_extend_epochs: 5,
        pool_extend_lookahead_epochs: 7,
        extension_idle_sleep_secs: 30,
        extension_busy_sleep_ms: 250,
        extension_claim_batch_size: 100,
        extension_claim_cooldown_secs: 60,
        extension_metrics_bind_addr: "unused".into(),
        allow_http_webhook_scheme: true,
    };

    let pool = db::create_pool(&config.database_url).await.unwrap();
    let blob_store = LocalBlobStore::new(blob_path).await.unwrap();

    let state = AppState {
        db: pool.clone(),
        blob_store: Arc::new(blob_store),
        pearl: None,
        config,
        metrics_handle: None,
    };

    let account = db::accounts::create_account(&pool, &oyster::AppId::INTERNAL, None)
        .await
        .unwrap();
    let access_key = db::access_keys::create_access_key(&pool, &account.id)
        .await
        .unwrap();

    let s3 = OysterS3::new(state);
    (s3, access_key.access_key_id, tmp)
}

/// Build an S3Request with credentials populated.
fn s3_req<T>(input: T, access_key_id: &str) -> S3Request<T> {
    use axum::http;
    S3Request {
        input,
        method: http::Method::GET,
        uri: http::Uri::from_static("/"),
        headers: http::HeaderMap::new(),
        extensions: http::Extensions::new(),
        credentials: Some(Credentials {
            access_key: access_key_id.to_string(),
            secret_key: SecretKey::from("unused-in-trait-tests"),
        }),
        region: None,
        service: None,
        trailing_headers: None,
    }
}

#[tokio::test]
async fn s3_create_and_list_buckets() {
    let (s3, ak, _tmp) = test_s3_with_account().await;

    // Create a bucket
    let resp = s3
        .create_bucket(s3_req(
            CreateBucketInput {
                bucket: "test-bucket".into(),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    assert_eq!(resp.output.location, Some("/test-bucket".into()));

    // List buckets — should contain the new bucket
    let resp = s3
        .list_buckets(s3_req(ListBucketsInput::default(), &ak))
        .await
        .unwrap();
    let names: Vec<_> = resp
        .output
        .buckets
        .unwrap()
        .iter()
        .map(|b| b.name.clone().unwrap())
        .collect();
    assert!(names.contains(&"test-bucket".to_string()));
}

#[tokio::test]
async fn s3_head_bucket() {
    let (s3, ak, _tmp) = test_s3_with_account().await;

    // HeadBucket on nonexistent → NoSuchBucket
    let err = s3
        .head_bucket(s3_req(
            HeadBucketInput {
                bucket: "no-such-bucket".into(),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::NoSuchBucket);

    // Create and then head → OK
    s3.create_bucket(s3_req(
        CreateBucketInput {
            bucket: "my-bucket".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    s3.head_bucket(s3_req(
        HeadBucketInput {
            bucket: "my-bucket".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn s3_put_get_delete_object() {
    let (s3, ak, _tmp) = test_s3_with_account().await;

    s3.create_bucket(s3_req(
        CreateBucketInput {
            bucket: "data".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    // PutObject
    let body_bytes = b"hello world";
    let body = StreamingBlob::from(s3s::Body::from(body_bytes.to_vec()));
    let put_resp = s3
        .put_object(s3_req(
            PutObjectInput {
                bucket: "data".into(),
                key: "greeting.txt".into(),
                body: Some(body),
                content_type: Some("text/plain".into()),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    assert!(put_resp.output.e_tag.is_some());

    // GetObject
    let get_resp = s3
        .get_object(s3_req(
            GetObjectInput {
                bucket: "data".into(),
                key: "greeting.txt".into(),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    assert_eq!(get_resp.output.content_type, Some("text/plain".into()));
    // Read the body
    let mut stream = get_resp.output.body.unwrap();
    let mut data = Vec::new();
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        data.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(data, body_bytes);

    // DeleteObject
    s3.delete_object(s3_req(
        DeleteObjectInput {
            bucket: "data".into(),
            key: "greeting.txt".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    // GetObject after delete → NoSuchKey
    let err = s3
        .get_object(s3_req(
            GetObjectInput {
                bucket: "data".into(),
                key: "greeting.txt".into(),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::NoSuchKey);
}

#[tokio::test]
async fn s3_head_object() {
    let (s3, ak, _tmp) = test_s3_with_account().await;

    s3.create_bucket(s3_req(
        CreateBucketInput {
            bucket: "meta".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    let content = b"some binary data";
    let body = StreamingBlob::from(s3s::Body::from(content.to_vec()));
    s3.put_object(s3_req(
        PutObjectInput {
            bucket: "meta".into(),
            key: "file.bin".into(),
            body: Some(body),
            content_type: Some("application/octet-stream".into()),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    let resp = s3
        .head_object(s3_req(
            HeadObjectInput {
                bucket: "meta".into(),
                key: "file.bin".into(),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    assert_eq!(resp.output.content_length, Some(content.len() as i64));
    assert_eq!(
        resp.output.content_type,
        Some("application/octet-stream".into())
    );
    assert!(resp.output.e_tag.is_some());
}

#[tokio::test]
async fn s3_list_objects_v2_with_prefix() {
    let (s3, ak, _tmp) = test_s3_with_account().await;

    s3.create_bucket(s3_req(
        CreateBucketInput {
            bucket: "mixed".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    for (key, data) in [
        ("photos/a.jpg", "a"),
        ("photos/b.jpg", "b"),
        ("docs/c.txt", "c"),
    ] {
        let body = StreamingBlob::from(s3s::Body::from(data.as_bytes().to_vec()));
        s3.put_object(s3_req(
            PutObjectInput {
                bucket: "mixed".into(),
                key: key.into(),
                body: Some(body),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    }

    let resp = s3
        .list_objects_v2(s3_req(
            ListObjectsV2Input {
                bucket: "mixed".into(),
                prefix: Some("photos/".into()),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();

    let keys: Vec<_> = resp
        .output
        .contents
        .unwrap()
        .iter()
        .map(|o| o.key.clone().unwrap())
        .collect();
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&"photos/a.jpg".to_string()));
    assert!(keys.contains(&"photos/b.jpg".to_string()));
}

#[tokio::test]
async fn s3_list_objects_v2_with_delimiter() {
    let (s3, ak, _tmp) = test_s3_with_account().await;

    s3.create_bucket(s3_req(
        CreateBucketInput {
            bucket: "structured".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    for (key, data) in [
        ("photos/a.jpg", "a"),
        ("photos/b.jpg", "b"),
        ("docs/c.txt", "c"),
    ] {
        let body = StreamingBlob::from(s3s::Body::from(data.as_bytes().to_vec()));
        s3.put_object(s3_req(
            PutObjectInput {
                bucket: "structured".into(),
                key: key.into(),
                body: Some(body),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    }

    let resp = s3
        .list_objects_v2(s3_req(
            ListObjectsV2Input {
                bucket: "structured".into(),
                delimiter: Some("/".into()),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();

    let common_prefixes: Vec<_> = resp
        .output
        .common_prefixes
        .unwrap()
        .iter()
        .map(|cp| cp.prefix.clone().unwrap())
        .collect();
    assert!(common_prefixes.contains(&"photos/".to_string()));
    assert!(common_prefixes.contains(&"docs/".to_string()));

    // No top-level objects (all keys contain the delimiter)
    let contents = resp.output.contents.unwrap();
    assert!(contents.is_empty());
}

// ---------------------------------------------------------------------------
// JSON API conditional request tests (If-Match / If-None-Match)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn json_conditional_requests() {
    let (app, _tmp, pool) = test_app().await;
    let (_, key) = create_test_account(&pool).await;
    let bucket_name = create_test_bucket(&app, &key, "cond-test").await;

    // Store a blob and extract md5
    let req = Request::put(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "text/plain")
        .body(Body::from(b"conditional content".to_vec()))
        .unwrap();
    let (status, body) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::CREATED);
    let md5 = body["md5"].as_str().unwrap().to_string();

    // 1. GET with If-Match: matching → 200, verify ETag header
    let (status, headers, _body) = full_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
            .header("if-match", format!("\"{md5}\""))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let etag = headers.get("etag").unwrap().to_str().unwrap();
    assert_eq!(etag, format!("\"{md5}\""));

    // 2. GET with If-Match: wrong → 412
    let (status, _, _) = full_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
            .header("if-match", "\"wrong\"")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);

    // 3. GET with If-None-Match: matching → 304
    let (status, _, _) = full_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
            .header("if-none-match", format!("\"{md5}\""))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);

    // 4. GET with If-None-Match: wrong → 200
    let (status, _, _) = full_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
            .header("if-none-match", "\"wrong\"")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 5. PUT with If-None-Match: * on existing key → 412
    let (status, _) = json_response(
        &app,
        Request::put(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "text/plain")
            .header("if-none-match", "*")
            .body(Body::from(b"new data".to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);

    // 6. PUT with If-None-Match: * on new key → 201
    let (status, _) = json_response(
        &app,
        Request::put(format!("/api/v1/buckets/{bucket_name}/blobs/new-key.txt"))
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "text/plain")
            .header("if-none-match", "*")
            .body(Body::from(b"fresh data".to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // 7. PUT with If-Match: matching → 201 (overwrite)
    let (status, _) = json_response(
        &app,
        Request::put(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "text/plain")
            .header("if-match", format!("\"{md5}\""))
            .body(Body::from(b"overwrite data".to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // 8. PUT with If-Match: wrong → 412
    let (status, _) = json_response(
        &app,
        Request::put(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "text/plain")
            .header("if-match", "\"wrong\"")
            .body(Body::from(b"should fail".to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);

    // Get the new md5 after overwrite for delete tests
    let req = Request::get(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = full_response(&app, req).await;
    let new_etag = headers.get("etag").unwrap().to_str().unwrap();
    let new_md5 = new_etag.trim_matches('"');

    // 9. DELETE with If-Match: wrong → 412
    let (status, _) = json_response(
        &app,
        Request::delete(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
            .header("authorization", format!("Bearer {key}"))
            .header("if-match", "\"wrong\"")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);

    // 10. DELETE with If-Match: matching → 204
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket_name}/blobs/cond.txt"))
                .header("authorization", format!("Bearer {key}"))
                .header("if-match", format!("\"{new_md5}\""))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

// ---------------------------------------------------------------------------
// S3 conditional request tests (If-Match / If-None-Match)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s3_conditional_requests() {
    let (s3, ak, _tmp) = test_s3_with_account().await;

    s3.create_bucket(s3_req(
        CreateBucketInput {
            bucket: "cond".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    // Put an object and capture ETag
    let body = StreamingBlob::from(s3s::Body::from(b"hello cond".to_vec()));
    let put_resp = s3
        .put_object(s3_req(
            PutObjectInput {
                bucket: "cond".into(),
                key: "obj.txt".into(),
                body: Some(body),
                content_type: Some("text/plain".into()),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    let etag = put_resp.output.e_tag.unwrap();
    let wrong_etag = ETag::Strong("0000000000000000000000000000dead".into());

    // 1. get_object with if_match: matching → OK
    s3.get_object(s3_req(
        GetObjectInput {
            bucket: "cond".into(),
            key: "obj.txt".into(),
            if_match: Some(ETagCondition::ETag(etag.clone())),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    // 2. get_object with if_match: wrong → PreconditionFailed
    let err = s3
        .get_object(s3_req(
            GetObjectInput {
                bucket: "cond".into(),
                key: "obj.txt".into(),
                if_match: Some(ETagCondition::ETag(wrong_etag.clone())),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::PreconditionFailed);

    // 3. get_object with if_none_match: matching → NotModified
    let err = s3
        .get_object(s3_req(
            GetObjectInput {
                bucket: "cond".into(),
                key: "obj.txt".into(),
                if_none_match: Some(ETagCondition::ETag(etag.clone())),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::NotModified);

    // 4. get_object with if_none_match: wrong → OK
    s3.get_object(s3_req(
        GetObjectInput {
            bucket: "cond".into(),
            key: "obj.txt".into(),
            if_none_match: Some(ETagCondition::ETag(wrong_etag.clone())),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    // 5. head_object with if_match: matching → OK
    s3.head_object(s3_req(
        HeadObjectInput {
            bucket: "cond".into(),
            key: "obj.txt".into(),
            if_match: Some(ETagCondition::ETag(etag.clone())),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    // 6. head_object with if_match: wrong → PreconditionFailed
    let err = s3
        .head_object(s3_req(
            HeadObjectInput {
                bucket: "cond".into(),
                key: "obj.txt".into(),
                if_match: Some(ETagCondition::ETag(wrong_etag.clone())),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::PreconditionFailed);

    // 7. head_object with if_none_match: matching → NotModified
    let err = s3
        .head_object(s3_req(
            HeadObjectInput {
                bucket: "cond".into(),
                key: "obj.txt".into(),
                if_none_match: Some(ETagCondition::ETag(etag.clone())),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::NotModified);

    // 8. put_object with if_match: matching → OK (overwrite)
    let body = StreamingBlob::from(s3s::Body::from(b"overwritten".to_vec()));
    s3.put_object(s3_req(
        PutObjectInput {
            bucket: "cond".into(),
            key: "obj.txt".into(),
            body: Some(body),
            if_match: Some(ETagCondition::ETag(etag.clone())),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    // 9. put_object with if_match: wrong → PreconditionFailed
    let body = StreamingBlob::from(s3s::Body::from(b"should fail".to_vec()));
    let err = s3
        .put_object(s3_req(
            PutObjectInput {
                bucket: "cond".into(),
                key: "obj.txt".into(),
                body: Some(body),
                if_match: Some(ETagCondition::ETag(wrong_etag.clone())),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::PreconditionFailed);

    // 10. put_object with if_none_match: * on existing key → PreconditionFailed
    let body = StreamingBlob::from(s3s::Body::from(b"should fail".to_vec()));
    let err = s3
        .put_object(s3_req(
            PutObjectInput {
                bucket: "cond".into(),
                key: "obj.txt".into(),
                body: Some(body),
                if_none_match: Some(ETagCondition::Any),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::PreconditionFailed);

    // 11. put_object with if_none_match: * on new key → OK
    let body = StreamingBlob::from(s3s::Body::from(b"brand new".to_vec()));
    s3.put_object(s3_req(
        PutObjectInput {
            bucket: "cond".into(),
            key: "new-obj.txt".into(),
            body: Some(body),
            if_none_match: Some(ETagCondition::Any),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    // 12. delete_object with if_match: wrong → PreconditionFailed
    let err = s3
        .delete_object(s3_req(
            DeleteObjectInput {
                bucket: "cond".into(),
                key: "obj.txt".into(),
                if_match: Some(ETagCondition::ETag(wrong_etag.clone())),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::PreconditionFailed);

    // 13. delete_object with if_match: matching → OK
    // First get the current etag after overwrite
    let head = s3
        .head_object(s3_req(
            HeadObjectInput {
                bucket: "cond".into(),
                key: "obj.txt".into(),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    let current_etag = head.output.e_tag.unwrap();

    s3.delete_object(s3_req(
        DeleteObjectInput {
            bucket: "cond".into(),
            key: "obj.txt".into(),
            if_match: Some(ETagCondition::ETag(current_etag)),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    // 14. get_object with if_match on missing key → PreconditionFailed (not NoSuchKey)
    // After deleting, the key doesn't exist. With if_match, we should get NoSuchKey
    // because the key lookup happens before the condition check.
    let err = s3
        .get_object(s3_req(
            GetObjectInput {
                bucket: "cond".into(),
                key: "obj.txt".into(),
                if_match: Some(ETagCondition::Any),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::NoSuchKey);
}

// ---------------------------------------------------------------------------
// Admin API (admin-key authenticated) integration tests
// ---------------------------------------------------------------------------

/// Create an app and issue an admin key for it. Returns (app_id, admin_key bearer token).
async fn create_test_app_admin_key(pool: &db::DbPool) -> (String, String) {
    let app = db::apps::create_app(pool, "test-app", "test@example.com")
        .await
        .unwrap();
    let raw = auth::generate_api_key();
    let hash = auth::hash_api_key(&raw);
    let prefix = auth::key_prefix(&raw);
    db::app_admin_keys::create_admin_key(pool, &app.id, &hash, &prefix, &raw)
        .await
        .unwrap();
    (app.id.to_string(), raw)
}

/// Issue an additional admin key for an existing app.
async fn issue_admin_key_for(pool: &db::DbPool, app_id: &str) -> (String, String) {
    let app_id: oyster::AppId = app_id.parse().unwrap();
    let raw = auth::generate_api_key();
    let hash = auth::hash_api_key(&raw);
    let prefix = auth::key_prefix(&raw);
    let created = db::app_admin_keys::create_admin_key(pool, &app_id, &hash, &prefix, &raw)
        .await
        .unwrap();
    (created.id, raw)
}

/// Create an account via the admin endpoint, returns (account_id, api_key bearer token).
async fn create_admin_account(app: &Router, admin_key: &str) -> (String, String) {
    let (status, body) = json_response(
        app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let account_id = body["account_id"].as_str().unwrap().to_string();
    let bearer = body["api_key"]["bearer_token"]
        .as_str()
        .unwrap()
        .to_string();
    (account_id, bearer)
}

#[tokio::test]
async fn admin_create_account() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;

    let (status, body) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["account_id"].as_str().is_some());
    assert!(body["api_key"]["bearer_token"].as_str().is_some());
}

#[tokio::test]
async fn admin_create_account_with_name() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;

    let (status, body) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name": "custom"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let account_id: AccountId = body["account_id"].as_str().unwrap().parse().unwrap();
    let account = db::accounts::get_account(&pool, &account_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.name, "custom");
}

#[tokio::test]
async fn admin_create_account_unauthorized() {
    let (app, _tmp, _pool) = test_app().await;

    let (status, _) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_create_account_api_key_works() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let (_account_id, api_key) = create_admin_account(&app, &admin_key).await;

    // The API key from admin account creation should work for normal operations.
    let bucket_name = create_test_bucket(&app, &api_key, "admin-bucket").await;
    assert!(!bucket_name.is_empty());
}

#[tokio::test]
async fn admin_create_and_revoke_api_key() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let (account_id, _api_key) = create_admin_account(&app, &admin_key).await;

    // Create an API key via admin route.
    let (status, body) = json_response(
        &app,
        Request::post(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let key_id = body["id"].as_str().unwrap().to_string();

    // Revoke it.
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/accounts/{account_id}/api-keys/{key_id}"))
                .header("authorization", format!("Bearer {admin_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Revoking again returns 404.
    let (status, _) = json_response(
        &app,
        Request::delete(format!("/api/v1/accounts/{account_id}/api-keys/{key_id}"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_access_key_crud() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let (account_id, _api_key) = create_admin_account(&app, &admin_key).await;

    // Create an access key.
    let (status, body) = json_response(
        &app,
        Request::post(format!("/api/v1/accounts/{account_id}/access-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let access_key_id = body["access_key_id"].as_str().unwrap().to_string();
    assert!(body["secret_access_key"].as_str().is_some());

    // List — should have 1.
    let (status, body) = json_response(
        &app,
        Request::get(format!("/api/v1/accounts/{account_id}/access-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let keys = body.as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["access_key_id"].as_str().unwrap(), access_key_id);

    // Delete.
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!(
                "/api/v1/accounts/{account_id}/access-keys/{access_key_id}"
            ))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // List — key should show revoked_at (still returned, but inactive).
    let (status, body) = json_response(
        &app,
        Request::get(format!("/api/v1/accounts/{account_id}/access-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let keys = body.as_array().unwrap();
    let active: Vec<_> = keys.iter().filter(|k| k["revoked_at"].is_null()).collect();
    assert_eq!(active.len(), 0);
}

#[tokio::test]
async fn admin_access_key_limit() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let (account_id, _api_key) = create_admin_account(&app, &admin_key).await;

    // Create 3 access keys (the maximum).
    for _ in 0..3 {
        let (status, _) = json_response(
            &app,
            Request::post(format!("/api/v1/accounts/{account_id}/access-keys"))
                .header("authorization", format!("Bearer {admin_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    // 4th should be rejected with 409.
    let (status, body) = json_response(
        &app,
        Request::post(format!("/api/v1/accounts/{account_id}/access-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("limit"));
}

#[tokio::test]
async fn admin_cross_app_isolation() {
    let (app, _tmp, pool) = test_app().await;

    // Create two different apps, each with its own admin key.
    let (_app_a_id, admin_key_a) = create_test_app_admin_key(&pool).await;

    let app_b = db::apps::create_app(&pool, "app-b", "b@example.com")
        .await
        .unwrap();
    let raw_b = auth::generate_api_key();
    let hash_b = auth::hash_api_key(&raw_b);
    let prefix_b = auth::key_prefix(&raw_b);
    db::app_admin_keys::create_admin_key(&pool, &app_b.id, &hash_b, &prefix_b, &raw_b)
        .await
        .unwrap();
    let admin_key_b = raw_b;

    // App A creates an account.
    let (account_id, _api_key) = create_admin_account(&app, &admin_key_a).await;

    // App B tries to create an API key for App A's account → 403.
    let (status, _) = json_response(
        &app,
        Request::post(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key_b}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // App B tries to list access keys for App A's account → 403.
    let (status, _) = json_response(
        &app,
        Request::get(format!("/api/v1/accounts/{account_id}/access-keys"))
            .header("authorization", format!("Bearer {admin_key_b}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_issue_admin_key_then_authenticates() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;

    let (status, _) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn admin_revoked_key_rejected() {
    let (app, _tmp, pool) = test_app().await;
    let (app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let (key_id, second_admin_key) = issue_admin_key_for(&pool, &app_id).await;

    // Revoke the second key.
    let revoked = db::app_admin_keys::revoke_admin_key(&pool, &key_id)
        .await
        .unwrap();
    assert!(revoked);

    // The revoked key is rejected.
    let (status, _) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {second_admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The original key still works.
    let (status, _) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn admin_unknown_key_rejected() {
    let (app, _tmp, _pool) = test_app().await;
    let bogus = auth::generate_api_key();

    let (status, _) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {bogus}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_no_authorization_header_rejected() {
    let (app, _tmp, _pool) = test_app().await;

    let (status, _) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_wrong_format_authorization_rejected() {
    let (app, _tmp, _pool) = test_app().await;

    let (status, _) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", "Bearer not-hex")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_two_keys_per_app_both_work() {
    let (app, _tmp, pool) = test_app().await;
    let (app_id, key_a) = create_test_app_admin_key(&pool).await;
    let (_key_b_id, key_b) = issue_admin_key_for(&pool, &app_id).await;

    for key in [&key_a, &key_b] {
        let (status, _) = json_response(
            &app,
            Request::post("/api/v1/accounts")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
}

#[tokio::test]
async fn admin_revoke_one_keeps_other_live() {
    let (app, _tmp, pool) = test_app().await;
    let (app_id, key_a) = create_test_app_admin_key(&pool).await;
    let (key_b_id, key_b) = issue_admin_key_for(&pool, &app_id).await;

    // Revoke key A. We need its id, so look it up by hash.
    let key_a_record = db::app_admin_keys::find_active_by_hash(&pool, &auth::hash_api_key(&key_a))
        .await
        .unwrap()
        .unwrap();
    db::app_admin_keys::revoke_admin_key(&pool, &key_a_record.id)
        .await
        .unwrap();

    // Key A no longer authenticates.
    let (status, _) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {key_a}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Key B still authenticates.
    let (status, _) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {key_b}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Cleanup variable use.
    let _ = key_b_id;
}

#[tokio::test]
async fn admin_list_accounts_returns_summaries() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;

    // Account 1: default name, just the auto-issued initial API key.
    let (account1_id, _api_key1) = create_admin_account(&app, &admin_key).await;

    // Account 2: custom name + an extra API key (so 2 active keys).
    let (status, body) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name": "two-keys"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let account2_id = body["account_id"].as_str().unwrap().to_string();

    let (status, _) = json_response(
        &app,
        Request::post(format!("/api/v1/accounts/{account2_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = json_response(
        &app,
        Request::get("/api/v1/accounts")
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let summaries = body.as_array().unwrap();
    assert_eq!(summaries.len(), 2);
    // Look up by id rather than relying on created_at order — SQLite stores
    // timestamps at second resolution so two accounts created back-to-back
    // can tie on created_at.
    let s1 = summaries
        .iter()
        .find(|s| s["id"].as_str() == Some(&account1_id))
        .expect("account1 summary present");
    let s2 = summaries
        .iter()
        .find(|s| s["id"].as_str() == Some(&account2_id))
        .expect("account2 summary present");
    assert_eq!(s1["active_api_key_count"].as_i64().unwrap(), 1);
    assert_eq!(s2["name"].as_str().unwrap(), "two-keys");
    assert_eq!(s2["active_api_key_count"].as_i64().unwrap(), 2);
}

#[tokio::test]
async fn admin_list_accounts_unauthorized() {
    let (app, _tmp, _pool) = test_app().await;

    let (status, _) = json_response(
        &app,
        Request::get("/api/v1/accounts")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_list_api_keys_returns_metadata() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let (account_id, _api_key) = create_admin_account(&app, &admin_key).await;

    // Mint an extra key.
    let (status, _) = json_response(
        &app,
        Request::post(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = json_response(
        &app,
        Request::get(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 2);
    for entry in entries {
        assert!(entry.get("id").and_then(|v| v.as_str()).is_some());
        assert!(entry.get("prefix").and_then(|v| v.as_str()).is_some());
        assert!(entry.get("note").and_then(|v| v.as_str()).is_some());
        assert!(entry.get("created_at").and_then(|v| v.as_str()).is_some());
        // Wire format must never expose the secret or hash.
        assert!(entry.get("bearer_token").is_none());
        assert!(entry.get("key_hash").is_none());
    }
}

#[tokio::test]
async fn admin_list_api_keys_foreign_account() {
    let (app, _tmp, pool) = test_app().await;

    let (_app_a_id, admin_key_a) = create_test_app_admin_key(&pool).await;

    let app_b = db::apps::create_app(&pool, "list-app-b", "lb@example.com")
        .await
        .unwrap();
    let raw_b = auth::generate_api_key();
    let hash_b = auth::hash_api_key(&raw_b);
    let prefix_b = auth::key_prefix(&raw_b);
    db::app_admin_keys::create_admin_key(&pool, &app_b.id, &hash_b, &prefix_b, &raw_b)
        .await
        .unwrap();
    let admin_key_b = raw_b;

    let (account_id, _api_key) = create_admin_account(&app, &admin_key_a).await;

    let (status, _) = json_response(
        &app,
        Request::get(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key_b}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_list_api_keys_missing_account() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let bogus_id = AccountId::new();

    let (status, _) = json_response(
        &app,
        Request::get(format!("/api/v1/accounts/{bogus_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_api_key_limit() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let (account_id, _api_key) = create_admin_account(&app, &admin_key).await;

    // Account is auto-created with 1 key. Mint 2 more for a total of 3.
    for _ in 0..2 {
        let (status, _) = json_response(
            &app,
            Request::post(format!("/api/v1/accounts/{account_id}/api-keys"))
                .header("authorization", format!("Bearer {admin_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    // 4th should be rejected with 409.
    let (status, body) = json_response(
        &app,
        Request::post(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("limit"));
}

#[tokio::test]
async fn admin_api_key_limit_excludes_revoked() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let (account_id, _api_key) = create_admin_account(&app, &admin_key).await;

    // Saturate to the cap (1 from account creation + 2 here = 3).
    let mut last_id = String::new();
    for _ in 0..2 {
        let (status, body) = json_response(
            &app,
            Request::post(format!("/api/v1/accounts/{account_id}/api-keys"))
                .header("authorization", format!("Bearer {admin_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        last_id = body["id"].as_str().unwrap().to_string();
    }

    // Revoke one — opens up a slot.
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/accounts/{account_id}/api-keys/{last_id}"))
                .header("authorization", format!("Bearer {admin_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Mint should now succeed with 201.
    let (status, _) = json_response(
        &app,
        Request::post(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn admin_create_account_with_note() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;

    let (status, body) = json_response(
        &app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"note": "ci-key"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let account_id = body["account_id"].as_str().unwrap().to_string();

    let (status, body) = json_response(
        &app,
        Request::get(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["note"].as_str().unwrap(), "ci-key");
}

#[tokio::test]
async fn admin_create_api_key_with_note() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let (account_id, _api_key) = create_admin_account(&app, &admin_key).await;

    let (status, _) = json_response(
        &app,
        Request::post(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"note": "deploy"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = json_response(
        &app,
        Request::get(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let notes: Vec<_> = entries
        .iter()
        .map(|e| e["note"].as_str().unwrap().to_string())
        .collect();
    assert!(notes.contains(&"deploy".to_string()));
}

#[tokio::test]
async fn admin_default_note_is_api() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let (account_id, _api_key) = create_admin_account(&app, &admin_key).await;

    let (status, body) = json_response(
        &app,
        Request::get(format!("/api/v1/accounts/{account_id}/api-keys"))
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["note"].as_str().unwrap(), "api");
}

/// Unreachable blob-store errors (connect/timeout to the Walrus aggregator)
/// surface as 502 Bad Gateway, not 500 Internal Server Error.
#[tokio::test]
async fn blob_store_unreachable_maps_to_502() {
    use axum::response::IntoResponse;
    use oyster::error::AppError;

    let err: AppError = BlobStoreError::Unreachable("connection refused".into()).into();
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

/// A malformed blob_id surfaced by a backing store as `InvalidBlobId`
/// must map to 400 (not 500). Guards against regressing the
/// `DirectWalrusBlobStore::read` parse-failure path back to
/// `BlobStoreError::Upstream` (502) or `Internal` (500).
#[tokio::test]
async fn blob_store_invalid_blob_id_maps_to_400() {
    use axum::response::IntoResponse;
    use oyster::error::AppError;

    let err: AppError = BlobStoreError::InvalidBlobId("not a walrus blob id".into()).into();
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Unmatched `/api/v1/...` paths return a clean 404 instead of falling through
/// to the S3 handler (which would choke on the Bearer Authorization header and
/// return a confusing 400).
#[tokio::test]
async fn unmatched_api_v1_path_returns_404() {
    let (app, _tmp, pool) = test_app().await;
    let (_account_id, key) = create_test_account(&pool).await;

    let req = Request::post("/api/v1/account/api-keys")
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let (status, body) = json_response(&app, req).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "unmatched /api/v1/... should 404, got body: {body}"
    );
}

// ---------------------------------------------------------------------------
// Blob tags (REST)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn blob_tags_crud_happy_path() {
    let (app, _tmp, pool) = test_app().await;
    let (_, api_key) = create_test_account(&pool).await;
    let bucket = create_test_bucket(&app, &api_key, "tags-happy").await;
    let (blob_key, _) = store_test_blob(&app, &api_key, &bucket, "a.txt", "text/plain", b"a").await;

    // PUT full set
    let (status, _) = json_response(
        &app,
        Request::put(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"tags":{"env":"prod","team":"platform"}}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // GET shows exactly that set
    let (status, body) = json_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tags"]["env"], "prod");
    assert_eq!(body["tags"]["team"], "platform");
    assert_eq!(body["tags"].as_object().unwrap().len(), 2);

    // PATCH merge
    let (status, _) = json_response(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"tags":{"team":"sre","owner":"alice"}}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = json_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(body["tags"]["env"], "prod");
    assert_eq!(body["tags"]["team"], "sre");
    assert_eq!(body["tags"]["owner"], "alice");

    // PUT single tag
    let resp = app
        .clone()
        .oneshot(
            Request::put(format!(
                "/api/v1/buckets/{bucket}/blobs/{blob_key}/tags/env"
            ))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "text/plain")
            .body(Body::from("staging"))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let (_, body) = json_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(body["tags"]["env"], "staging");

    // DELETE single tag
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!(
                "/api/v1/buckets/{bucket}/blobs/{blob_key}/tags/env"
            ))
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let (_, body) = json_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(body["tags"].get("env").is_none());

    // DELETE all
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let (_, body) = json_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(body["tags"].as_object().unwrap().len(), 0);
}

#[tokio::test]
async fn blob_tags_initial_set_via_header() {
    let (app, _tmp, pool) = test_app().await;
    let (_, api_key) = create_test_account(&pool).await;
    let bucket = create_test_bucket(&app, &api_key, "tags-hdr").await;

    let req = Request::put(format!("/api/v1/buckets/{bucket}/blobs/hdr.txt"))
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "text/plain")
        .header("x-oyster-tag", "env=prod")
        .header("x-oyster-tag", "team=platform")
        .body(Body::from(b"payload".to_vec()))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, body) = json_response(
        &app,
        Request::get(format!("/api/v1/buckets/{bucket}/blobs/hdr.txt/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(body["tags"]["env"], "prod");
    assert_eq!(body["tags"]["team"], "platform");
}

#[tokio::test]
async fn blob_tags_reject_over_limit() {
    let (app, _tmp, pool) = test_app().await;
    let (_, api_key) = create_test_account(&pool).await;
    let bucket = create_test_bucket(&app, &api_key, "tags-over").await;
    let (blob_key, _) = store_test_blob(&app, &api_key, &bucket, "o.txt", "text/plain", b"x").await;

    let mut entries = Vec::new();
    for i in 0..11 {
        entries.push(format!(r#""k{i}":"v""#));
    }
    let body_str = format!(r#"{{"tags":{{{}}}}}"#, entries.join(","));

    let (status, _) = json_response(
        &app,
        Request::put(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .body(Body::from(body_str))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn blob_tags_reject_long_key() {
    let (app, _tmp, pool) = test_app().await;
    let (_, api_key) = create_test_account(&pool).await;
    let bucket = create_test_bucket(&app, &api_key, "tags-lk").await;
    let (blob_key, _) = store_test_blob(&app, &api_key, &bucket, "l.txt", "text/plain", b"x").await;

    let long = "a".repeat(129);
    let body_str = format!(r#"{{"tags":{{"{long}":"v"}}}}"#);
    let (status, _) = json_response(
        &app,
        Request::put(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .body(Body::from(body_str))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn blob_tags_reject_long_value() {
    let (app, _tmp, pool) = test_app().await;
    let (_, api_key) = create_test_account(&pool).await;
    let bucket = create_test_bucket(&app, &api_key, "tags-lv").await;
    let (blob_key, _) = store_test_blob(&app, &api_key, &bucket, "l.txt", "text/plain", b"x").await;

    let long = "a".repeat(257);
    let body_str = format!(r#"{{"tags":{{"k":"{long}"}}}}"#);
    let (status, _) = json_response(
        &app,
        Request::put(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .body(Body::from(body_str))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn blob_tags_reject_total_bytes() {
    let (app, _tmp, pool) = test_app().await;
    let (_, api_key) = create_test_account(&pool).await;
    let bucket = create_test_bucket(&app, &api_key, "tags-tb").await;
    let (blob_key, _) = store_test_blob(&app, &api_key, &bucket, "t.txt", "text/plain", b"x").await;

    // 9 entries × ~240-byte values → >2 KiB total.
    let mut entries = Vec::new();
    for i in 0..9 {
        entries.push(format!(r#""key{i:02}":"{}""#, "v".repeat(240)));
    }
    let body_str = format!(r#"{{"tags":{{{}}}}}"#, entries.join(","));

    let (status, _) = json_response(
        &app,
        Request::put(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .body(Body::from(body_str))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn blob_tags_reject_bad_char() {
    let (app, _tmp, pool) = test_app().await;
    let (_, api_key) = create_test_account(&pool).await;
    let bucket = create_test_bucket(&app, &api_key, "tags-bc").await;
    let (blob_key, _) = store_test_blob(&app, &api_key, &bucket, "b.txt", "text/plain", b"x").await;

    let (status, _) = json_response(
        &app,
        Request::put(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"tags":{"bad!":"v"}}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn blob_tags_reject_duplicate_keys_via_header() {
    // BTreeMap-based PUT body silently dedups duplicate JSON keys, so we
    // drive this path through the `x-oyster-tag` multi-value header instead,
    // which delivers duplicates verbatim to `validate_tag_set`.
    let (app, _tmp, pool) = test_app().await;
    let (_, api_key) = create_test_account(&pool).await;
    let bucket = create_test_bucket(&app, &api_key, "tags-dup").await;

    let req = Request::put(format!("/api/v1/buckets/{bucket}/blobs/dup.txt"))
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "text/plain")
        .header("x-oyster-tag", "env=prod")
        .header("x-oyster-tag", "env=staging")
        .body(Body::from(b"x".to_vec()))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn blob_tags_cascade_on_delete() {
    use oyster::AccountId;

    let (app, _tmp, pool) = test_app().await;
    let (account_id_str, api_key) = create_test_account(&pool).await;
    let account_id: AccountId = account_id_str.parse().unwrap();
    let bucket = create_test_bucket(&app, &api_key, "tags-cascade").await;

    let req = Request::put(format!("/api/v1/buckets/{bucket}/blobs/c.txt"))
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "text/plain")
        .header("x-oyster-tag", "env=prod")
        .body(Body::from(b"x".to_vec()))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::CREATED);

    // Sanity: tag is stored.
    let tags = db::blob_tags::list_tags(&pool, &account_id, &bucket, "c.txt")
        .await
        .unwrap();
    assert_eq!(tags.get("env"), Some(&"prod".to_string()));

    // Delete the blob; FK cascade should wipe tags.
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/buckets/{bucket}/blobs/c.txt"))
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let tags_after = db::blob_tags::list_tags(&pool, &account_id, &bucket, "c.txt")
        .await
        .unwrap();
    assert!(
        tags_after.is_empty(),
        "expected cascade to remove tags, got {tags_after:?}"
    );
}

#[tokio::test]
async fn blob_tags_public_get_unaffected() {
    // The unauthenticated `read_blob` handler must never surface tag data
    // or change content-type behaviour.
    let (app, _tmp, pool) = test_app().await;
    let (_, api_key) = create_test_account(&pool).await;
    let bucket = create_test_bucket(&app, &api_key, "tags-public").await;

    let req = Request::put(format!("/api/v1/buckets/{bucket}/blobs/p.png"))
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "image/png")
        .header("x-oyster-tag", "env=prod")
        .body(Body::from(b"\x89PNG fake".to_vec()))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::CREATED);

    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/buckets/{bucket}/blobs/p.png"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "image/png"
    );
    // No tag-related response header is added by the public path.
    for name in resp.headers().keys() {
        let n = name.as_str().to_lowercase();
        assert!(
            !n.starts_with("x-oyster-tag") && !n.starts_with("x-amz-tagging"),
            "unexpected tag-related header on public GET: {n}"
        );
    }
}

#[tokio::test]
async fn blob_tags_auth_required() {
    let (app, _tmp, pool) = test_app().await;
    let (_, api_key) = create_test_account(&pool).await;
    let bucket = create_test_bucket(&app, &api_key, "tags-auth").await;
    let (blob_key, _) = store_test_blob(&app, &api_key, &bucket, "a.txt", "text/plain", b"x").await;

    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}/tags"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Blob tags (S3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s3_put_object_with_amz_tagging() {
    let (s3, ak, _tmp) = test_s3_with_account().await;
    s3.create_bucket(s3_req(
        CreateBucketInput {
            bucket: "tags".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    let body = StreamingBlob::from(s3s::Body::from(b"hello".to_vec()));
    s3.put_object(s3_req(
        PutObjectInput {
            bucket: "tags".into(),
            key: "obj.txt".into(),
            body: Some(body),
            content_type: Some("text/plain".into()),
            tagging: Some("env=prod&team=platform".into()),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    let resp = s3
        .get_object_tagging(s3_req(
            GetObjectTaggingInput {
                bucket: "tags".into(),
                key: "obj.txt".into(),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();

    let mut got: Vec<(String, String)> = resp
        .output
        .tag_set
        .into_iter()
        .map(|t| (t.key.unwrap_or_default(), t.value.unwrap_or_default()))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("env".to_string(), "prod".to_string()),
            ("team".to_string(), "platform".to_string()),
        ]
    );
}

#[tokio::test]
async fn s3_put_get_delete_object_tagging() {
    let (s3, ak, _tmp) = test_s3_with_account().await;
    s3.create_bucket(s3_req(
        CreateBucketInput {
            bucket: "tags".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    let body = StreamingBlob::from(s3s::Body::from(b"hi".to_vec()));
    s3.put_object(s3_req(
        PutObjectInput {
            bucket: "tags".into(),
            key: "o.txt".into(),
            body: Some(body),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    // PutObjectTagging full replace
    s3.put_object_tagging(s3_req(
        PutObjectTaggingInput {
            bucket: "tags".into(),
            checksum_algorithm: None,
            content_md5: None,
            expected_bucket_owner: None,
            key: "o.txt".into(),
            request_payer: None,
            tagging: Tagging {
                tag_set: vec![
                    Tag {
                        key: Some("env".into()),
                        value: Some("prod".into()),
                    },
                    Tag {
                        key: Some("team".into()),
                        value: Some("platform".into()),
                    },
                ],
            },
            version_id: None,
        },
        &ak,
    ))
    .await
    .unwrap();

    let resp = s3
        .get_object_tagging(s3_req(
            GetObjectTaggingInput {
                bucket: "tags".into(),
                key: "o.txt".into(),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    assert_eq!(resp.output.tag_set.len(), 2);

    s3.delete_object_tagging(s3_req(
        DeleteObjectTaggingInput {
            bucket: "tags".into(),
            key: "o.txt".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    let resp = s3
        .get_object_tagging(s3_req(
            GetObjectTaggingInput {
                bucket: "tags".into(),
                key: "o.txt".into(),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    assert_eq!(resp.output.tag_set.len(), 0);
}

#[tokio::test]
async fn s3_get_object_tag_count() {
    let (s3, ak, _tmp) = test_s3_with_account().await;
    s3.create_bucket(s3_req(
        CreateBucketInput {
            bucket: "tags".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    let body = StreamingBlob::from(s3s::Body::from(b"hi".to_vec()));
    s3.put_object(s3_req(
        PutObjectInput {
            bucket: "tags".into(),
            key: "o.txt".into(),
            body: Some(body),
            tagging: Some("a=1&b=2&c=3".into()),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    let resp = s3
        .get_object(s3_req(
            GetObjectInput {
                bucket: "tags".into(),
                key: "o.txt".into(),
                ..Default::default()
            },
            &ak,
        ))
        .await
        .unwrap();
    assert_eq!(resp.output.tag_count, Some(3));
}

#[tokio::test]
async fn s3_put_object_tagging_rejects_invalid_charset() {
    let (s3, ak, _tmp) = test_s3_with_account().await;
    s3.create_bucket(s3_req(
        CreateBucketInput {
            bucket: "tags".into(),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    let body = StreamingBlob::from(s3s::Body::from(b"hi".to_vec()));
    s3.put_object(s3_req(
        PutObjectInput {
            bucket: "tags".into(),
            key: "o.txt".into(),
            body: Some(body),
            ..Default::default()
        },
        &ak,
    ))
    .await
    .unwrap();

    let err = s3
        .put_object_tagging(s3_req(
            PutObjectTaggingInput {
                bucket: "tags".into(),
                checksum_algorithm: None,
                content_md5: None,
                expected_bucket_owner: None,
                key: "o.txt".into(),
                request_payer: None,
                tagging: Tagging {
                    tag_set: vec![Tag {
                        key: Some("bad!".into()),
                        value: Some("v".into()),
                    }],
                },
                version_id: None,
            },
            &ak,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), &s3s::S3ErrorCode::InvalidTag);
}

// ---------------------------------------------------------------------------
// Self-service webhook URL endpoints (admin-key authenticated)
// ---------------------------------------------------------------------------

/// Variant of `test_app()` with the production setting
/// `allow_http_webhook_scheme = false`. Used to verify that release builds
/// reject `http://` URLs.
async fn test_app_https_only() -> (Router, TempDir, db::DbPool) {
    let tmp = TempDir::new().unwrap();
    let blob_path = tmp.path().join("blobs");

    let config = Config {
        bind_addr: "unused".into(),
        database_url: "sqlite::memory:".into(),
        blob_store_path: blob_path.clone(),
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),

        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,

        pool_initial_epochs_ahead: 5,
        pool_initial_encoded_capacity_bytes: BYTES_PER_UNIT_SIZE,
        pool_extend_epochs: 5,
        pool_extend_lookahead_epochs: 7,
        extension_idle_sleep_secs: 30,
        extension_busy_sleep_ms: 250,
        extension_claim_batch_size: 100,
        extension_claim_cooldown_secs: 60,
        extension_metrics_bind_addr: "unused".into(),
        allow_http_webhook_scheme: false,
    };

    let pool = db::create_pool(&config.database_url).await.unwrap();
    let blob_store = oyster::blob_store::LocalBlobStore::new(blob_path)
        .await
        .unwrap();

    let state = AppState {
        db: pool.clone(),
        blob_store: Arc::new(blob_store),
        pearl: None,
        config,
        metrics_handle: None,
    };

    (routes::build_router(state), tmp, pool)
}

#[tokio::test]
async fn admin_set_webhook_url_works() {
    use base64::Engine as _;

    let (app, _tmp, pool) = test_app().await;
    let (app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let app_id_typed: oyster::AppId = app_id.parse().unwrap();

    let (status, body) = json_response(
        &app,
        Request::put("/api/v1/admin/app/webhook")
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"webhook_url":"http://localhost:1/hook"}"#.to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["webhook_url"].as_str().unwrap(),
        "http://localhost:1/hook"
    );
    let pubkey_b64 = body["webhook_public_key"].as_str().unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(pubkey_b64)
        .unwrap();
    assert_eq!(decoded.len(), 32);

    // DB row carries the same public key.
    let stored = db::apps::get_app(&pool, &app_id_typed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.webhook_public_key.as_deref(), Some(pubkey_b64));
    assert_eq!(
        stored.webhook_url.as_deref(),
        Some("http://localhost:1/hook")
    );

    // Audit row written.
    let events = db::audit_events::list_audit_events_by_app(&pool, &app_id_typed)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "webhook.url_set");
    let parsed: serde_json::Value = serde_json::from_str(&events[0].event_data).unwrap();
    assert_eq!(parsed["host"], "localhost");
    assert!(parsed["public_key_fingerprint"].as_str().unwrap().len() == 16);
}

#[tokio::test]
async fn admin_set_webhook_url_rotates_keypair() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;

    let (status, body1) = json_response(
        &app,
        Request::put("/api/v1/admin/app/webhook")
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"webhook_url":"http://localhost:1/hook"}"#.to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pk1 = body1["webhook_public_key"].as_str().unwrap().to_string();

    let (status, body2) = json_response(
        &app,
        Request::put("/api/v1/admin/app/webhook")
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"webhook_url":"http://localhost:1/hook"}"#.to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pk2 = body2["webhook_public_key"].as_str().unwrap().to_string();

    assert_ne!(pk1, pk2, "second PUT should rotate the keypair");
}

#[tokio::test]
async fn admin_clear_webhook_url_works() {
    let (app, _tmp, pool) = test_app().await;
    let (app_id, admin_key) = create_test_app_admin_key(&pool).await;
    let app_id_typed: oyster::AppId = app_id.parse().unwrap();

    // Pre-set so clear has something to clear.
    let (status, _) = json_response(
        &app,
        Request::put("/api/v1/admin/app/webhook")
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"webhook_url":"http://localhost:1/hook"}"#.to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = json_response(
        &app,
        Request::delete("/api/v1/admin/app/webhook")
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["webhook_url"].is_null());
    assert!(body["webhook_public_key"].is_null());

    // All three columns are NULL via direct DB read.
    use sqlx::Row;
    let row = sqlx::query(&db::sql(
        "SELECT webhook_url, webhook_public_key, webhook_private_key FROM apps WHERE id = ?",
    ))
    .bind(&app_id_typed)
    .fetch_one(&pool)
    .await
    .unwrap();
    let url: Option<String> = row.get("webhook_url");
    let pubk: Option<String> = row.get("webhook_public_key");
    let privk: Option<String> = row.get("webhook_private_key");
    assert!(url.is_none());
    assert!(pubk.is_none());
    assert!(privk.is_none());

    // Audit log records both events.
    let events = db::audit_events::list_audit_events_by_app(&pool, &app_id_typed)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "webhook.url_set");
    assert_eq!(events[1].event_type, "webhook.url_cleared");
}

#[tokio::test]
async fn admin_get_app_returns_public_key() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;

    let (status, set_body) = json_response(
        &app,
        Request::put("/api/v1/admin/app/webhook")
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"webhook_url":"http://localhost:1/hook"}"#.to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pk = set_body["webhook_public_key"].as_str().unwrap().to_string();

    let (status, body) = json_response(
        &app,
        Request::get("/api/v1/admin/app")
            .header("authorization", format!("Bearer {admin_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["webhook_public_key"].as_str().unwrap(), pk);
    assert_eq!(
        body["webhook_url"].as_str().unwrap(),
        "http://localhost:1/hook"
    );
}

#[tokio::test]
async fn admin_set_webhook_url_unauthenticated() {
    let (app, _tmp, _pool) = test_app().await;

    let (status, _) = json_response(
        &app,
        Request::put("/api/v1/admin/app/webhook")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"webhook_url":"http://localhost:1/hook"}"#.to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_set_webhook_url_invalid() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;

    let oversized = format!("https://example.com/{}", "a".repeat(2049));
    let cases: &[&str] = &[
        "not a url",
        "ftp://x",
        "https://user:pw@example.com",
        oversized.as_str(),
        "",
        "https://",
    ];

    for case in cases {
        let body = serde_json::json!({ "webhook_url": case }).to_string();
        let (status, _) = json_response(
            &app,
            Request::put("/api/v1/admin/app/webhook")
                .header("authorization", format!("Bearer {admin_key}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "expected 400 for {case:?}");
    }
}

#[tokio::test]
async fn admin_set_webhook_url_isolated_per_app() {
    let (app, _tmp, pool) = test_app().await;
    let (_app_a_id, admin_key_a) = create_test_app_admin_key(&pool).await;

    let app_b = db::apps::create_app(&pool, "iso-app-b", "b@example.com")
        .await
        .unwrap();
    let raw_b = auth::generate_api_key();
    let hash_b = auth::hash_api_key(&raw_b);
    let prefix_b = auth::key_prefix(&raw_b);
    db::app_admin_keys::create_admin_key(&pool, &app_b.id, &hash_b, &prefix_b, &raw_b)
        .await
        .unwrap();

    // App A sets a webhook.
    let (status, body_a) = json_response(
        &app,
        Request::put("/api/v1/admin/app/webhook")
            .header("authorization", format!("Bearer {admin_key_a}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"webhook_url":"http://localhost:1/a"}"#.to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pk_a = body_a["webhook_public_key"].as_str().unwrap().to_string();

    // App B sets a different webhook.
    let (status, body_b) = json_response(
        &app,
        Request::put("/api/v1/admin/app/webhook")
            .header("authorization", format!("Bearer {raw_b}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"webhook_url":"http://localhost:1/b"}"#.to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pk_b = body_b["webhook_public_key"].as_str().unwrap().to_string();

    // App A still sees its own URL + key after App B's update.
    let (status, body_a_after) = json_response(
        &app,
        Request::get("/api/v1/admin/app")
            .header("authorization", format!("Bearer {admin_key_a}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body_a_after["webhook_url"].as_str().unwrap(),
        "http://localhost:1/a"
    );
    assert_eq!(body_a_after["webhook_public_key"].as_str().unwrap(), pk_a);
    assert_ne!(pk_a, pk_b);
}

#[tokio::test]
async fn admin_set_webhook_url_http_rejected_in_default_config() {
    let (app, _tmp, pool) = test_app_https_only().await;
    let (_app_id, admin_key) = create_test_app_admin_key(&pool).await;

    let (status, _) = json_response(
        &app,
        Request::put("/api/v1/admin/app/webhook")
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"webhook_url":"http://localhost:1/hook"}"#.to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn webhook_delivery_includes_signature() {
    use std::sync::{Arc, Mutex};

    use axum::{Router, body::Bytes, extract::State, http::HeaderMap, routing::post};
    use base64::Engine as _;
    use ed25519_dalek::{Verifier, VerifyingKey};
    use oyster::{
        FundingAmount,
        webhook::{EVENT_TYPE_FUNDING_REQUIRED, FundingRequiredPayload, WebhookClient},
    };

    #[derive(Clone, Default)]
    struct Captured {
        body: Vec<u8>,
        sig: String,
        fp: String,
    }
    let store: Arc<Mutex<Option<Captured>>> = Arc::new(Mutex::new(None));

    async fn handler(
        State(store): State<Arc<Mutex<Option<Captured>>>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> StatusCode {
        let sig = headers
            .get("X-Oyster-Signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let fp = headers
            .get("X-Oyster-Public-Key-Fingerprint")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        *store.lock().unwrap() = Some(Captured {
            body: body.to_vec(),
            sig,
            fp,
        });
        StatusCode::OK
    }

    let receiver_app: Router = Router::new()
        .route("/hook", post(handler))
        .with_state(store.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, receiver_app).await.unwrap();
    });

    // Generate a keypair and build a WebhookClient pointing at the local server.
    let (sk, pk) = oyster::webhook_keys::generate_keypair();
    let url = format!("http://{addr}/hook");
    let client = WebhookClient::new(url, sk, pk);

    let payload = FundingRequiredPayload {
        event_id: uuid::Uuid::nil(),
        event_type: EVENT_TYPE_FUNDING_REQUIRED,
        account_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
        pearl_address: "0xabc".into(),
        amount: FundingAmount {
            wal_frost: 1,
            sui_mist: 2,
        },
        timestamp: chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
    };
    client.notify_funding_required(&payload).await;

    let captured = store
        .lock()
        .unwrap()
        .clone()
        .expect("receiver should have captured a delivery");

    // Body matches what we serialize.
    let expected_body = serde_json::to_vec(&payload).unwrap();
    assert_eq!(captured.body, expected_body);

    // Fingerprint header is the first 8 bytes of the public key, hex-encoded.
    assert_eq!(captured.fp, hex::encode(&pk[..8]));

    // X-Oyster-Signature parses as `ed25519=<base64>` and verifies.
    let sig_b64 = captured
        .sig
        .strip_prefix("ed25519=")
        .expect("signature header should start with ed25519=");
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(sig_b64)
        .unwrap();
    let signature = ed25519_dalek::Signature::from_bytes(
        &sig_bytes.as_slice().try_into().expect("64-byte signature"),
    );
    let vk = VerifyingKey::from_bytes(&pk).unwrap();
    vk.verify(&captured.body, &signature)
        .expect("signature verifies");
}
