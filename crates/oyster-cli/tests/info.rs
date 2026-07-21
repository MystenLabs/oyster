//! Tests for `oyster info` config-file diagnostics. `info` never
//! contacts a server, so these drive the real binary without a harness.

#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

const CONFIG_YAML: &str =
    "active_context: dev\ncontexts:\n  dev:\n    url: http://localhost:3000\n";

fn write_config(dir: &Path, mode: u32) -> std::path::PathBuf {
    let path = dir.join("client.yaml");
    fs::write(&path, CONFIG_YAML).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

fn run_info(config: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_oyster"))
        .args(["--config", config.to_str().unwrap(), "--json", "info"])
        .output()
        .expect("failed to run oyster binary")
}

/// An unreadable config (0644 permissions) must be surfaced as a
/// warning, not silently reported as "config: (none)".
#[test]
fn info_warns_on_insecure_config_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), 0o644);

    let output = run_info(&config);

    assert!(output.status.success(), "info must stay best-effort");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("insecure permissions"),
        "stderr must explain why the config was not loaded, got: {stderr}"
    );
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(info["config_file"].is_null());
}

/// With correct permissions the config loads and no warning is printed.
#[test]
fn info_reads_config_with_0o600() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), 0o600);

    let output = run_info(&config);

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("warning"),
        "no warning expected, got: {stderr}"
    );
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        info["config_file"].as_str(),
        config.to_str(),
        "config_file must point at the loaded file"
    );
    assert_eq!(info["url"].as_str(), Some("http://localhost:3000"));
}
