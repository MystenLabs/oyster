#![allow(missing_docs)]

use std::{io::Write, process::Output, time::Duration};

use assert_cmd::Command;
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
    let req = Request::post("/api/v1/debug/create-account")
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
        Request::get("/api/v1/account/wallet")
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let address = body["wallet"]["address"].as_str().expect("wallet address");
    harness.fund_wallet(address).await;
}

/// Build a CLI command with auth flags and a timeout.
#[allow(deprecated)]
fn cli_cmd(url: &str, api_key: &str) -> Command {
    let mut cmd = Command::cargo_bin("oyster").unwrap();
    cmd.args(["--url", url, "--api-key", api_key, "--json"]);
    cmd.timeout(Duration::from_secs(120));
    cmd
}

/// Build a CLI command without auth (for public endpoints like `read`).
#[allow(deprecated)]
fn cli_cmd_public(url: &str) -> Command {
    let mut cmd = Command::cargo_bin("oyster").unwrap();
    cmd.args(["--url", url]);
    cmd.timeout(Duration::from_secs(120));
    cmd
}

/// Run a CLI command on a blocking thread to avoid blocking the tokio runtime.
async fn run_cli(mut cmd: Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.output().expect("failed to execute CLI"))
        .await
        .expect("spawn_blocking join")
}

/// Full CLI E2E lifecycle exercising all major commands as subprocesses.
#[test]
fn cli_e2e_full_lifecycle() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        eprintln!("[cli_e2e] booting harness...");
        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        // Create account and fund wallet via in-process helpers.
        eprintln!("[cli_e2e] creating test account...");
        let (_account_id, api_key) = create_test_account(app).await;
        eprintln!("[cli_e2e] funding test wallet...");
        fund_test_wallet(&harness, app, &api_key).await;

        // Serve on a random port for CLI subprocess calls.
        eprintln!("[cli_e2e] starting HTTP server...");
        let url = harness.serve_on_random_port().await;
        eprintln!("[cli_e2e] server ready at {url}");

        // 1. create-bucket
        eprintln!("[cli_e2e] 1/10 create-bucket");
        let output = run_cli({
            let mut cmd = cli_cmd(&url, &api_key);
            cmd.args(["create-bucket", "cli-test-bucket"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "create-bucket failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let bucket: Value =
            serde_json::from_slice(&output.stdout).expect("parse create-bucket JSON");
        assert!(bucket["name"].as_str().is_some());

        // 2. list-buckets
        eprintln!("[cli_e2e] 2/10 list-buckets");
        let output = run_cli({
            let mut cmd = cli_cmd(&url, &api_key);
            cmd.arg("list-buckets");
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "list-buckets failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let list: Value = serde_json::from_slice(&output.stdout).expect("parse list-buckets JSON");
        let buckets = list["data"].as_array().expect("data array");
        assert!(buckets.iter().any(|b| b["name"] == "cli-test-bucket"));

        // 3. store — write test data to a temp file
        eprintln!("[cli_e2e] 3/10 store");
        let test_data = b"Hello from CLI E2E test!";
        let mut tmp_input = tempfile::NamedTempFile::new().expect("create temp input file");
        tmp_input.write_all(test_data).expect("write temp data");
        tmp_input.flush().expect("flush temp data");

        let input_path = tmp_input.path().to_str().unwrap().to_string();
        let output = run_cli({
            let mut cmd = cli_cmd(&url, &api_key);
            cmd.args([
                "store",
                &input_path,
                "--bucket",
                "cli-test-bucket",
                "--key",
                "test-file.txt",
            ]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "store failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stored: Value = serde_json::from_slice(&output.stdout).expect("parse store JSON");
        let blob_key = stored["key"].as_str().expect("key").to_string();
        assert!(stored["blob_id"].as_str().is_some());
        assert!(stored["size"].as_u64().is_some());
        assert!(stored["md5"].as_str().is_some());

        // 4. list-blobs
        eprintln!("[cli_e2e] 4/10 list-blobs");
        let output = run_cli({
            let mut cmd = cli_cmd(&url, &api_key);
            cmd.args(["list-blobs", "--bucket", "cli-test-bucket"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "list-blobs failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let list: Value = serde_json::from_slice(&output.stdout).expect("parse list-blobs JSON");
        let blobs = list["data"].as_array().expect("data array");
        assert!(blobs.iter().any(|b| b["key"].as_str() == Some(&blob_key)));

        // 5. read — download to a temp file and verify contents
        eprintln!("[cli_e2e] 5/10 read");
        let tmp_output = tempfile::NamedTempFile::new().expect("create temp output file");
        let output_path = tmp_output.path().to_str().unwrap().to_string();
        let output = run_cli({
            let mut cmd = cli_cmd_public(&url);
            cmd.args([
                "read",
                &blob_key,
                "--bucket",
                "cli-test-bucket",
                "-o",
                &output_path,
            ]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "read failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let read_data = std::fs::read(tmp_output.path()).expect("read output file");
        assert_eq!(read_data, test_data, "read data should match stored data");

        // 6. delete
        eprintln!("[cli_e2e] 6/10 delete");
        let output = run_cli({
            let mut cmd = cli_cmd(&url, &api_key);
            cmd.args(["delete", &blob_key, "--bucket", "cli-test-bucket"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "delete failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // 7. delete-bucket
        eprintln!("[cli_e2e] 7/10 delete-bucket");
        let output = run_cli({
            let mut cmd = cli_cmd(&url, &api_key);
            cmd.args(["delete-bucket", "cli-test-bucket"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "delete-bucket failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // 8. create-api-key
        eprintln!("[cli_e2e] 8/10 create-api-key");
        let output = run_cli({
            let mut cmd = cli_cmd(&url, &api_key);
            cmd.arg("create-api-key");
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "create-api-key failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let key: Value = serde_json::from_slice(&output.stdout).expect("parse create-api-key JSON");
        assert!(key["id"].as_str().is_some());
        assert!(key["prefix"].as_str().is_some());
        assert!(key["secret"].as_str().is_some());

        // 9. wallet
        eprintln!("[cli_e2e] 9/10 wallet");
        let output = run_cli({
            let mut cmd = cli_cmd(&url, &api_key);
            cmd.arg("wallet");
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "wallet failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let wallet: Value = serde_json::from_slice(&output.stdout).expect("parse wallet JSON");
        assert_eq!(wallet["provisioned"].as_bool(), Some(true));
        assert!(wallet["wallet"]["address"].as_str().is_some());

        // 10. info
        eprintln!("[cli_e2e] 10/10 info");
        let output = run_cli({
            let mut cmd = cli_cmd(&url, &api_key);
            cmd.arg("info");
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "info failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let info: Value = serde_json::from_slice(&output.stdout).expect("parse info JSON");
        assert!(info["url"].as_str().is_some());

        eprintln!("[cli_e2e] all 10 steps passed!");
    });
}
