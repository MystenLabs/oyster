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

// ---------------------------------------------------------------------------
// SpyBlobStore – wraps LocalBlobStore and records account_id from store()
// ---------------------------------------------------------------------------

type DeleteCall = (String, Option<String>, AccountId);

struct SpyBlobStore {
    inner: LocalBlobStore,
    /// Each `store()` call appends the `account_id` argument here.
    calls: Mutex<Vec<AccountId>>,
    /// Each `delete()` call appends (blob_id, sui_object_id, account_id) here.
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
        sui_object_id: Option<&str>,
        account_id: &AccountId,
    ) -> BoxFuture<'_, Result<(), BlobStoreError>> {
        self.delete_calls.lock().unwrap().push((
            blob_id.0.clone(),
            sui_object_id.map(|s| s.to_string()),
            *account_id,
        ));
        self.inner.delete(blob_id, sui_object_id, account_id)
    }

    fn exists(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<bool, BlobStoreError>> {
        self.inner.exists(blob_id)
    }
}

/// Build a fresh app with an in-memory SQLite DB and a temp blob store directory.
/// Returns `(Router, TempDir)` — hold onto the TempDir so it isn't dropped mid-test.
async fn test_app() -> (Router, TempDir) {
    let tmp = TempDir::new().unwrap();
    let blob_path = tmp.path().join("blobs");

    let config = Config {
        bind_addr: "unused".into(),
        database_url: "sqlite::memory:".into(),
        blob_store_path: blob_path.clone(),
        enable_debug_endpoints: true,
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),

        walrus_aggregator_url: None,
        walrus_default_epochs: 5,
        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,

        blob_extend_interval_secs: 3600,
        blob_extend_lookahead_days: 7,
        blob_extend_epochs: 5,
        extension_metrics_bind_addr: "unused".into(),
        fund_manager_webhook_url: None,
    };

    let pool = db::create_pool(&config.database_url).await.unwrap();
    let blob_store = LocalBlobStore::new(blob_path).await.unwrap();

    let state = AppState {
        db: pool,
        blob_store: Arc::new(blob_store),
        pearl: None,
        config,
        metrics_handle: None,
    };

    (routes::build_router(state), tmp)
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
        enable_debug_endpoints: true,
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),

        walrus_aggregator_url: None,
        walrus_default_epochs: 5,
        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,

        blob_extend_interval_secs: 3600,
        blob_extend_lookahead_days: 7,
        blob_extend_epochs: 5,
        extension_metrics_bind_addr: "unused".into(),
        fund_manager_webhook_url: None,
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

/// Helper: create an account directly via DB, returns the raw API key secret.
async fn create_test_account_via_db(pool: &db::DbPool) -> String {
    let account = db::accounts::create_account(pool).await.unwrap();
    let raw_key = auth::generate_api_key();
    let key_hash = auth::hash_api_key(&raw_key);
    let prefix = auth::key_prefix(&raw_key);
    db::api_keys::create_api_key(pool, &account.id, &key_hash, &prefix, &raw_key)
        .await
        .unwrap();
    raw_key
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

/// Helper: create an account via the debug endpoint, returns (account_id, api_key_secret).
async fn create_test_account(app: &Router) -> (String, String) {
    let req = Request::post("/debug/create-account")
        .body(Body::empty())
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, StatusCode::CREATED);
    let account_id = body["account_id"].as_str().unwrap().to_string();
    let secret = body["api_key"]["secret"].as_str().unwrap().to_string();
    (account_id, secret)
}

/// Helper: create a bucket, returns the bucket name.
async fn create_test_bucket(app: &Router, api_key: &str, name: &str) -> String {
    let req = Request::post("/buckets")
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
    let req = Request::put(format!("/buckets/{bucket_name}/blobs/{key}"))
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", content_type)
        .body(Body::from(data.to_vec()))
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(
        body.get("sui_object_id").is_some(),
        "response should include sui_object_id field"
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
    let (app, _tmp) = test_app().await;
    let req = Request::get("/health").body(Body::empty()).unwrap();
    let (status, body) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn ready_returns_ok_without_pearl() {
    let (app, _tmp) = test_app().await;
    let req = Request::get("/ready").body(Body::empty()).unwrap();
    let (status, body) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ready"], true);
}

#[tokio::test]
async fn full_lifecycle() {
    let (app, _tmp) = test_app().await;

    // 1. Create account
    let (_account_id, key) = create_test_account(&app).await;

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
        Request::get(format!("/buckets/{bucket_name}/blobs/{blob_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"hello oyster");

    // 5. Read blob by blob_id (no auth)
    let (status, body) = raw_response(
        &app,
        Request::get(format!("/blobs/by-blob-id/{blob_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"hello oyster");

    // 6. List blobs in bucket
    let (status, body) = json_response(
        &app,
        Request::get(format!("/buckets/{bucket_name}/blobs"))
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let blobs = body["data"].as_array().unwrap();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0]["key"].as_str().unwrap(), blob_key);
    assert!(blobs[0].get("sui_object_id").is_some());
    assert!(blobs[0].get("md5").is_some());

    // 7. Update blob metadata
    let (status, body) = json_response(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/buckets/{bucket_name}/blobs/{blob_key}/metadata"))
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
            Request::delete(format!("/buckets/{bucket_name}/blobs/{blob_key}"))
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
        Request::get(format!("/buckets/{bucket_name}/blobs/{blob_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 10. Delete bucket
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/buckets/{bucket_name}"))
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
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;

    let cases = vec![
        ("PUT", "/account/billing"),
        ("GET", "/account/report"),
        ("POST", "/account/transfer"),
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
    let (app, _tmp) = test_app().await;

    // Endpoints that require auth should reject unauthenticated requests.
    let cases = vec![
        Request::post("/account/api-keys")
            .body(Body::empty())
            .unwrap(),
        Request::post("/buckets")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"x"}"#))
            .unwrap(),
        Request::get("/buckets").body(Body::empty()).unwrap(),
    ];

    for req in cases {
        let (status, _) = json_response(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // Bad bearer token
    let req = Request::get("/buckets")
        .header("authorization", "Bearer bogus_key_value")
        .body(Body::empty())
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn duplicate_bucket_name_conflict() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;

    create_test_bucket(&app, &key, "dup-name").await;

    // Second bucket with same name should conflict
    let req = Request::post("/buckets")
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"dup-name"}"#))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn different_accounts_same_bucket_name() {
    let (app, _tmp) = test_app().await;
    let (_, key1) = create_test_account(&app).await;
    let (_, key2) = create_test_account(&app).await;

    create_test_bucket(&app, &key1, "shared-name").await;

    // Bucket names are globally unique — different account cannot reuse the name
    let req = Request::post("/buckets")
        .header("authorization", format!("Bearer {key2}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"shared-name"}"#))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn not_found_cases() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;
    let bucket_name = create_test_bucket(&app, &key, "nf-test").await;

    // Read non-existent blob
    let (status, _) = raw_response(
        &app,
        Request::get(format!("/buckets/{bucket_name}/blobs/nonexistent-key"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Read non-existent blob by blob_id
    let (status, _) = raw_response(
        &app,
        Request::get(
            "/blobs/by-blob-id/0000000000000000000000000000000000000000000000000000000000000000",
        )
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Delete non-existent blob
    let (status, _) = json_response(
        &app,
        Request::delete(format!("/buckets/{bucket_name}/blobs/nonexistent-key"))
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Delete non-existent bucket
    let (status, _) = json_response(
        &app,
        Request::delete("/buckets/nonexistent-id")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn store_blob_to_nonexistent_bucket() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;

    let req = Request::put("/buckets/nonexistent-bucket/blobs/test.txt")
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/octet-stream")
        .body(Body::from(b"data".to_vec()))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn content_addressed_dedup() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;
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
            Request::delete(format!("/buckets/{bucket_name}/blobs/{key1}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let (status, body) = raw_response(
        &app,
        Request::get(format!("/buckets/{bucket_name}/blobs/{key2}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, data);
}

#[tokio::test]
async fn api_key_create_and_revoke() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;

    // Create a second API key
    let (status, body) = json_response(
        &app,
        Request::post("/account/api-keys")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let new_key_id = body["id"].as_str().unwrap().to_string();
    let new_secret = body["secret"].as_str().unwrap().to_string();

    // New key works
    let (status, _) = json_response(
        &app,
        Request::get("/buckets")
            .header("authorization", format!("Bearer {new_secret}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Revoke it
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/account/api-keys/{new_key_id}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Revoked key no longer works
    let (status, _) = json_response(
        &app,
        Request::get("/buckets")
            .header("authorization", format!("Bearer {new_secret}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bucket_pagination() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;

    // Create 3 buckets
    for i in 0..3 {
        create_test_bucket(&app, &key, &format!("bucket-{i}")).await;
    }

    // Fetch with limit=2
    let (status, body) = json_response(
        &app,
        Request::get("/buckets?limit=2")
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
        Request::get(format!("/buckets?limit=2&cursor={cursor}"))
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
async fn delete_bucket_cascades_blobs() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;
    let bucket_name = create_test_bucket(&app, &key, "cascade-test").await;

    let (blob_key, _) = store_test_blob(
        &app,
        &key,
        &bucket_name,
        "cascade.bin",
        "application/octet-stream",
        b"cascade me",
    )
    .await;

    // Delete the bucket
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/buckets/{bucket_name}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Blob should be gone
    let (status, _) = raw_response(
        &app,
        Request::get(format!("/buckets/{bucket_name}/blobs/{blob_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cross_account_isolation() {
    let (app, _tmp) = test_app().await;
    let (_, key1) = create_test_account(&app).await;
    let (_, key2) = create_test_account(&app).await;

    let bucket_name = create_test_bucket(&app, &key1, "private").await;

    // Account 2 cannot store to a bucket they don't own (not found).
    let req = Request::put(format!("/buckets/{bucket_name}/blobs/intruder.txt"))
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
            Request::delete(format!("/buckets/{bucket_name}"))
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
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;
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
            Request::get(format!("/buckets/{bucket_name}/blobs/{blob_key}"))
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
async fn wallet_returns_not_provisioned_in_local_mode() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;

    let (status, body) = json_response(
        &app,
        Request::get("/account/wallet")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["provisioned"].as_bool(), Some(false));
    assert!(body["wallet"].is_null());
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
/// Returns `(Router, TempDir)` — hold onto the TempDir so it isn't dropped mid-test.
async fn test_app_with_pearl() -> (Router, TempDir) {
    let pearl = start_pearl().await;
    let tmp = TempDir::new().unwrap();
    let blob_path = tmp.path().join("blobs");

    let config = Config {
        bind_addr: "unused".into(),
        database_url: "sqlite::memory:".into(),
        blob_store_path: blob_path.clone(),
        enable_debug_endpoints: true,
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),

        walrus_aggregator_url: None,
        walrus_default_epochs: 5,
        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,
        blob_extend_interval_secs: 3600,
        blob_extend_lookahead_days: 7,
        blob_extend_epochs: 5,
        extension_metrics_bind_addr: "unused".into(),
        fund_manager_webhook_url: None,
    };

    let pool = db::create_pool(&config.database_url).await.unwrap();
    let blob_store = LocalBlobStore::new(blob_path).await.unwrap();

    let state = AppState {
        db: pool,
        blob_store: Arc::new(blob_store),
        pearl: Some(pearl),
        config,
        metrics_handle: None,
    };

    (routes::build_router(state), tmp)
}

#[tokio::test]
async fn wallet_with_pearl_returns_address() {
    let (app, _tmp) = test_app_with_pearl().await;

    // Create account via debug endpoint — Pearl is connected so it provisions a wallet.
    let (_account_id, api_key) = create_test_account(&app).await;

    let (status, body) = json_response(
        &app,
        Request::get("/account/wallet")
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["provisioned"].as_bool(), Some(true));

    let address = body["wallet"]["address"].as_str().unwrap();
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
    let key = create_test_account_via_db(&pool).await;
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

    let key_a = create_test_account_via_db(&pool).await;
    let key_b = create_test_account_via_db(&pool).await;

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
    let key = create_test_account_via_db(&pool).await;
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
            Request::delete(format!("/buckets/{bucket_name}/blobs/{blob_key}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let delete_calls = spy.recorded_delete_calls();
    assert_eq!(delete_calls.len(), 1, "expected exactly one delete call");
    let (ref _blob_id, ref sui_object_id, ref _account_id) = delete_calls[0];
    // sui_object_id is None because LocalBlobStore::store() returns StoreResult { sui_object_id: None }.
    assert_eq!(*sui_object_id, None);
}

// ---------------------------------------------------------------------------
// Metrics endpoint tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_endpoint_without_setup_returns_not_found() {
    let (app, _tmp) = test_app().await;
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
        enable_debug_endpoints: true,
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),
        walrus_aggregator_url: None,
        walrus_default_epochs: 5,
        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,
        blob_extend_interval_secs: 3600,
        blob_extend_lookahead_days: 7,
        blob_extend_epochs: 5,
        extension_metrics_bind_addr: "unused".into(),
        fund_manager_webhook_url: None,
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
// Access key CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn access_key_crud() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;

    // Create an access key
    let (status, body) = json_response(
        &app,
        Request::post("/account/access-keys")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let access_key_id = body["access_key_id"].as_str().unwrap().to_string();
    assert!(access_key_id.starts_with("OYAK"));
    assert_eq!(access_key_id.len(), 20);
    assert!(body["secret_access_key"].as_str().is_some());
    assert_eq!(body["secret_access_key"].as_str().unwrap().len(), 40);

    // List — should contain the key
    let (status, body) = json_response(
        &app,
        Request::get("/account/access-keys")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let keys = body.as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["access_key_id"].as_str().unwrap(), access_key_id);
    // Secret must NOT be in list response
    assert!(keys[0].get("secret_access_key").is_none());

    // Delete
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/account/access-keys/{access_key_id}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // List — key should show revoked_at
    let (status, body) = json_response(
        &app,
        Request::get("/account/access-keys")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let keys = body.as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert!(keys[0]["revoked_at"].as_str().is_some());
}

#[tokio::test]
async fn access_key_limit() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;

    // Create 3 access keys (the maximum)
    for _ in 0..3 {
        let (status, _) = json_response(
            &app,
            Request::post("/account/access-keys")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    // 4th should be rejected with 409
    let (status, body) = json_response(
        &app,
        Request::post("/account/access-keys")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("limit"));
}

#[tokio::test]
async fn access_key_cross_account_isolation() {
    let (app, _tmp) = test_app().await;
    let (_, key_a) = create_test_account(&app).await;
    let (_, key_b) = create_test_account(&app).await;

    // Account A creates an access key
    let (status, body) = json_response(
        &app,
        Request::post("/account/access-keys")
            .header("authorization", format!("Bearer {key_a}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let access_key_id = body["access_key_id"].as_str().unwrap().to_string();

    // Account B cannot delete account A's access key
    let (status, _) = json_response(
        &app,
        Request::delete(format!("/account/access-keys/{access_key_id}"))
            .header("authorization", format!("Bearer {key_b}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Account B's list should be empty
    let (status, body) = json_response(
        &app,
        Request::get("/account/access-keys")
            .header("authorization", format!("Bearer {key_b}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
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
        enable_debug_endpoints: true,
        pearl_grpc_url: None,
        pearl_service_secret: "test-secret".into(),
        walrus_aggregator_url: None,
        walrus_default_epochs: 5,
        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,
        blob_extend_interval_secs: 3600,
        blob_extend_lookahead_days: 7,
        blob_extend_epochs: 5,
        extension_metrics_bind_addr: "unused".into(),
        fund_manager_webhook_url: None,
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

    let account = db::accounts::create_account(&pool).await.unwrap();
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
