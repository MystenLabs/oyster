#![allow(missing_docs)]

use std::{io::Write, path::Path, process::Output, time::Duration};

use assert_cmd::Command;
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

/// Helper: create a test account via the admin endpoint.
async fn create_test_account_via_admin(app: &Router, admin_key: &str) -> (String, String) {
    let req = Request::post("/api/v1/accounts")
        .header("authorization", format!("Bearer {admin_key}"))
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

/// Isolate the CLI's config-discovery walk from the host environment.
/// Without this, the CLI walks `~/.config/oyster/client.yaml` and fails the
/// chmod check if the test runner's user has a real config with mode != 0o600.
fn isolate_env(cmd: &mut Command, iso_home: &Path) {
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env("HOME", iso_home);
    cmd.current_dir(iso_home);
}

/// Build a CLI command with auth flags and a timeout.
#[allow(deprecated)]
fn cli_cmd(iso_home: &Path, url: &str, api_key: &str) -> Command {
    let mut cmd = Command::cargo_bin("oyster").unwrap();
    cmd.args(["--url", url, "--api-key", api_key, "--json"]);
    cmd.timeout(Duration::from_secs(120));
    isolate_env(&mut cmd, iso_home);
    cmd
}

/// Build a CLI command without auth (for public endpoints like `read`).
#[allow(deprecated)]
fn cli_cmd_public(iso_home: &Path, url: &str) -> Command {
    let mut cmd = Command::cargo_bin("oyster").unwrap();
    cmd.args(["--url", url]);
    cmd.timeout(Duration::from_secs(120));
    isolate_env(&mut cmd, iso_home);
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

        // Create account and fund wallet via admin key.
        eprintln!("[cli_e2e] creating test account...");
        let (_app_id, admin_key) = harness.create_app_admin_key("cli-e2e-app").await;
        let (_account_id, api_key) = create_test_account_via_admin(app, &admin_key).await;
        eprintln!("[cli_e2e] funding test wallet...");
        fund_test_wallet(&harness, app, &api_key).await;

        // Serve on a random port for CLI subprocess calls.
        eprintln!("[cli_e2e] starting HTTP server...");
        let base_url = harness.serve_on_random_port().await;
        let url = format!("{base_url}/api/v1");
        eprintln!("[cli_e2e] server ready at {url}");

        // Isolate the CLI's config-discovery walk from the host's real
        // ~/.config/oyster/client.yaml — otherwise the chmod check rejects
        // any user config with mode != 0o600.
        let iso_dir = tempfile::tempdir().expect("create iso tempdir");
        let iso_home = iso_dir.path().to_owned();

        // 1. create-bucket
        eprintln!("[cli_e2e] 1/9 create-bucket");
        let output = run_cli({
            let mut cmd = cli_cmd(&iso_home, &url, &api_key);
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
        eprintln!("[cli_e2e] 2/9 list-buckets");
        let output = run_cli({
            let mut cmd = cli_cmd(&iso_home, &url, &api_key);
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
        eprintln!("[cli_e2e] 3/9 store");
        let test_data = b"Hello from CLI E2E test!";
        let mut tmp_input = tempfile::NamedTempFile::new().expect("create temp input file");
        tmp_input.write_all(test_data).expect("write temp data");
        tmp_input.flush().expect("flush temp data");

        let input_path = tmp_input.path().to_str().unwrap().to_string();
        let output = run_cli({
            let mut cmd = cli_cmd(&iso_home, &url, &api_key);
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
        eprintln!("[cli_e2e] 4/9 list-blobs");
        let output = run_cli({
            let mut cmd = cli_cmd(&iso_home, &url, &api_key);
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
        eprintln!("[cli_e2e] 5/9 read");
        let tmp_output = tempfile::NamedTempFile::new().expect("create temp output file");
        let output_path = tmp_output.path().to_str().unwrap().to_string();
        let output = run_cli({
            let mut cmd = cli_cmd_public(&iso_home, &url);
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
        eprintln!("[cli_e2e] 6/9 delete");
        let output = run_cli({
            let mut cmd = cli_cmd(&iso_home, &url, &api_key);
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
        eprintln!("[cli_e2e] 7/9 delete-bucket");
        let output = run_cli({
            let mut cmd = cli_cmd(&iso_home, &url, &api_key);
            cmd.args(["delete-bucket", "cli-test-bucket"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "delete-bucket failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // 8. wallet
        eprintln!("[cli_e2e] 8/9 wallet");
        let output = run_cli({
            let mut cmd = cli_cmd(&iso_home, &url, &api_key);
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
        assert!(wallet["address"].as_str().is_some());

        // 9. info
        eprintln!("[cli_e2e] 9/9 info");
        let output = run_cli({
            let mut cmd = cli_cmd(&iso_home, &url, &api_key);
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

        eprintln!("[cli_e2e] all 9 steps passed!");
    });
}

/// E2E for the `oyster app account` subcommands. Drives `list / create /
/// keys / use` against the in-process server with `--json` (always
/// non-interactive) and a per-test config file.
///
/// TODO: the inquire-based interactive revoke flow is **not** covered
/// here — verify manually with `cargo run -p oyster-cli -- app account
/// use <id>` in a real terminal at the cap.
#[test]
fn cli_e2e_account_management() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        eprintln!("[cli_e2e_account] booting harness...");
        let harness = OysterTestHarness::start().await;

        eprintln!("[cli_e2e_account] minting admin key...");
        let (_app_id, admin_key) = harness.create_app_admin_key("cli-e2e-account-app").await;
        let base_url = harness.serve_on_random_port().await;
        let url = format!("{base_url}/api/v1");
        eprintln!("[cli_e2e_account] server ready at {url}");

        // Build a per-test config file that the CLI will read via --config.
        let dir = tempfile::tempdir().expect("create tempdir");
        let cfg_path = dir.path().join("client.yaml");
        let yaml = format!(
            "active_context: test\n\
             contexts:\n  \
               test:\n    \
                 url: {url}\n    \
                 apps:\n      \
                   cli-e2e-account-app:\n        \
                     admin_key: {admin_key}\n"
        );
        std::fs::write(&cfg_path, &yaml).expect("write config");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cfg_path, std::fs::Permissions::from_mode(0o600))
                .expect("chmod 0600");
        }

        let cfg_path_str = cfg_path.to_str().unwrap().to_string();
        let cli_admin_cmd = || -> Command {
            #[allow(deprecated)]
            let mut cmd = Command::cargo_bin("oyster").unwrap();
            cmd.args(["--config", &cfg_path_str, "--json"]);
            cmd.timeout(Duration::from_secs(60));
            cmd
        };

        // 1. list — empty.
        eprintln!("[cli_e2e_account] 1/7 account list (empty)");
        let output = run_cli({
            let mut cmd = cli_admin_cmd();
            cmd.args(["app", "account", "list"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "list (empty) failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: Value = serde_json::from_slice(&output.stdout).expect("parse list JSON");
        assert_eq!(v.as_array().unwrap().len(), 0);

        // 2. create --activate — saves bearer to context.api_key.
        eprintln!("[cli_e2e_account] 2/7 account create --activate");
        let output = run_cli({
            let mut cmd = cli_admin_cmd();
            cmd.args(["app", "account", "create", "--name", "foo", "--activate"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "create failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let resp: Value = serde_json::from_slice(&output.stdout).expect("parse create JSON");
        let account_id = resp["account_id"].as_str().expect("account_id").to_string();
        let bearer = resp["api_key"]["bearer_token"]
            .as_str()
            .expect("bearer_token")
            .to_string();

        // The config file should now contain the bearer under contexts.test.
        let saved = std::fs::read_to_string(&cfg_path).expect("read config");
        assert!(
            saved.contains(&bearer),
            "config should have the activated bearer saved"
        );
        assert!(
            saved.contains("api_key:"),
            "config should have api_key field"
        );

        // 3. list — exactly one row, name foo, active_api_key_count == 1.
        eprintln!("[cli_e2e_account] 3/7 account list (one)");
        let output = run_cli({
            let mut cmd = cli_admin_cmd();
            cmd.args(["app", "account", "list"]);
            cmd
        })
        .await;
        assert!(output.status.success());
        let v: Value = serde_json::from_slice(&output.stdout).expect("parse list JSON");
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "foo");
        assert_eq!(rows[0]["active_api_key_count"], 1);

        // 4. keys foo — one row, default note "api".
        eprintln!("[cli_e2e_account] 4/7 account keys foo");
        let output = run_cli({
            let mut cmd = cli_admin_cmd();
            cmd.args(["app", "account", "keys", "foo"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "keys failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: Value = serde_json::from_slice(&output.stdout).expect("parse keys JSON");
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0]["note"].as_str().unwrap().contains("api"),
            "default note should contain 'api'"
        );

        // 5. use foo — first mint should succeed (now 2 active keys).
        eprintln!("[cli_e2e_account] 5/7 account use foo (mint #2)");
        let output = run_cli({
            let mut cmd = cli_admin_cmd();
            cmd.args(["app", "account", "use", "foo"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "use #2 failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // mint #3 should still succeed (cap is 3 active).
        let output = run_cli({
            let mut cmd = cli_admin_cmd();
            cmd.args(["app", "account", "use", "foo"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "use #3 failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // 6. use foo (no --revoke*) at cap — should error mentioning --revoke-oldest.
        eprintln!("[cli_e2e_account] 6/7 account use foo at cap (no --revoke*)");
        let output = run_cli({
            let mut cmd = cli_admin_cmd();
            cmd.args(["app", "account", "use", "foo"]);
            cmd
        })
        .await;
        assert!(
            !output.status.success(),
            "use at cap should fail without --revoke*"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--revoke-oldest"),
            "stderr should mention --revoke-oldest, got: {stderr}"
        );

        // 7. use foo --revoke-oldest — succeeds; total active still 3 (revoke + mint).
        eprintln!("[cli_e2e_account] 7/7 account use foo --revoke-oldest");
        let output = run_cli({
            let mut cmd = cli_admin_cmd();
            cmd.args(["app", "account", "use", "foo", "--revoke-oldest"]);
            cmd
        })
        .await;
        assert!(
            output.status.success(),
            "use --revoke-oldest failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Sanity: account_id still resolves and active_api_key_count == 3.
        let output = run_cli({
            let mut cmd = cli_admin_cmd();
            cmd.args(["app", "account", "list"]);
            cmd
        })
        .await;
        assert!(output.status.success());
        let v: Value = serde_json::from_slice(&output.stdout).expect("parse list JSON");
        let rows = v.as_array().unwrap();
        let row = rows
            .iter()
            .find(|r| r["id"] == account_id)
            .expect("account row");
        assert_eq!(row["active_api_key_count"], 3);

        eprintln!("[cli_e2e_account] all 7 steps passed!");
    });
}
