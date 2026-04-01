#![allow(missing_docs)]

use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use oyster_e2e_tests::OysterTestHarness;
use serde_json::Value;
use tower::ServiceExt;

/// Run an async test body on a tokio runtime with 32MB worker thread stacks.
///
/// The walrus encoding pipeline is deeply recursive and overflows the default 2MB stack.
/// `cargo test` runs each `#[test]` on a thread with the default 2MB stack, so we must
/// ensure the heavy work runs on a runtime worker thread (which has the enlarged stack)
/// rather than on the test thread via `block_on`.
fn run_e2e<F: std::future::Future<Output = ()> + Send + 'static>(f: F) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(32 * 1024 * 1024)
        .build()
        .expect("build tokio runtime");
    rt.block_on(async {
        // Spawn onto a worker thread with the enlarged stack.
        tokio::spawn(f).await.expect("e2e test panicked");
    });
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

/// Helper: get the wallet address for an account and fund it with SUI + WAL.
async fn fund_test_wallet(harness: &OysterTestHarness, app: &Router, api_key: &str) {
    let (status, body) = json_response(
        app,
        Request::get("/api/v1/account/wallet")
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let address = body["address"].as_str().expect("wallet address");
    harness.fund_wallet(address).await;
}

/// Helper: create a bucket.
async fn create_test_bucket(app: &Router, api_key: &str, name: &str) -> String {
    let req = Request::post("/api/v1/buckets")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"name":"{name}"}}"#)))
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, axum::http::StatusCode::CREATED);
    body["name"].as_str().unwrap().to_string()
}

/// Full end-to-end blob lifecycle:
/// create account → create bucket → store blob → read blob → list blobs → delete blob.
///
/// This test exercises the real Walrus pipeline: encoding → on-chain registration via Pearl
/// signing → sliver upload to storage nodes → certification → aggregator read.
#[test]
fn e2e_blob_lifecycle() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        // 1. Create an account via admin JWT (provisions a Pearl wallet automatically).
        let (_app_id, jwt) = harness.create_app_jwt("e2e-blob-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &jwt).await;

        // 2. Fund the test account's wallet with SUI (gas) and WAL (storage).
        fund_test_wallet(&harness, app, &api_key).await;

        // 3. Create a bucket.
        let bucket_id = create_test_bucket(app, &api_key, "e2e-test-bucket").await;

        // 4. Store a blob.
        let blob_data = b"Hello from the Oyster E2E test!";
        let blob_key = "test-file.txt";
        let store_req = Request::put(format!("/api/v1/buckets/{bucket_id}/blobs/{blob_key}"))
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

        let key = store_body["key"].as_str().unwrap().to_string();
        let blob_id = store_body["blob_id"].as_str().unwrap().to_string();
        assert!(!key.is_empty(), "key should be non-empty");
        assert!(!blob_id.is_empty(), "blob_id should be non-empty");

        // The response should include sui_object_id since we're using DirectWalrusBlobStore.
        let sui_object_id = store_body["sui_object_id"].as_str();
        assert!(
            sui_object_id.is_some(),
            "sui_object_id should be present in direct walrus mode"
        );

        // 5. Read the blob back by bucket + key.
        let (status, body) = raw_response(
            app,
            Request::get(format!("/api/v1/buckets/{bucket_id}/blobs/{key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body, blob_data, "read blob data should match stored data");

        // 6. Read the blob by blob_id.
        let (status, body) = raw_response(
            app,
            Request::get(format!("/api/v1/blobs/by-blob-id/{blob_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body, blob_data, "read by blob_id should match stored data");

        // 7. List blobs in the bucket.
        let (status, list_body) = json_response(
            app,
            Request::get(format!("/api/v1/buckets/{bucket_id}/blobs"))
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let blobs = list_body["data"].as_array().unwrap();
        assert_eq!(blobs.len(), 1, "should have exactly one blob");
        assert_eq!(blobs[0]["key"].as_str().unwrap(), key);

        // 8. Delete the blob (DB record removed; on-chain blob object deleted via delete_blob PTB).
        let resp = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/buckets/{bucket_id}/blobs/{key}"))
                    .header("authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);

        // 9. Verify the blob is gone from Oyster's perspective.
        let (status, _) = raw_response(
            app,
            Request::get(format!("/api/v1/buckets/{bucket_id}/blobs/{key}"))
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
fn e2e_content_dedup() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_app_id, jwt) = harness.create_app_jwt("e2e-dedup-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &jwt).await;
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "dedup-bucket").await;

        let data = b"duplicate content for dedup test";

        // Store the same data twice under different keys.
        let make_store_req = |bucket: &str, blob_key: &str, api_key: &str, data: &[u8]| {
            Request::put(format!("/api/v1/buckets/{bucket}/blobs/{blob_key}"))
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/octet-stream")
                .body(Body::from(data.to_vec()))
                .unwrap()
        };

        let req1 = make_store_req(&bucket_id, "copy-1", &api_key, data);
        let (s1, b1) = json_response(app, req1).await;
        assert_eq!(s1, axum::http::StatusCode::CREATED);

        let req2 = make_store_req(&bucket_id, "copy-2", &api_key, data);
        let (s2, b2) = json_response(app, req2).await;
        assert_eq!(s2, axum::http::StatusCode::CREATED);

        // Same content should produce the same blob_id.
        assert_eq!(
            b1["blob_id"].as_str().unwrap(),
            b2["blob_id"].as_str().unwrap(),
            "same content should produce same blob_id"
        );

        // But different keys (different DB records).
        assert_ne!(
            b1["key"].as_str().unwrap(),
            b2["key"].as_str().unwrap(),
            "each store should have a distinct key"
        );
    });
}

/// Test the wallet provisioning flow through the real stack.
#[test]
fn e2e_wallet_provisioning() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        // Create account via admin JWT — Pearl is connected so it should provision a wallet.
        let (_app_id, jwt) = harness.create_app_jwt("e2e-wallet-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &jwt).await;

        // Check wallet endpoint.
        let (status, body) = json_response(
            app,
            Request::get("/api/v1/account/wallet")
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);

        let address = body["address"].as_str().unwrap();
        assert!(
            address.starts_with("0x"),
            "wallet address should start with 0x, got: {address}"
        );
    });
}

/// Verify that reserved bucket names (health, ready, metrics, api) are rejected
/// through the full stack (router → handler → validation).
#[test]
fn reserved_bucket_names_rejected() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;
        let (_app_id, jwt) = harness.create_app_jwt("e2e-reserved-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &jwt).await;

        // Hardcoded — must stay in sync with RESERVED_BUCKET_NAMES in validation.rs.
        let reserved = ["health", "ready", "metrics", "api"];
        for name in &reserved {
            let req = Request::post("/api/v1/buckets")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"name":"{name}"}}"#)))
                .unwrap();
            let (status, body) = json_response(app, req).await;
            assert_eq!(
                status,
                axum::http::StatusCode::BAD_REQUEST,
                "expected 400 for reserved bucket name '{name}', got {status}"
            );
            let msg = body["error"].as_str().unwrap_or("");
            assert!(
                msg.contains("reserved"),
                "error for '{name}' should mention 'reserved', got: {msg}"
            );
        }

        // Non-reserved substrings must still be allowed.
        create_test_bucket(app, &api_key, "healthy").await;
    });
}

/// Verify deterministic wallet address derivation through the full Oyster→Pearl stack.
#[test]
fn e2e_deterministic_wallet_address() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_app_id, jwt) = harness.create_app_jwt("e2e-deterministic-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &jwt).await;
        fund_test_wallet(&harness, app, &api_key).await;

        // Fetch wallet twice — should return the same address both times.
        let mut addresses = Vec::new();
        for _ in 0..2 {
            let (status, body) = json_response(
                app,
                Request::get("/api/v1/account/wallet")
                    .header("authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::OK);
            let addr = body["address"].as_str().unwrap().to_string();
            addresses.push(addr);
        }
        assert_eq!(
            addresses[0], addresses[1],
            "wallet address should be deterministic across calls"
        );

        // Store a blob, then verify the address is still the same.
        let bucket_id = create_test_bucket(app, &api_key, "determinism-bucket").await;
        let store_req = Request::put(format!("/api/v1/buckets/{bucket_id}/blobs/det-test.txt"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "text/plain")
            .body(Body::from(b"determinism test".to_vec()))
            .unwrap();
        let (status, _) = json_response(app, store_req).await;
        assert_eq!(status, axum::http::StatusCode::CREATED);

        let (status, body) = json_response(
            app,
            Request::get("/api/v1/account/wallet")
                .header("authorization", format!("Bearer {api_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let addr_after_store = body["address"].as_str().unwrap();
        assert_eq!(
            addresses[0], addr_after_store,
            "wallet address should remain stable after storing a blob"
        );
    });
}

/// Helper: create an account via the admin (JWT) endpoint.
async fn create_test_account_via_admin(app: &Router, jwt: &str) -> (String, String) {
    let req = Request::post("/api/v1/accounts")
        .header("authorization", format!("Bearer {jwt}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, axum::http::StatusCode::CREATED);
    let account_id = body["account_id"].as_str().unwrap().to_string();
    let api_key = body["api_key"]["bearer_token"]
        .as_str()
        .unwrap()
        .to_string();
    (account_id, api_key)
}

/// Admin-created account can store and read blobs end-to-end through real Walrus storage.
#[test]
fn e2e_admin_account_blob_lifecycle() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        // 1. Create an app + JWT via the harness.
        let (_app_id, jwt) = harness.create_app_jwt("admin-e2e-app").await;

        // 2. Create an account via the admin route.
        let (_account_id, api_key) = create_test_account_via_admin(app, &jwt).await;

        // 3. Fund the account's wallet.
        fund_test_wallet(&harness, app, &api_key).await;

        // 4. Create a bucket.
        let bucket_id = create_test_bucket(app, &api_key, "admin-e2e-bucket").await;

        // 5. Store a blob.
        let blob_data = b"Hello from admin-created account!";
        let blob_key = "admin-test.txt";
        let store_req = Request::put(format!("/api/v1/buckets/{bucket_id}/blobs/{blob_key}"))
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

        let key = store_body["key"].as_str().unwrap().to_string();

        // 6. Read the blob back.
        let (status, body) = raw_response(
            app,
            Request::get(format!("/api/v1/buckets/{bucket_id}/blobs/{key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body, blob_data, "read blob data should match stored data");
    });
}
