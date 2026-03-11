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
    AppState,
    auth,
    blob_store::{BlobId, BlobStore, BlobStoreError, LocalBlobStore, StoreResult},
    config::Config,
    db,
    routes,
};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// SpyBlobStore – wraps LocalBlobStore and records account_id from store()
// ---------------------------------------------------------------------------

type DeleteCall = (String, Option<String>, Option<String>);

struct SpyBlobStore {
    inner: LocalBlobStore,
    /// Each `store()` call appends the `account_id` argument here.
    calls: Mutex<Vec<Option<String>>>,
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

    fn recorded_calls(&self) -> Vec<Option<String>> {
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
        account_id: Option<&str>,
    ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>> {
        self.calls
            .lock()
            .unwrap()
            .push(account_id.map(|s| s.to_string()));
        self.inner.store(data, account_id)
    }

    fn read(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<Vec<u8>, BlobStoreError>> {
        self.inner.read(blob_id)
    }

    fn delete(
        &self,
        blob_id: &BlobId,
        sui_object_id: Option<&str>,
        account_id: Option<&str>,
    ) -> BoxFuture<'_, Result<(), BlobStoreError>> {
        self.delete_calls.lock().unwrap().push((
            blob_id.0.clone(),
            sui_object_id.map(|s| s.to_string()),
            account_id.map(|s| s.to_string()),
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

/// Helper: create a bucket, returns the bucket id.
async fn create_test_bucket(app: &Router, api_key: &str, name: &str) -> String {
    let req = Request::post("/buckets")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"name":"{name}"}}"#)))
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().unwrap().to_string()
}

/// Helper: store a blob, returns (object_id, blob_id).
async fn store_test_blob(
    app: &Router,
    api_key: &str,
    bucket_id: &str,
    content_type: &str,
    data: &[u8],
) -> (String, String) {
    let req = Request::put(format!("/buckets/{bucket_id}/blobs"))
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
    let object_id = body["object_id"].as_str().unwrap().to_string();
    let blob_id = body["blob_id"].as_str().unwrap().to_string();
    (object_id, blob_id)
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
    let bucket_id = create_test_bucket(&app, &key, "my-bucket").await;

    // 3. Store blob
    let (object_id, blob_id) =
        store_test_blob(&app, &key, &bucket_id, "text/plain", b"hello oyster").await;

    // 4. Read blob by object_id (no auth)
    let (status, body) = raw_response(
        &app,
        Request::get(format!("/blobs/{object_id}"))
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
        Request::get(format!("/buckets/{bucket_id}/blobs"))
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let blobs = body["data"].as_array().unwrap();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0]["object_id"].as_str().unwrap(), object_id);
    assert!(blobs[0].get("sui_object_id").is_some());

    // 7. Update blob metadata
    let (status, body) = json_response(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/blobs/{object_id}/metadata"))
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
            Request::delete(format!("/blobs/{object_id}"))
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
        Request::get(format!("/blobs/{object_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 10. Delete bucket
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/buckets/{bucket_id}"))
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

    // Different account can use the same bucket name
    let req = Request::post("/buckets")
        .header("authorization", format!("Bearer {key2}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"shared-name"}"#))
        .unwrap();
    let (status, _) = json_response(&app, req).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn not_found_cases() {
    let (app, _tmp) = test_app().await;
    let (_, key) = create_test_account(&app).await;

    // Read non-existent blob
    let (status, _) = raw_response(
        &app,
        Request::get("/blobs/nonexistent-id")
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
        Request::delete("/blobs/nonexistent-id")
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

    let req = Request::put("/buckets/nonexistent-bucket/blobs")
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
    let bucket_id = create_test_bucket(&app, &key, "dedup-test").await;

    let data = b"identical content";
    let (oid1, bid1) = store_test_blob(&app, &key, &bucket_id, "text/plain", data).await;
    let (oid2, bid2) = store_test_blob(&app, &key, &bucket_id, "text/plain", data).await;

    // Same content -> same blob_id, different object_ids
    assert_eq!(bid1, bid2);
    assert_ne!(oid1, oid2);

    // Delete one — the other should still be readable
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/blobs/{oid1}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let (status, body) = raw_response(
        &app,
        Request::get(format!("/blobs/{oid2}"))
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
    let bucket_id = create_test_bucket(&app, &key, "cascade-test").await;

    let (object_id, _) = store_test_blob(
        &app,
        &key,
        &bucket_id,
        "application/octet-stream",
        b"cascade me",
    )
    .await;

    // Delete the bucket
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/buckets/{bucket_id}"))
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
        Request::get(format!("/blobs/{object_id}"))
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

    let bucket_id = create_test_bucket(&app, &key1, "private").await;

    // Account 2 cannot list blobs in account 1's bucket (gets empty, not an error,
    // because bucket lookup is scoped to account).
    // But storing to a bucket you don't own should fail (not found).
    let req = Request::put(format!("/buckets/{bucket_id}/blobs"))
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
            Request::delete(format!("/buckets/{bucket_id}"))
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
    let bucket_id = create_test_bucket(&app, &key, "ct-test").await;

    let (object_id, _) =
        store_test_blob(&app, &key, &bucket_id, "image/png", b"\x89PNG fake").await;

    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/blobs/{object_id}"))
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

    let account_id = "test-pearl-account";

    // Fetch address through the wrapper — any account_id works now (stateless).
    let address = pearl.get_address(account_id).await.unwrap();
    assert!(address.starts_with("0x"));
    assert_eq!(
        address.len(),
        66,
        "Sui address should be 66 chars (0x + 64 hex)"
    );

    // Same account_id returns the same address (deterministic).
    let address2 = pearl.get_address(account_id).await.unwrap();
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

    let account_id = "sign-test-account";
    let address = pearl.get_address(account_id).await.unwrap();

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
        .sign_transaction(account_id, tx_data_bytes)
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
        .sign_transaction("invalid-tx-test-account", vec![1, 2, 3])
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
    let bucket_id = create_test_bucket(&app, &key, "test-bucket").await;

    store_test_blob(&app, &key, &bucket_id, "text/plain", b"data").await;

    let calls = spy.recorded_calls();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].is_some(),
        "account_id should always be passed to store()"
    );
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

    store_test_blob(&app, &key_a, &bucket_a, "text/plain", b"aaa").await;
    store_test_blob(&app, &key_b, &bucket_b, "text/plain", b"bbb").await;

    let calls = spy.recorded_calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].is_some());
    assert!(calls[1].is_some());
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
    let bucket_id = create_test_bucket(&app, &key, "delete-test").await;

    let (object_id, _blob_id) =
        store_test_blob(&app, &key, &bucket_id, "text/plain", b"delete me").await;

    // Delete the blob via the API.
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/blobs/{object_id}"))
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let delete_calls = spy.recorded_delete_calls();
    assert_eq!(delete_calls.len(), 1, "expected exactly one delete call");
    let (ref _blob_id, ref sui_object_id, ref account_id) = delete_calls[0];
    // sui_object_id is None because LocalBlobStore::store() returns StoreResult { sui_object_id: None }.
    assert_eq!(*sui_object_id, None);
    assert!(
        account_id.is_some(),
        "account_id should be threaded through to delete()"
    );
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
