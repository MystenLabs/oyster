//! Admin withdrawals end to end: a funded Pearl wallet, a registered
//! destination, a request by one admin key, approval by a second, and
//! the SUI/WAL landing at the destination on the in-process Sui cluster.

use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use oyster_e2e_tests::{OysterTestHarness, run_e2e};
use serde_json::Value;
use sui_sdk::SuiClientBuilder;
use sui_types::base_types::SuiAddress;
use tower::ServiceExt;

async fn json_response(app: &Router, req: Request<Body>) -> (axum::http::StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn create_account_via_admin(app: &Router, admin_key: &str) -> (String, String) {
    let (status, body) = json_response(
        app,
        Request::post("/api/v1/accounts")
            .header("authorization", format!("Bearer {admin_key}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name": "withdraw-me"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
    (
        body["account_id"].as_str().unwrap().to_string(),
        body["api_key"]["bearer_token"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

async fn wallet_address(app: &Router, api_key: &str) -> SuiAddress {
    let (status, body) = json_response(
        app,
        Request::get("/api/v1/account/wallet")
            .header("authorization", format!("Bearer {api_key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    body["address"].as_str().unwrap().parse().unwrap()
}

async fn second_admin_key(harness: &OysterTestHarness, app_id: &str) -> String {
    let app_id: oyster::AppId = app_id.parse().unwrap();
    let raw = oyster::auth::generate_api_key();
    let hash = oyster::auth::hash_api_key(&raw);
    let prefix = oyster::auth::key_prefix(&raw);
    oyster::db::app_admin_keys::create_admin_key(&harness.db, &app_id, &hash, &prefix, &raw)
        .await
        .unwrap();
    raw
}

async fn request_and_approve(
    app: &Router,
    requester: &str,
    approver: &str,
    account_id: &str,
    body: &str,
) -> Value {
    let (status, req) = json_response(
        app,
        Request::post(format!("/api/v1/admin/accounts/{account_id}/withdrawals"))
            .header("authorization", format!("Bearer {requester}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{req}");
    let id = req["id"].as_str().unwrap();
    let (status, done) = json_response(
        app,
        Request::post(format!("/api/v1/admin/withdrawals/{id}/approve"))
            .header("authorization", format!("Bearer {approver}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{done}");
    assert_eq!(done["status"], "completed", "{done}");
    assert!(done["tx_digest"].as_str().is_some_and(|d| !d.is_empty()));
    done
}

/// `(SUI in MIST, WAL in FROST)` held by `addr`.
async fn balances(rpc_url: &str, addr: SuiAddress) -> (u64, u64) {
    let client = SuiClientBuilder::default().build(rpc_url).await.unwrap();
    let all = client.coin_read_api().get_all_balances(addr).await.unwrap();
    let mut sui = 0;
    let mut wal = 0;
    for b in all {
        let total = u64::try_from(b.total_balance).unwrap();
        if b.coin_type == "0x2::sui::SUI" {
            sui += total;
        } else {
            wal += total;
        }
    }
    (sui, wal)
}

#[test]
fn e2e_admin_withdrawal_moves_funds_with_dual_control() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;

        let (app_id, key_a) = harness.create_app_admin_key("e2e-withdraw-app").await;
        let key_b = second_admin_key(&harness, &app_id).await;
        let (account_id, api_key) = create_account_via_admin(app, &key_a).await;
        let wallet = wallet_address(app, &api_key).await;
        harness.fund_wallet(&wallet.to_string()).await;
        let (wallet_sui_before, wallet_wal_before) = balances(&harness.rpc_url, wallet).await;
        assert!(wallet_sui_before > 0 && wallet_wal_before > 0);

        // A fresh destination nobody has used.
        let destination = SuiAddress::random_for_testing_only();
        assert_eq!(balances(&harness.rpc_url, destination).await, (0, 0));

        // Register it (cooldown 0 in the e2e config).
        let (status, body) = json_response(
            app,
            Request::put(format!(
                "/api/v1/admin/accounts/{account_id}/withdrawal-address"
            ))
            .header("authorization", format!("Bearer {key_a}"))
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"address":"{destination}"}}"#)))
            .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["usable_now"], true);

        // --- Fixed amounts: 1 SUI + 1 WAL, requested by A, approved by B. ---
        const ONE: u64 = 1_000_000_000;
        let done = request_and_approve(
            app,
            &key_a,
            &key_b,
            &account_id,
            &format!(r#"{{"sui_mist":{ONE},"wal_frost":{ONE}}}"#),
        )
        .await;
        assert_eq!(
            done["approved_by_admin_key_id"].as_str().map(str::is_empty),
            Some(false)
        );
        assert_ne!(
            done["approved_by_admin_key_id"],
            done["requested_by_admin_key_id"]
        );
        assert_eq!(balances(&harness.rpc_url, destination).await, (ONE, ONE));
        let (wallet_sui_mid, wallet_wal_mid) = balances(&harness.rpc_url, wallet).await;
        assert_eq!(wallet_wal_mid, wallet_wal_before - ONE);
        assert!(
            wallet_sui_mid < wallet_sui_before - ONE,
            "SUI amount plus gas left the wallet"
        );

        // The wallet still works: the account can upload.
        let (status, body) = json_response(
            app,
            Request::post("/api/v1/buckets")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"after-withdrawal"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let (status, body) = json_response(
            app,
            Request::put("/api/v1/buckets/after-withdrawal/blobs/x.txt")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "text/plain")
                .body(Body::from("still works"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");

        // --- Drain: requested by B, approved by A (either direction). ---
        let (wallet_sui_pre, wallet_wal_pre) = balances(&harness.rpc_url, wallet).await;
        assert!(wallet_wal_pre > 0, "upload should not have spent all WAL");
        let done = request_and_approve(app, &key_b, &key_a, &account_id, r#"{"all":true}"#).await;
        assert_eq!(done["all"], true);
        assert_eq!(
            balances(&harness.rpc_url, wallet).await,
            (0, 0),
            "wallet emptied"
        );
        let (dest_sui, dest_wal) = balances(&harness.rpc_url, destination).await;
        assert_eq!(dest_wal, ONE + wallet_wal_pre);
        assert!(
            dest_sui > ONE && dest_sui < ONE + wallet_sui_pre,
            "SUI minus the fee arrived"
        );

        // The ledger shows both, newest first, with digests.
        let (status, body) = json_response(
            app,
            Request::get(format!("/api/v1/admin/accounts/{account_id}/withdrawals"))
                .header("authorization", format!("Bearer {key_a}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let list = body["withdrawals"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|w| w["status"] == "completed"));
        assert_eq!(list[0]["all"], true);

        // A drained wallet cannot pay for another withdrawal: the request
        // is recorded as failed and reported as a conflict.
        let (status, req) = json_response(
            app,
            Request::post(format!("/api/v1/admin/accounts/{account_id}/withdrawals"))
                .header("authorization", format!("Bearer {key_a}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"sui_mist":1}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::ACCEPTED, "{req}");
        let id = req["id"].as_str().unwrap();
        let (status, body) = json_response(
            app,
            Request::post(format!("/api/v1/admin/withdrawals/{id}/approve"))
                .header("authorization", format!("Bearer {key_b}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CONFLICT, "{body}");
        let (_, body) = json_response(
            app,
            Request::get(format!("/api/v1/admin/withdrawals/{id}"))
                .header("authorization", format!("Bearer {key_b}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(body["status"], "failed", "{body}");
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|e| e.contains("insufficient"))
        );
    });
}
