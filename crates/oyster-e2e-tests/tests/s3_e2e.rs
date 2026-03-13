#![allow(missing_docs)]

use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use oyster_e2e_tests::OysterTestHarness;
use serde_json::Value;
use tower::ServiceExt;

/// Run an async test body on a tokio runtime with 32MB worker thread stacks.
fn run_e2e<F: std::future::Future<Output = ()> + Send + 'static>(f: F) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(32 * 1024 * 1024)
        .build()
        .expect("build tokio runtime");
    rt.block_on(async {
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

/// Helper: get the wallet address for an account and fund it with SUI + WAL.
async fn fund_test_wallet(harness: &OysterTestHarness, app: &Router, api_key: &str) {
    let (status, body) = json_response(
        app,
        Request::get("/account/wallet")
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let address = body["wallet"]["address"].as_str().expect("wallet address");
    harness.fund_wallet(address).await;
}

/// Helper: create an S3 access key for the given account.
async fn create_access_key(app: &Router, api_key: &str) -> (String, String) {
    let req = Request::post("/account/access-keys")
        .header("authorization", format!("Bearer {api_key}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, axum::http::StatusCode::CREATED);
    let access_key_id = body["access_key_id"].as_str().unwrap().to_string();
    let secret_access_key = body["secret_access_key"].as_str().unwrap().to_string();
    (access_key_id, secret_access_key)
}

/// Build an S3 client with static credentials pointing at the test S3 server.
fn build_s3_client(endpoint: &str, access_key_id: &str, secret: &str) -> aws_sdk_s3::Client {
    let creds = aws_credential_types::Credentials::new(access_key_id, secret, None, None, "test");
    let config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .region(aws_sdk_s3::config::Region::new("us-east-1"))
        .endpoint_url(endpoint)
        .credentials_provider(creds)
        .force_path_style(true)
        .build();
    aws_sdk_s3::Client::from_conf(config)
}

/// Full S3 lifecycle: CreateBucket → HeadBucket → ListBuckets → PutObject → HeadObject →
/// GetObject → ListObjectsV2 → DeleteObject → verify deletion → DeleteBucket → verify deletion.
#[test]
fn s3_e2e_full_lifecycle() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        // Create account + access key via the JSON API.
        let (_account_id, api_key) = create_test_account(app).await;
        fund_test_wallet(&harness, app, &api_key).await;
        let (access_key_id, secret_access_key) = create_access_key(app, &api_key).await;

        // Start S3 server and build client.
        let s3_url = harness.serve_s3_on_random_port().await;
        let s3 = build_s3_client(&s3_url, &access_key_id, &secret_access_key);

        // 1. CreateBucket
        s3.create_bucket()
            .bucket("s3-e2e-bucket")
            .send()
            .await
            .expect("CreateBucket failed");

        // 2. HeadBucket
        s3.head_bucket()
            .bucket("s3-e2e-bucket")
            .send()
            .await
            .expect("HeadBucket failed");

        // 3. ListBuckets
        let list = s3.list_buckets().send().await.expect("ListBuckets failed");
        let names: Vec<_> = list.buckets().iter().filter_map(|b| b.name()).collect();
        assert!(
            names.contains(&"s3-e2e-bucket"),
            "bucket not in list: {names:?}"
        );

        // 4. PutObject
        let put = s3
            .put_object()
            .bucket("s3-e2e-bucket")
            .key("hello.txt")
            .content_type("text/plain")
            .body(aws_sdk_s3::primitives::ByteStream::from_static(b"Hello S3"))
            .send()
            .await
            .expect("PutObject failed");
        let put_etag = put.e_tag().expect("PutObject should return ETag");
        assert!(!put_etag.is_empty());

        // 5. HeadObject
        let head = s3
            .head_object()
            .bucket("s3-e2e-bucket")
            .key("hello.txt")
            .send()
            .await
            .expect("HeadObject failed");
        assert_eq!(head.content_length(), Some(8));
        assert_eq!(head.e_tag().unwrap(), put_etag);

        // 6. GetObject
        let get = s3
            .get_object()
            .bucket("s3-e2e-bucket")
            .key("hello.txt")
            .send()
            .await
            .expect("GetObject failed");
        assert_eq!(get.e_tag().unwrap(), put_etag);
        let body_bytes = get.body.collect().await.expect("collect body").into_bytes();
        assert_eq!(&body_bytes[..], b"Hello S3");

        // 7. ListObjectsV2
        let list_obj = s3
            .list_objects_v2()
            .bucket("s3-e2e-bucket")
            .send()
            .await
            .expect("ListObjectsV2 failed");
        assert_eq!(list_obj.key_count(), Some(1));
        let keys: Vec<_> = list_obj.contents().iter().filter_map(|o| o.key()).collect();
        assert_eq!(keys, vec!["hello.txt"]);

        // 8. DeleteObject
        s3.delete_object()
            .bucket("s3-e2e-bucket")
            .key("hello.txt")
            .send()
            .await
            .expect("DeleteObject failed");

        // 9. Verify deletion: HeadObject should fail.
        let head_err = s3
            .head_object()
            .bucket("s3-e2e-bucket")
            .key("hello.txt")
            .send()
            .await;
        assert!(head_err.is_err(), "HeadObject should fail after delete");

        // 10. DeleteBucket
        s3.delete_bucket()
            .bucket("s3-e2e-bucket")
            .send()
            .await
            .expect("DeleteBucket failed");

        // 11. Verify: HeadBucket should fail.
        let head_bucket_err = s3.head_bucket().bucket("s3-e2e-bucket").send().await;
        assert!(
            head_bucket_err.is_err(),
            "HeadBucket should fail after delete"
        );
    });
}

/// Test ListObjectsV2 with prefix and delimiter.
#[test]
fn s3_e2e_list_with_prefix_and_delimiter() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_account_id, api_key) = create_test_account(app).await;
        fund_test_wallet(&harness, app, &api_key).await;
        let (access_key_id, secret_access_key) = create_access_key(app, &api_key).await;

        let s3_url = harness.serve_s3_on_random_port().await;
        let s3 = build_s3_client(&s3_url, &access_key_id, &secret_access_key);

        s3.create_bucket()
            .bucket("prefix-bucket")
            .send()
            .await
            .unwrap();

        // Put objects with hierarchical keys.
        for key in &["docs/a.txt", "docs/b.txt", "images/cat.png", "readme.md"] {
            s3.put_object()
                .bucket("prefix-bucket")
                .key(*key)
                .body(aws_sdk_s3::primitives::ByteStream::from_static(b"data"))
                .send()
                .await
                .unwrap();
        }

        // List with prefix "docs/" — should return 2 keys.
        let list = s3
            .list_objects_v2()
            .bucket("prefix-bucket")
            .prefix("docs/")
            .send()
            .await
            .unwrap();
        assert_eq!(list.key_count(), Some(2));

        // List with delimiter "/" — should have common_prefixes for docs/ and images/,
        // and contents should have readme.md.
        let list = s3
            .list_objects_v2()
            .bucket("prefix-bucket")
            .delimiter("/")
            .send()
            .await
            .unwrap();

        let prefixes: Vec<_> = list
            .common_prefixes()
            .iter()
            .filter_map(|cp| cp.prefix())
            .collect();
        assert!(
            prefixes.contains(&"docs/"),
            "expected docs/ in common_prefixes: {prefixes:?}"
        );
        assert!(
            prefixes.contains(&"images/"),
            "expected images/ in common_prefixes: {prefixes:?}"
        );

        let keys: Vec<_> = list.contents().iter().filter_map(|o| o.key()).collect();
        assert_eq!(keys, vec!["readme.md"]);
    });
}

/// Test PutObject overwrite semantics.
#[test]
fn s3_e2e_overwrite_object() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_account_id, api_key) = create_test_account(app).await;
        fund_test_wallet(&harness, app, &api_key).await;
        let (access_key_id, secret_access_key) = create_access_key(app, &api_key).await;

        let s3_url = harness.serve_s3_on_random_port().await;
        let s3 = build_s3_client(&s3_url, &access_key_id, &secret_access_key);

        s3.create_bucket()
            .bucket("overwrite-bucket")
            .send()
            .await
            .unwrap();

        // Put v1.
        s3.put_object()
            .bucket("overwrite-bucket")
            .key("file.txt")
            .body(aws_sdk_s3::primitives::ByteStream::from_static(b"v1"))
            .send()
            .await
            .unwrap();

        // Put v2 (overwrite).
        s3.put_object()
            .bucket("overwrite-bucket")
            .key("file.txt")
            .body(aws_sdk_s3::primitives::ByteStream::from_static(b"v2"))
            .send()
            .await
            .unwrap();

        // GetObject should return v2.
        let get = s3
            .get_object()
            .bucket("overwrite-bucket")
            .key("file.txt")
            .send()
            .await
            .unwrap();
        let body = get.body.collect().await.unwrap().into_bytes();
        assert_eq!(&body[..], b"v2");

        // ListObjectsV2 should show exactly 1 object.
        let list = s3
            .list_objects_v2()
            .bucket("overwrite-bucket")
            .send()
            .await
            .unwrap();
        assert_eq!(list.key_count(), Some(1));
    });
}

/// Test S3 error responses for nonexistent resources.
#[test]
fn s3_e2e_error_cases() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (_account_id, api_key) = create_test_account(app).await;
        let (access_key_id, secret_access_key) = create_access_key(app, &api_key).await;

        let s3_url = harness.serve_s3_on_random_port().await;
        let s3 = build_s3_client(&s3_url, &access_key_id, &secret_access_key);

        // Create a bucket so we can test object-level errors.
        s3.create_bucket()
            .bucket("error-bucket")
            .send()
            .await
            .unwrap();

        // GetObject on nonexistent key.
        let err = s3
            .get_object()
            .bucket("error-bucket")
            .key("no-such-key")
            .send()
            .await;
        assert!(err.is_err(), "GetObject on missing key should fail");

        // HeadBucket on nonexistent bucket.
        let err = s3.head_bucket().bucket("no-such-bucket").send().await;
        assert!(err.is_err(), "HeadBucket on missing bucket should fail");

        // PutObject on nonexistent bucket.
        let err = s3
            .put_object()
            .bucket("no-such-bucket")
            .key("test.txt")
            .body(aws_sdk_s3::primitives::ByteStream::from_static(b"data"))
            .send()
            .await;
        assert!(err.is_err(), "PutObject on missing bucket should fail");
    });
}
