use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use oyster_e2e_tests::OysterTestHarness;
use serde_json::Value;
use tower::ServiceExt;

/// Build a tokio runtime with 32MB worker thread stacks.
/// The walrus encoding pipeline is deeply recursive and overflows the default 2MB stack.
fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(32 * 1024 * 1024)
        .build()
        .expect("build tokio runtime")
}

/// Helper: send a request and return (status, body as JSON Value).
async fn json_response(app: &Router, req: Request<Body>) -> (axum::http::StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// Helper: send a request and return (status, raw bytes).
async fn raw_response(app: &Router, req: Request<Body>) -> (axum::http::StatusCode, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

/// Helper: create a test account via the debug endpoint.
async fn create_test_account(app: &Router) -> (String, String) {
    let req = Request::post("/debug/create-account")
        .body(Body::empty())
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, axum::http::StatusCode::CREATED);
    let account_id = body["account_id"].as_str().unwrap().to_string();
    let secret = body["api_key"]["secret"].as_str().unwrap().to_string();
    (account_id, secret)
}

/// Helper: create a bucket.
async fn create_test_bucket(app: &Router, api_key: &str, name: &str) -> String {
    let req = Request::post("/buckets")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"name":"{name}"}}"#)))
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, axum::http::StatusCode::CREATED);
    body["id"].as_str().unwrap().to_string()
}

/// Full end-to-end blob lifecycle:
/// create account → create bucket → store blob → read blob → list blobs → delete blob.
///
/// This test exercises the real Walrus pipeline: encoding → on-chain registration via Pearl
/// signing → sliver upload to storage nodes → certification → aggregator read.
#[test]
#[ignore = "requires walrus test cluster (~30s startup)"]
fn e2e_blob_lifecycle() {
    build_runtime().block_on(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        // 1. Create an account (provisions a Pearl wallet automatically).
        let (_account_id, api_key) = create_test_account(app).await;

        // 2. Create a bucket.
        let bucket_id = create_test_bucket(app, &api_key, "e2e-test-bucket").await;

        // 3. Store a blob.
        let blob_data = b"Hello from the Oyster E2E test!";
        let store_req = Request::put(format!("/buckets/{bucket_id}/blobs"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "text/plain")
            .body(Body::from(blob_data.to_vec()))
            .unwrap();
        let (status, store_body) = json_response(app, store_req).await;
        assert_eq!(
            status,
            axum::http::StatusCode::CREATED,
            "store blob failed: {store_body}"
        );

        let object_id = store_body["object_id"].as_str().unwrap().to_string();
        let blob_id = store_body["blob_id"].as_str().unwrap().to_string();
        assert!(!object_id.is_empty(), "object_id should be non-empty");
        assert!(!blob_id.is_empty(), "blob_id should be non-empty");

        // The response should include sui_object_id since we're using DirectWalrusBlobStore.
        let sui_object_id = store_body["sui_object_id"].as_str();
        assert!(
            sui_object_id.is_some(),
            "sui_object_id should be present in direct walrus mode"
        );

        // 4. Read the blob back by object_id.
        let (status, body) = raw_response(
            app,
            Request::get(format!("/blobs/{object_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body, blob_data, "read blob data should match stored data");

        // 5. Read the blob by blob_id.
        let (status, body) = raw_response(
            app,
            Request::get(format!("/blobs/by-blob-id/{blob_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body, blob_data, "read by blob_id should match stored data");

        // 6. List blobs in the bucket.
        let (status, list_body) = json_response(
            app,
            Request::get(format!("/buckets/{bucket_id}/blobs"))
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let blobs = list_body["data"].as_array().unwrap();
        assert_eq!(blobs.len(), 1, "should have exactly one blob");
        assert_eq!(blobs[0]["object_id"].as_str().unwrap(), object_id);

        // 7. Delete the blob (DB record removed; on-chain deletion is a no-op for now).
        let resp = app
            .clone()
            .oneshot(
                Request::delete(format!("/blobs/{object_id}"))
                    .header("authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);

        // 8. Verify the blob is gone from Oyster's perspective.
        let (status, _) = raw_response(
            app,
            Request::get(format!("/blobs/{object_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    });
}

/// Test storing multiple blobs and verifying content-addressed dedup works through the real
/// Walrus pipeline.
#[test]
#[ignore = "requires walrus test cluster (~30s startup)"]
fn e2e_content_dedup() {
    build_runtime().block_on(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_account_id, api_key) = create_test_account(app).await;
        let bucket_id = create_test_bucket(app, &api_key, "dedup-bucket").await;

        let data = b"duplicate content for dedup test";

        // Store the same data twice.
        let make_store_req = |bucket: &str, key: &str, data: &[u8]| {
            Request::put(format!("/buckets/{bucket}/blobs"))
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/octet-stream")
                .body(Body::from(data.to_vec()))
                .unwrap()
        };

        let req1 = make_store_req(&bucket_id, &api_key, data);
        let (s1, b1) = json_response(app, req1).await;
        assert_eq!(s1, axum::http::StatusCode::CREATED);

        let req2 = make_store_req(&bucket_id, &api_key, data);
        let (s2, b2) = json_response(app, req2).await;
        assert_eq!(s2, axum::http::StatusCode::CREATED);

        // Same content should produce the same blob_id.
        assert_eq!(
            b1["blob_id"].as_str().unwrap(),
            b2["blob_id"].as_str().unwrap(),
            "same content should produce same blob_id"
        );

        // But different object_ids (different DB records).
        assert_ne!(
            b1["object_id"].as_str().unwrap(),
            b2["object_id"].as_str().unwrap(),
            "each store should create a distinct object_id"
        );
    });
}

/// Test the wallet provisioning flow through the real stack.
#[test]
#[ignore = "requires walrus test cluster (~30s startup)"]
fn e2e_wallet_provisioning() {
    build_runtime().block_on(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        // Create account — Pearl is connected so it should provision a wallet.
        let (_account_id, api_key) = create_test_account(app).await;

        // Check wallets endpoint.
        let (status, body) = json_response(
            app,
            Request::get("/account/wallets")
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["provisioned"].as_bool(), Some(true));
        let wallets = body["wallets"].as_array().unwrap();
        assert_eq!(wallets.len(), 1, "should have exactly one wallet");

        let address = wallets[0]["address"].as_str().unwrap();
        assert!(
            address.starts_with("0x"),
            "wallet address should start with 0x, got: {address}"
        );
    });
}
