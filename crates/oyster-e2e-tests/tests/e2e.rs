#![allow(missing_docs)]

use std::str::FromStr;

use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use oyster_e2e_tests::{OysterTestHarness, run_e2e};
use serde_json::Value;
use tower::ServiceExt;

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
/// signing → sliver upload to storage nodes → certification → direct storage-node read.
#[test]
fn e2e_blob_lifecycle() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        // 1. Create an account via admin key (provisions a Pearl wallet automatically).
        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-blob-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &admin_key).await;

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

        // The response should include pooled_blob_object_id since we're using DirectWalrusBlobStore.
        let pooled_blob_object_id = store_body["pooled_blob_object_id"].as_str();
        assert!(
            pooled_blob_object_id.is_some(),
            "pooled_blob_object_id should be present in direct walrus mode"
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

        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-dedup-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &admin_key).await;
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

        // Create account via admin key — Pearl is connected so it should provision a wallet.
        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-wallet-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &admin_key).await;

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
        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-reserved-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &admin_key).await;

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

        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-deterministic-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &admin_key).await;
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

/// Helper: create an account via the admin endpoint.
async fn create_test_account_via_admin(app: &Router, admin_key: &str) -> (String, String) {
    let req = Request::post("/api/v1/accounts")
        .header("authorization", format!("Bearer {admin_key}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name": "e2e-test-account"}"#))
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

/// Helper: create an account via admin with an explicit
/// `max_unencoded_bytes` cap.
async fn create_test_account_with_cap(
    app: &Router,
    admin_key: &str,
    max_unencoded_bytes: u64,
) -> (String, String) {
    let body_str =
        format!(r#"{{"name": "e2e-cap-account", "max_unencoded_bytes": {max_unencoded_bytes}}}"#);
    let req = Request::post("/api/v1/accounts")
        .header("authorization", format!("Bearer {admin_key}"))
        .header("content-type", "application/json")
        .body(Body::from(body_str))
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

        // 1. Create an app + admin key via the harness.
        let (_app_id, admin_key) = harness.create_app_admin_key("admin-e2e-app").await;

        // 2. Create an account via the admin route.
        let (_account_id, api_key) = create_test_account_via_admin(app, &admin_key).await;

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

/// Helper: PUT a blob and return the parsed JSON body.
async fn put_blob(app: &Router, api_key: &str, bucket_id: &str, key: &str, data: &[u8]) -> Value {
    let req = Request::put(format!("/api/v1/buckets/{bucket_id}/blobs/{key}"))
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/octet-stream")
        .body(Body::from(data.to_vec()))
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "store blob failed: {body}"
    );
    body
}

/// Test A — pool is lazy-created on an account's first blob upload and the
/// on-chain `StoragePool` matches DB accounting.
#[test]
fn e2e_pool_created_lazily_on_first_blob() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-lazy-pool-app").await;
        let (account_id_str, api_key) = create_test_account_via_admin(app, &admin_key).await;
        let account_id = oyster::AccountId::from_str(&account_id_str).expect("parse account id");
        fund_test_wallet(&harness, app, &api_key).await;

        // Before any upload, the account has no pool.
        let pre = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .expect("query storage pool");
        assert!(pre.is_none(), "pool should not exist before any upload");

        let bucket_id = create_test_bucket(app, &api_key, "lazy-pool-bucket").await;
        put_blob(
            app,
            &api_key,
            &bucket_id,
            "lazy.txt",
            b"lazy pool creation test",
        )
        .await;

        let state = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .expect("query storage pool")
            .expect("pool should exist after first upload");
        assert!(
            !state.object_id.is_empty(),
            "DB pool object_id should be non-empty",
        );
        assert!(
            state.used_encoded_bytes > 0,
            "DB pool_used_encoded_bytes should be non-zero after one upload",
        );

        let pool_id: oyster::sui_types::base_types::ObjectID =
            state.object_id.parse().expect("parse pool ObjectID");
        let status = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status");

        assert_eq!(status.storage_pool_object_id, pool_id);
        assert_eq!(status.blob_count, 1, "one blob should be registered");
        assert!(
            status.used_encoded_bytes > 0,
            "on-chain used_encoded_bytes should be non-zero",
        );
        assert!(
            status.reserved_encoded_capacity_bytes >= status.used_encoded_bytes,
            "reserved >= used",
        );
        assert!(
            status.end_epoch > status.start_epoch,
            "end_epoch > start_epoch",
        );
        assert_eq!(
            status.used_encoded_bytes as i64, state.used_encoded_bytes,
            "DB and on-chain used_encoded_bytes should agree",
        );
    });
}

/// Test B — `blob_count` and `used_encoded_bytes` track N distinct uploads.
#[test]
fn e2e_pool_accounting_across_n_uploads() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-n-uploads-app").await;
        let (account_id_str, api_key) = create_test_account_via_admin(app, &admin_key).await;
        let account_id = oyster::AccountId::from_str(&account_id_str).expect("parse account id");
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "n-uploads-bucket").await;

        for i in 0..3 {
            let content = format!("n-uploads-test content #{i}");
            put_blob(
                app,
                &api_key,
                &bucket_id,
                &format!("file-{i}.txt"),
                content.as_bytes(),
            )
            .await;
        }

        let state = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .expect("query storage pool")
            .expect("pool should exist");
        let pool_id: oyster::sui_types::base_types::ObjectID =
            state.object_id.parse().expect("parse pool ObjectID");

        let status = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status");

        assert_eq!(status.blob_count, 3, "three distinct blobs registered");
        assert_eq!(
            status.used_encoded_bytes as i64, state.used_encoded_bytes,
            "DB and on-chain used_encoded_bytes should agree",
        );
        assert!(
            status.reserved_encoded_capacity_bytes >= status.used_encoded_bytes,
            "reserved >= used",
        );
    });
}

/// After a first small upload, a second small upload that still fits in the
/// already-reserved MiB MUST NOT trigger another `increase_storage_pool_capacity`
/// PTB. We verify by asserting that on-chain `reserved_encoded_capacity_bytes`
/// is unchanged between the two uploads.
#[test]
fn e2e_small_upload_reuses_reserved_capacity() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-reuse-capacity-app").await;
        let (account_id_str, api_key) = create_test_account_via_admin(app, &admin_key).await;
        let account_id = oyster::AccountId::from_str(&account_id_str).expect("parse account id");
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "reuse-capacity-bucket").await;

        put_blob(
            app,
            &api_key,
            &bucket_id,
            "first.txt",
            b"first small upload",
        )
        .await;

        let state_after_first = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .expect("query storage pool")
            .expect("pool should exist after first upload");
        let pool_id: oyster::sui_types::base_types::ObjectID = state_after_first
            .object_id
            .parse()
            .expect("parse pool ObjectID");

        let status_after_first = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status");

        put_blob(
            app,
            &api_key,
            &bucket_id,
            "second.txt",
            b"second small upload",
        )
        .await;

        let status_after_second = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status");

        assert_eq!(
            status_after_second.reserved_encoded_capacity_bytes,
            status_after_first.reserved_encoded_capacity_bytes,
            "second small upload should not have triggered increase_storage_pool_capacity",
        );
        assert_eq!(status_after_second.blob_count, 2, "both blobs registered");
        assert!(
            status_after_second.used_encoded_bytes > status_after_first.used_encoded_bytes,
            "used_encoded_bytes should grow even when reserved is unchanged",
        );
    });
}

/// Test C — the extension task advances `pool_end_epoch` both on-chain and in DB.
#[test]
fn e2e_extension_task_extends_pool() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-extend-app").await;
        let (account_id_str, api_key) = create_test_account_via_admin(app, &admin_key).await;
        let account_id = oyster::AccountId::from_str(&account_id_str).expect("parse account id");
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "extend-bucket").await;

        put_blob(app, &api_key, &bucket_id, "extend.txt", b"extension test").await;

        let state = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .expect("query storage pool")
            .expect("pool should exist");
        let pool_id: oyster::sui_types::base_types::ObjectID =
            state.object_id.parse().expect("parse pool ObjectID");

        let before = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status before");

        let processed = harness.trigger_extension_cycle(7, 1).await;
        assert_eq!(processed, 1, "exactly one pool should have been processed");

        let after = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status after");

        assert_eq!(
            after.end_epoch,
            before.end_epoch + 1,
            "on-chain end_epoch should advance by 1 extend_epoch",
        );

        let state_after = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .expect("query storage pool")
            .expect("pool should exist");
        assert_eq!(
            state_after.end_epoch as u32, after.end_epoch,
            "DB end_epoch should match on-chain end_epoch after extension",
        );
    });
}

/// Test D — deleting one of two references to the same content does not free
/// pool capacity; deleting the last reference calls `delete_pooled_blob` and
/// brings `used_encoded_bytes` to zero on both chain and DB.
#[test]
fn e2e_refcounted_delete_frees_pool_capacity() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-refcount-app").await;
        let (account_id_str, api_key) = create_test_account_via_admin(app, &admin_key).await;
        let account_id = oyster::AccountId::from_str(&account_id_str).expect("parse account id");
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "refcount-bucket").await;

        let data = b"shared content for refcounted delete";
        let a = put_blob(app, &api_key, &bucket_id, "copy-a", data).await;
        let b = put_blob(app, &api_key, &bucket_id, "copy-b", data).await;
        assert_eq!(
            a["blob_id"].as_str().unwrap(),
            b["blob_id"].as_str().unwrap(),
            "same content should dedup to the same blob_id",
        );

        let state = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .expect("query storage pool")
            .expect("pool should exist");
        let pool_id: oyster::sui_types::base_types::ObjectID =
            state.object_id.parse().expect("parse pool ObjectID");

        let before = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status before");
        assert_eq!(before.blob_count, 1, "dedup should produce one pooled blob");
        assert!(
            before.used_encoded_bytes > 0,
            "pool should have non-zero used bytes",
        );

        // DELETE copy-a — ref-count drops 2 → 1, no on-chain delete yet.
        let resp = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/buckets/{bucket_id}/blobs/copy-a"))
                    .header("authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);

        let mid = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status mid");
        assert_eq!(
            mid.blob_count, 1,
            "ref-count > 0 should not free the pooled blob",
        );
        assert_eq!(
            mid.used_encoded_bytes, before.used_encoded_bytes,
            "used_encoded_bytes unchanged while a reference remains",
        );

        // DELETE copy-b — ref-count drops 1 → 0, on-chain delete_pooled_blob fires.
        let resp = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/buckets/{bucket_id}/blobs/copy-b"))
                    .header("authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NO_CONTENT);

        let after = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status after");
        assert_eq!(after.blob_count, 0, "last ref dropped → pooled blob freed");
        assert_eq!(
            after.used_encoded_bytes, 0,
            "used_encoded_bytes should be zero",
        );

        let state_after = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .expect("query storage pool")
            .expect("pool should exist");
        assert_eq!(
            state_after.used_encoded_bytes, 0,
            "DB pool_used_encoded_bytes should track on-chain state",
        );
    });
}

/// Phase 2 regression: when DB-side pool accounting drifts above the
/// on-chain reservation (e.g. cross-replica race), the first
/// register_pooled_blobs PTB aborts with EInsufficientCapacity; Oyster
/// must refresh on-chain truth, reconcile the DB, and retry once.
#[test]
fn store_blob_retries_after_einsufficient_capacity_drift() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;
        let (_app_id, admin_key) = harness.create_app_admin_key("drift-test").await;
        let (account_id_str, api_key) = create_test_account_via_admin(app, &admin_key).await;
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "drift-bucket").await;

        // 1. First upload — establishes the StoragePool on-chain and in DB.
        //    Small body so the initial 1 MiB reservation isn't grown.
        let body_a = put_blob(app, &api_key, &bucket_id, "a.txt", b"hello a").await;
        let blob_id_a = body_a["blob_id"].as_str().unwrap().to_string();

        let account_id: oyster::AccountId = oyster::AccountId::from_str(&account_id_str).unwrap();
        let post_a = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .unwrap()
            .expect("pool state after blob A");
        let on_chain_reserved = post_a.reserved_encoded_bytes;
        let used_a = post_a.used_encoded_bytes;

        // 2. Drift: pretend reserved capacity is much larger than on-chain,
        //    while leaving `used` at the post-A truth. This is the
        //    cross-replica race state that confused the pre-fix code:
        //    `grow_by_bytes` reads from DB, sees plenty of headroom, and
        //    submits a register PTB with no growth.
        let inflated_reserved: i64 = on_chain_reserved + 100 * 1024 * 1024;
        oyster::db::accounts::reconcile_pool_after_drift(
            &harness.db,
            &account_id,
            inflated_reserved,
            used_a,
        )
        .await
        .expect("inflate pool_reserved_encoded_bytes");

        // 3. Second upload — a body large enough that its encoded size
        //    can't fit alongside blob A's used bytes inside the on-chain
        //    1 MiB reservation. With DB drift in place, the first
        //    register_pooled_blobs PTB must abort with EInsufficientCapacity;
        //    Oyster must then refresh on-chain state, reconcile, and
        //    retry once with a recomputed `grow_by`.
        let body_b_data = vec![b'B'; 2 * 1024 * 1024];
        let body_b = put_blob(app, &api_key, &bucket_id, "b.bin", &body_b_data).await;
        let blob_id_b = body_b["blob_id"].as_str().unwrap().to_string();
        assert_ne!(blob_id_a, blob_id_b, "blob_ids must be distinct");

        // 4. DB has been reconciled to on-chain truth + the retry's grow.
        let post_b = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .unwrap()
            .expect("pool state after blob B");
        // Used bumped by exactly one blob's encoded_size (blob B).
        assert!(
            post_b.used_encoded_bytes > used_a,
            "used must increase: {} → {}",
            used_a,
            post_b.used_encoded_bytes
        );
        // Reserved is no longer the inflated drift value — it reflects
        // the on-chain reservation plus any grow the retry applied.
        assert!(
            post_b.reserved_encoded_bytes < inflated_reserved,
            "reserved must be reconciled: {} (was drifted to {})",
            post_b.reserved_encoded_bytes,
            inflated_reserved
        );
        assert!(
            post_b.reserved_encoded_bytes >= post_b.used_encoded_bytes,
            "reserved must cover used"
        );
    });
}

/// Self-heal regression: an on-chain `PooledBlob` whose DB row was
/// dropped (e.g., a prior delete tx failed but the DB delete proceeded
/// anyway to preserve idempotent semantics) must not block a re-upload
/// of the same content. The register PTB aborts with
/// `dynamic_field::add` code 0 (`EFieldAlreadyExists`); Oyster must
/// look up the existing `PooledBlob` on-chain and return Ok, leaving
/// the on-chain `used_encoded_bytes` untouched.
#[test]
fn store_blob_self_heals_on_orphaned_pooled_blob() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;
        let (_app_id, admin_key) = harness.create_app_admin_key("self-heal-test").await;
        let (account_id_str, api_key) = create_test_account_via_admin(app, &admin_key).await;
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "self-heal-bucket").await;
        let account_id: oyster::AccountId = oyster::AccountId::from_str(&account_id_str).unwrap();

        let data = b"orphan-recovery";

        // 1. First upload registers the PooledBlob on-chain.
        let body_a = put_blob(app, &api_key, &bucket_id, "a.txt", data).await;
        let blob_id = body_a["blob_id"].as_str().unwrap().to_string();
        let original_pooled_id = body_a["pooled_blob_object_id"]
            .as_str()
            .expect("first PUT must carry a PooledBlob ObjectID")
            .to_string();

        let pool_state = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .expect("query pool")
            .expect("pool after first upload");
        let pool_id: oyster::sui_types::base_types::ObjectID =
            pool_state.object_id.parse().expect("parse pool ObjectID");
        let used_before = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status before")
            .used_encoded_bytes;

        // 2. Drop the DB row directly, leaving the on-chain PooledBlob
        //    orphaned. This is the steady-state outcome of a
        //    delete-tx-failed → DB-delete-only branch in routes/blobs.rs.
        oyster::db::blobs::delete_blob(&harness.db, &bucket_id, "a.txt", &account_id)
            .await
            .expect("DB-only delete of blob a.txt")
            .expect("row existed");
        assert!(
            oyster::db::blobs::find_pooled_blob_object_id_for_account(
                &harness.db,
                &account_id,
                &blob_id
            )
            .await
            .expect("dedup query")
            .is_none(),
            "DB dedup index must be empty before the re-upload",
        );

        // 3. Re-upload the same content under a different key. The
        //    register PTB aborts with EFieldAlreadyExists; the
        //    self-heal arm must catch it and return Ok with the
        //    existing on-chain PooledBlob ID.
        let body_b = put_blob(app, &api_key, &bucket_id, "b.txt", data).await;
        let recovered_pooled_id = body_b["pooled_blob_object_id"]
            .as_str()
            .expect("self-heal PUT must carry a PooledBlob ObjectID")
            .to_string();
        assert_eq!(
            recovered_pooled_id, original_pooled_id,
            "self-heal must return the existing on-chain PooledBlob ObjectID",
        );

        // 4. The new DB row must carry the recovered PooledBlob ID so
        //    a follow-up delete actually clears the orphan on-chain.
        let row = oyster::db::blobs::get_blob_by_key(&harness.db, &bucket_id, "b.txt")
            .await
            .expect("query b.txt")
            .expect("b.txt row exists");
        assert_eq!(
            row.pooled_blob_object_id.as_deref(),
            Some(original_pooled_id.as_str()),
            "b.txt's DB row must reference the recovered PooledBlob",
        );

        // 5. The on-chain pool's used_encoded_bytes must NOT have
        //    bumped — we recovered an existing PooledBlob, no new
        //    storage was registered.
        let used_after = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .expect("storage_pool_status after")
            .used_encoded_bytes;
        assert_eq!(
            used_after, used_before,
            "self-heal must not register additional encoded bytes",
        );
    });
}

/// An over-cap upload must be rejected with 400 *before* any Sui tx
/// is submitted. Because `new_unencoded > max_unencoded` is checked
/// before lazy-creating the on-chain `StoragePool`, no pool should
/// exist on the account afterwards either.
#[test]
fn over_cap_upload_returns_400_without_submitting_tx() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;
        let (_app_id, admin_key) = harness.create_app_admin_key("over-cap-app").await;
        let (account_id_str, api_key) =
            create_test_account_with_cap(app, &admin_key, 1_000_000).await;
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "cap-bucket").await;

        // 2 MiB body easily exceeds the 1 MB cap.
        let blob_data = vec![b'C'; 2 * 1024 * 1024];
        let req = Request::put(format!("/api/v1/buckets/{bucket_id}/blobs/too-big.bin"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/octet-stream")
            .body(Body::from(blob_data))
            .unwrap();
        let (status, body) = json_response(app, req).await;
        assert_eq!(
            status,
            axum::http::StatusCode::BAD_REQUEST,
            "expected 400, body = {body}"
        );

        // Response carries the structured `cap_exceeded` block with the
        // right numbers and an admin-endpoint hint.
        let cap = &body["cap_exceeded"];
        assert!(!cap.is_null(), "missing cap_exceeded block: {body}");
        assert_eq!(cap["max_unencoded_bytes"].as_i64(), Some(1_000_000));
        assert_eq!(cap["new_unencoded_bytes"].as_u64(), Some(2 * 1024 * 1024));
        // Used encoded bytes is 0 because the pre-pool short-circuit
        // fires before any on-chain read.
        assert_eq!(cap["used_encoded_bytes"].as_u64(), Some(0));
        assert!(
            cap["admin_endpoint"]
                .as_str()
                .is_some_and(|s| s.contains("/max-storage")),
            "missing/incorrect admin_endpoint hint: {cap}"
        );

        // No on-chain `StoragePool` was lazy-created — the rejection
        // happened before the lazy-create branch. This is the strongest
        // assertion available here: the spec's pre-pool short-circuit
        // means no Sui tx was submitted at all.
        let account_id: oyster::AccountId = oyster::AccountId::from_str(&account_id_str).unwrap();
        let pool = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .unwrap();
        assert!(
            pool.is_none(),
            "no StoragePool should be created for an over-cap first upload, got {pool:?}"
        );
    });
}

/// Helper: PUT a new max_unencoded_bytes cap via the admin endpoint.
async fn put_max_storage(
    app: &Router,
    admin_key: &str,
    account_id: &str,
    new_cap: u64,
) -> (axum::http::StatusCode, Value) {
    let body = format!(r#"{{"max_unencoded_bytes": {new_cap}}}"#);
    let req = Request::put(format!("/api/v1/accounts/{account_id}/max-storage"))
        .header("authorization", format!("Bearer {admin_key}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    json_response(app, req).await
}

/// Phase 4: an admin can lower an account's `max_unencoded_bytes`
/// below the on-chain `reserved_encoded_capacity_bytes` and the route
/// submits a Pearl-signed `decrease_storage_pool_capacity_by_size` PTB
/// that shrinks the pool on-chain. The new threshold must stay above
/// `used_encoded_bytes` so no orphaning is needed.
#[test]
fn admin_lower_cap_triggers_on_chain_shrink() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;
        let (_app_id, admin_key) = harness.create_app_admin_key("shrink-app").await;
        // Generous cap so the first upload doesn't itself trigger a shrink.
        let (account_id_str, api_key) =
            create_test_account_with_cap(app, &admin_key, 5_000_000_000).await;
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "shrink-bucket").await;

        // 1. Lazy-create the on-chain StoragePool with a small upload.
        let _ = put_blob(app, &api_key, &bucket_id, "tiny.txt", b"hello shrink").await;

        let account_id: oyster::AccountId = oyster::AccountId::from_str(&account_id_str).unwrap();
        let pool_state = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .unwrap()
            .expect("pool state after first upload");
        let pool_object_id = pool_state.object_id.parse().unwrap();
        let pre =
            oyster::sui_object_reader::read_storage_pool_state(&harness.rpc_url, pool_object_id)
                .await
                .expect("on-chain pool state");
        let reserved_pre = pre.reserved_encoded_bytes;
        let used_pre = pre.used_encoded_bytes;
        assert!(reserved_pre > used_pre, "expect free capacity to shrink");

        // 2. Pick a new (lower) cap that, after forward-encoding,
        //    fits between `used_encoded` and `reserved_encoded`.
        //    With n_shards small (test cluster) `encoded_blob_length_for_n_shards`
        //    may saturate; pick a tiny cap so the threshold lands
        //    between `used_pre` and `reserved_pre` deterministically.
        let new_cap: u64 = 1_000; // 1 KB unencoded — encoded threshold easily < reserved
        let (status, body) = put_max_storage(app, &admin_key, &account_id_str, new_cap).await;
        assert_eq!(status, axum::http::StatusCode::OK, "body={body}");
        let digest = body["shrink_tx_digest"].as_str();
        assert!(
            digest.is_some_and(|d| !d.is_empty()),
            "expected shrink_tx_digest, body={body}",
        );
        let pool_block = &body["pool"];
        assert!(!pool_block.is_null(), "expected pool block: {body}");
        let resp_reserved = pool_block["reserved_encoded_bytes"].as_u64().unwrap();
        assert!(
            (resp_reserved as i64) < (reserved_pre as i64),
            "response reserved {resp_reserved} must be < pre {reserved_pre}",
        );

        // 3. On-chain reservation actually shrank.
        let post =
            oyster::sui_object_reader::read_storage_pool_state(&harness.rpc_url, pool_object_id)
                .await
                .expect("on-chain pool state post-shrink");
        assert!(
            post.reserved_encoded_bytes < reserved_pre,
            "on-chain reserved must shrink: pre={reserved_pre}, post={}",
            post.reserved_encoded_bytes,
        );
        assert_eq!(
            post.used_encoded_bytes, used_pre,
            "used must be unchanged by shrink",
        );

        // 4. DB cap reflects the new value.
        let stored_cap = oyster::db::accounts::get_max_unencoded_bytes(&harness.db, &account_id)
            .await
            .unwrap();
        assert_eq!(stored_cap, Some(new_cap as i64));

        // 5. DB pool counters reconciled to on-chain post-shrink truth.
        let db_pool = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .unwrap()
            .expect("pool state");
        assert_eq!(
            db_pool.reserved_encoded_bytes as u64, post.reserved_encoded_bytes,
            "DB reserved must match on-chain post-shrink",
        );
        assert_eq!(
            db_pool.used_encoded_bytes as u64, post.used_encoded_bytes,
            "DB used must match on-chain post-shrink",
        );
    });
}

/// Phase 4: lowering the cap below `used_encoded_bytes` must be
/// rejected with 400 + `would_orphan` block — no on-chain shrink
/// submitted, no DB write performed.
#[test]
fn admin_lower_cap_rejects_when_would_orphan() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;
        let (_app_id, admin_key) = harness.create_app_admin_key("orphan-app").await;
        let (account_id_str, api_key) =
            create_test_account_with_cap(app, &admin_key, 5_000_000_000).await;
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "orphan-bucket").await;

        // Lazy-create the pool with a non-trivial upload so
        // `used_encoded_bytes` rises above the encoding overhead
        // floor that `f(1) = encoded_blob_length_for_n_shards(n, 1, RS2)`
        // returns on the test cluster. With a 100 KiB body, `used >
        // f(1)` holds and the orphan check fires reliably.
        let body = vec![b'O'; 100 * 1024];
        let _ = put_blob(app, &api_key, &bucket_id, "big.bin", &body).await;

        let account_id: oyster::AccountId = oyster::AccountId::from_str(&account_id_str).unwrap();
        let pool_state = oyster::db::accounts::get_storage_pool(&harness.db, &account_id)
            .await
            .unwrap()
            .expect("pool state after first upload");
        let pool_object_id = pool_state.object_id.parse().unwrap();
        let pre =
            oyster::sui_object_reader::read_storage_pool_state(&harness.rpc_url, pool_object_id)
                .await
                .expect("on-chain pool state");

        // Lower the cap to 1 byte — encoded threshold ≈ 0, certainly
        // below `used_encoded_bytes`, so the route must 400 with the
        // `would_orphan` block and never submit a shrink.
        let (status, body) = put_max_storage(app, &admin_key, &account_id_str, 1).await;
        assert_eq!(
            status,
            axum::http::StatusCode::BAD_REQUEST,
            "expected 400, body={body}",
        );
        let block = &body["would_orphan"];
        assert!(!block.is_null(), "missing would_orphan block: {body}");
        assert_eq!(block["max_unencoded_bytes"].as_i64(), Some(1));
        assert!(
            block["used_encoded_bytes"].as_i64().unwrap() > 0,
            "used_encoded_bytes should be > 0: {block}",
        );

        // On-chain pool unchanged.
        let post =
            oyster::sui_object_reader::read_storage_pool_state(&harness.rpc_url, pool_object_id)
                .await
                .expect("on-chain pool state post-rejection");
        assert_eq!(post.reserved_encoded_bytes, pre.reserved_encoded_bytes);
        assert_eq!(post.used_encoded_bytes, pre.used_encoded_bytes);

        // DB cap is unchanged (still the original 5 GB).
        let stored_cap = oyster::db::accounts::get_max_unencoded_bytes(&harness.db, &account_id)
            .await
            .unwrap();
        assert_eq!(stored_cap, Some(5_000_000_000));
    });
}

/// PUT a blob larger than the Walrus encoder's per-blob ceiling for the
/// network's `n_shards` and confirm the upload is rejected with 413 +
/// a structured `payload_too_large` block (rather than a 500 internal
/// error). Exercises the encode-step error mapping in
/// `DirectWalrusBlobStore::store_impl`.
#[test]
fn e2e_store_blob_over_encoder_ceiling_returns_413() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;
        let (_app_id, admin_key) = harness.create_app_admin_key("oversize-app").await;
        // Generous cap so the storage-cap check doesn't trip first.
        let (_account_id, api_key) =
            create_test_account_with_cap(app, &admin_key, 5_000_000_000).await;
        fund_test_wallet(&harness, app, &api_key).await;
        let bucket_id = create_test_bucket(app, &api_key, "oversize-bucket").await;

        // Look up the network's `n_shards` so we know exactly which
        // encoder ceiling to step over.
        let read_client = oyster::sui_transaction::build_sui_read_client(
            &harness.rpc_url,
            harness.system_object,
            harness.staking_object,
        )
        .await
        .expect("read client");
        use walrus_sui::client::ReadClient as _;
        let n_shards = read_client.n_shards().await.expect("n_shards");
        let max_blob = walrus_core::encoding::max_blob_size_for_n_shards(
            n_shards,
            walrus_core::EncodingType::RS2,
        );
        // Margin above the boundary for symbol-size rounding.
        let over = max_blob + 16 * 1024;

        let req = Request::put(format!("/api/v1/buckets/{bucket_id}/blobs/oversize.bin"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/octet-stream")
            .body(Body::from(vec![0u8; over as usize]))
            .unwrap();
        let (status, body) = json_response(app, req).await;
        assert_eq!(
            status,
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            "expected 413, got {status}: {body}",
        );
        let block = &body["payload_too_large"];
        assert!(!block.is_null(), "missing payload_too_large block: {body}");
        assert_eq!(
            block["unencoded_size_bytes"].as_u64(),
            Some(over),
            "body={body}",
        );
        assert_eq!(
            block["n_shards"].as_u64(),
            Some(u64::from(n_shards.get())),
            "body={body}",
        );
    });
}
