use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use oyster::{AppState, blob_store::LocalBlobStore, config::Config, db, routes};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

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
        walrus_publisher_url: None,
        walrus_aggregator_url: None,
        walrus_default_epochs: 5,
        sui_rpc_url: None,
        walrus_system_object: None,
        walrus_staking_object: None,
        pearl_account_id: None,
        blob_extend_interval_secs: 3600,
        blob_extend_lookahead_days: 7,
        blob_extend_epochs: 5,
    };

    let pool = db::create_pool(&config.database_url).await.unwrap();
    let blob_store = LocalBlobStore::new(blob_path).await.unwrap();

    let state = AppState {
        db: pool,
        blob_store: Arc::new(blob_store),
        pearl: None,
        config,
    };

    (routes::build_router(state), tmp)
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
    let object_id = body["object_id"].as_str().unwrap().to_string();
    let blob_id = body["blob_id"].as_str().unwrap().to_string();
    (object_id, blob_id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Oyster–Pearl integration (4.5.3)
// ---------------------------------------------------------------------------

/// Stand up Pearl's gRPC server in-process and return a connected `PearlConnection`.
async fn start_pearl() -> oyster::pearl_client::PearlConnection {
    use pearl::{
        auth::check_service_secret,
        grpc::{PearlService, proto::pearl_server::PearlServer},
    };

    const PEARL_SECRET: &str = "oyster-pearl-test-secret";

    let db = pearl::db::create_pool("sqlite::memory:")
        .await
        .expect("in-memory pool");

    let service = PearlService { db };
    let interceptor = check_service_secret(PEARL_SECRET.to_string());
    let svc = PearlServer::with_interceptor(service, interceptor);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
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
async fn pearl_client_create_and_get_wallets() {
    let pearl = start_pearl().await;

    // Create an account through Oyster's PearlConnection wrapper.
    let create_resp = pearl.create_account(100, 200, 500, 1000).await.unwrap();
    assert!(!create_resp.account_id.is_empty());
    assert!(create_resp.address.starts_with("0x"));

    // Fetch wallets through the wrapper.
    let wallets_resp = pearl
        .get_account_wallets(&create_resp.account_id)
        .await
        .unwrap();
    assert_eq!(wallets_resp.wallets.len(), 1);
    let wallet = &wallets_resp.wallets[0];
    assert_eq!(wallet.account_id, create_resp.account_id);
    assert_eq!(wallet.address, create_resp.address);
    assert_eq!(wallet.min_sui_balance, 100);
    assert_eq!(wallet.min_wal_balance, 200);
    assert_eq!(wallet.top_up_target_sui, 500);
    assert_eq!(wallet.top_up_target_wal, 1000);
}

#[tokio::test]
async fn pearl_client_sign_transaction_success() {
    use sui_types::{
        base_types::{ObjectDigest, ObjectID, SuiAddress},
        programmable_transaction_builder::ProgrammableTransactionBuilder,
        transaction::TransactionData,
    };

    let pearl = start_pearl().await;

    let create_resp = pearl.create_account(0, 0, 0, 0).await.unwrap();

    let sender: SuiAddress = create_resp.address.parse().expect("valid SuiAddress");
    let gas_ref = (
        ObjectID::random(),
        sui_types::base_types::SequenceNumber::new(),
        ObjectDigest::random(),
    );
    let pt = ProgrammableTransactionBuilder::new().finish();
    let tx_data = TransactionData::new_programmable(sender, vec![gas_ref], pt, 5_000_000, 1_000);
    let tx_data_bytes = bcs::to_bytes(&tx_data).unwrap();

    let resp = pearl
        .sign_transaction(&create_resp.account_id, tx_data_bytes)
        .await
        .unwrap();

    assert!(!resp.signed_transaction.is_empty());

    // Verify the response deserializes back into a valid Transaction.
    let _tx: sui_types::transaction::Transaction =
        bcs::from_bytes(&resp.signed_transaction).expect("valid Transaction");
}

#[tokio::test]
async fn pearl_client_sign_transaction_invalid_tx_data() {
    let pearl = start_pearl().await;

    let create_resp = pearl.create_account(0, 0, 0, 0).await.unwrap();

    let err = pearl
        .sign_transaction(&create_resp.account_id, vec![1, 2, 3])
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}
