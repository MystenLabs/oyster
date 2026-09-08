//! SEC-F3b: Pearl master-seed rotation, exercised end-to-end against an
//! account created under seed version 1 on the in-process Sui + Walrus
//! cluster. Covers the acceptance criterion "exercised on a
//! non-production environment against accounts created before the
//! change": the account, its wallet funding, its `StoragePool` and a
//! stored blob all exist before `keys migrate` runs.

use std::str::FromStr;

use axum::{Router, body::Body, http::Request};
use http_body_util::BodyExt;
use oyster::{
    AccountId,
    key_migration::{self, AccountOutcome, MigrationContext, ObjectClass},
};
use oyster_e2e_tests::{OysterTestHarness, run_e2e};
use serde_json::Value;
use sui_types::base_types::{ObjectID, SuiAddress};
use tower::ServiceExt;
use walrus_sui::client::ReadClient as _;

async fn json_response(app: &Router, req: Request<Body>) -> (axum::http::StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn raw_response(app: &Router, req: Request<Body>) -> (axum::http::StatusCode, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

async fn create_account_via_admin(
    app: &Router,
    admin_key: &str,
    name: &str,
) -> (AccountId, String) {
    let req = Request::post("/api/v1/accounts")
        .header("authorization", format!("Bearer {admin_key}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"name": "{name}"}}"#)))
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
    let account_id = AccountId::from_str(body["account_id"].as_str().unwrap()).unwrap();
    let api_key = body["api_key"]["bearer_token"]
        .as_str()
        .unwrap()
        .to_string();
    (account_id, api_key)
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
    body["address"]
        .as_str()
        .expect("wallet address")
        .parse()
        .expect("valid SuiAddress")
}

async fn create_bucket(app: &Router, api_key: &str, name: &str) -> String {
    let req = Request::post("/api/v1/buckets")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"name":"{name}"}}"#)))
        .unwrap();
    let (status, body) = json_response(app, req).await;
    assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
    body["name"].as_str().unwrap().to_string()
}

async fn put_blob(
    app: &Router,
    api_key: &str,
    bucket: &str,
    key: &str,
    data: &[u8],
) -> (axum::http::StatusCode, Value) {
    let req = Request::put(format!("/api/v1/buckets/{bucket}/blobs/{key}"))
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/octet-stream")
        .body(Body::from(data.to_vec()))
        .unwrap();
    json_response(app, req).await
}

async fn get_blob(app: &Router, bucket: &str, key: &str) -> (axum::http::StatusCode, Vec<u8>) {
    raw_response(
        app,
        Request::get(format!("/api/v1/buckets/{bucket}/blobs/{key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

fn classes(objects: &[key_migration::OwnedObject]) -> Vec<ObjectClass> {
    objects.iter().map(|o| o.class).collect()
}

#[test]
fn e2e_key_rotation_migrates_v1_account_to_v2() {
    run_e2e(async {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        tracing_subscriber::fmt::try_init().ok();

        let harness = OysterTestHarness::start().await;
        let app = &harness.router;
        let db = &harness.db;

        // --- An account that predates the rotation: version 1, funded,
        // with a pool and a blob on-chain. ---
        let (_app_id, admin_key) = harness.create_app_admin_key("e2e-rotation-app").await;
        let (account_id, api_key) = create_account_via_admin(app, &admin_key, "rotate-me").await;
        assert_eq!(
            oyster::db::accounts::get_key_version(db, &account_id)
                .await
                .unwrap(),
            Some(1),
            "new accounts are stamped with Pearl's active version (1)"
        );
        let v1_addr = wallet_address(app, &api_key).await;
        harness.fund_wallet(&v1_addr.to_string()).await;
        let bucket = create_bucket(app, &api_key, "rotation-bucket").await;
        let before = b"stored under seed version 1";
        let (status, body) = put_blob(app, &api_key, &bucket, "before.txt", before).await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let (status, data) = get_blob(app, &bucket, "before.txt").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(data, before);

        let pool_id: ObjectID = oyster::db::accounts::get_storage_pool(db, &account_id)
            .await
            .unwrap()
            .expect("pool created by first upload")
            .object_id
            .parse()
            .unwrap();

        let ctx = MigrationContext::new(
            db.clone(),
            harness.pearl.clone(),
            &harness.rpc_url,
            harness.system_object,
            harness.staking_object,
        )
        .await
        .expect("migration context");
        assert_eq!(ctx.object_owner(pool_id).await.unwrap(), Some(v1_addr));

        let v2_addr: SuiAddress = harness
            .pearl
            .get_address(&account_id, 2)
            .await
            .expect("Pearl derives version 2")
            .parse()
            .unwrap();
        assert_ne!(v1_addr, v2_addr);

        // --- Dry run: reports the plan, changes nothing. ---
        let report = key_migration::migrate(&ctx, 2, None, true, false)
            .await
            .unwrap();
        assert_eq!(report.outcomes.len(), 1);
        match &report.outcomes[0].1 {
            AccountOutcome::Planned {
                from,
                to,
                movable,
                skipped,
            } => {
                assert_eq!((*from, *to), (v1_addr, v2_addr));
                let c = classes(movable);
                assert!(c.contains(&ObjectClass::StoragePool), "{c:?}");
                assert!(c.contains(&ObjectClass::SuiCoin), "{c:?}");
                assert!(c.contains(&ObjectClass::WalCoin), "{c:?}");
                assert!(skipped.is_empty(), "{skipped:?}");
            }
            other => panic!("expected Planned, got {other}"),
        }
        assert_eq!(
            oyster::db::accounts::get_key_version(db, &account_id)
                .await
                .unwrap(),
            Some(1)
        );
        assert_eq!(ctx.object_owner(pool_id).await.unwrap(), Some(v1_addr));

        // --- The migration itself. ---
        let report = key_migration::migrate(&ctx, 2, None, false, false)
            .await
            .unwrap();
        assert_eq!(report.problems(), 0, "{:?}", report.outcomes);
        match &report.outcomes[0].1 {
            AccountOutcome::Moved(t) => {
                assert!(!t.digests.is_empty());
                assert!(t.skipped.is_empty(), "{:?}", t.skipped);
                assert!(t.moved >= 3, "pool + SUI + WAL at least, got {}", t.moved);
            }
            other => panic!("expected Moved, got {other}"),
        }

        // DB: re-stamped, lock released.
        let cand = oyster::db::accounts::get_key_migration_candidate(db, &account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cand.key_version, 2);
        assert!(cand.key_migrating_since.is_none());

        // Chain: everything at v2, nothing left at v1.
        assert_eq!(ctx.object_owner(pool_id).await.unwrap(), Some(v2_addr));
        assert!(ctx.list_owned(v1_addr).await.unwrap().is_empty());
        let at_v2 = classes(&ctx.list_owned(v2_addr).await.unwrap());
        assert!(at_v2.contains(&ObjectClass::StoragePool), "{at_v2:?}");
        assert!(at_v2.contains(&ObjectClass::SuiCoin), "{at_v2:?}");
        assert!(at_v2.contains(&ObjectClass::WalCoin), "{at_v2:?}");

        // The API now reports the v2 funding address.
        assert_eq!(wallet_address(app, &api_key).await, v2_addr);

        // --- Service keeps working, now signing under version 2. ---
        let (status, data) = get_blob(app, &bucket, "before.txt").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(data, before);
        let after = b"stored under seed version 2";
        let (status, body) = put_blob(app, &api_key, &bucket, "after.txt", after).await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let (status, data) = get_blob(app, &bucket, "after.txt").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(data, after);

        // The extension worker extends the migrated pool with the v2 key.
        let status_before = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .unwrap();
        let current_epoch = harness
            .walrus_sui_client()
            .read_client()
            .current_epoch()
            .await
            .unwrap();
        let lookahead = status_before.end_epoch + 1 - current_epoch;
        let processed = harness.trigger_extension_cycle(lookahead, 1).await;
        assert_eq!(processed, 1);
        let status_after = harness
            .walrus_sui_client()
            .storage_pool_status(pool_id)
            .await
            .unwrap();
        assert_eq!(status_after.end_epoch, status_before.end_epoch + 1);
        assert_eq!(ctx.object_owner(pool_id).await.unwrap(), Some(v2_addr));

        // --- Idempotent: a fleet-wide re-run finds no account below 2,
        // and an explicit re-run of this account is a no-op. ---
        let report = key_migration::migrate(&ctx, 2, None, false, false)
            .await
            .unwrap();
        assert!(report.outcomes.is_empty(), "{:?}", report.outcomes);
        let report = key_migration::migrate(&ctx, 2, Some(&account_id), false, false)
            .await
            .unwrap();
        assert!(matches!(
            report.outcomes[0].1,
            AccountOutcome::AlreadyAtVersion
        ));

        // --- Sweep: an integrator still funding the old address. ---
        harness.fund_address(&v1_addr.to_string()).await;
        assert!(!ctx.list_owned(v1_addr).await.unwrap().is_empty());
        let report = key_migration::sweep(&ctx, 1, None, false).await.unwrap();
        assert_eq!(report.problems(), 0, "{:?}", report.outcomes);
        assert!(matches!(report.outcomes[0].1, AccountOutcome::Moved(_)));
        assert!(ctx.list_owned(v1_addr).await.unwrap().is_empty());
        let report = key_migration::sweep(&ctx, 1, None, false).await.unwrap();
        assert!(matches!(
            report.outcomes[0].1,
            AccountOutcome::NothingOnChain
        ));
        assert_eq!(
            oyster::db::accounts::get_key_version(db, &account_id)
                .await
                .unwrap(),
            Some(2),
            "sweep never touches key_version"
        );

        // --- An account with nothing on-chain just flips. ---
        let (empty_id, _) = create_account_via_admin(app, &admin_key, "never-funded").await;
        let report = key_migration::migrate(&ctx, 2, Some(&empty_id), false, false)
            .await
            .unwrap();
        assert!(matches!(
            report.outcomes[0].1,
            AccountOutcome::NothingOnChain
        ));
        assert_eq!(
            oyster::db::accounts::get_key_version(db, &empty_id)
                .await
                .unwrap(),
            Some(2)
        );

        // --- Unconfigured target version: refused before any lock. ---
        let report = key_migration::migrate(&ctx, 3, Some(&account_id), false, false)
            .await
            .unwrap();
        match &report.outcomes[0].1 {
            AccountOutcome::Failed(key_migration::MigrationError::Pearl(msg)) => {
                assert!(msg.contains("version 3"), "{msg}");
            }
            other => panic!("expected Pearl failure, got {other}"),
        }
        let cand = oyster::db::accounts::get_key_migration_candidate(db, &account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cand.key_version, 2);
        assert!(cand.key_migrating_since.is_none());

        // --- The rotation lock: writes answer 503, migrate reports it,
        // --break-lock clears it. ---
        // Simulate a run that crashed mid-migration: the lock is set, the
        // version is not.
        sqlx::query(&oyster::db::sql(
            "UPDATE accounts SET key_migrating_since = '2026-01-01 00:00:00' WHERE id = ?",
        ))
        .bind(&account_id)
        .execute(db)
        .await
        .unwrap();
        let (status, body) = put_blob(app, &api_key, &bucket, "locked.txt", b"x").await;
        assert_eq!(
            status,
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "{body}"
        );
        let report = key_migration::migrate(&ctx, 3, Some(&account_id), false, false)
            .await
            .unwrap();
        assert!(matches!(
            report.outcomes[0].1,
            AccountOutcome::Locked { .. }
        ));
        // break_lock clears the lock, then the (unconfigured) version 3
        // still fails, leaving no lock behind.
        let report = key_migration::migrate(&ctx, 3, Some(&account_id), false, true)
            .await
            .unwrap();
        assert!(matches!(
            report.outcomes[0].1,
            AccountOutcome::Failed(key_migration::MigrationError::Pearl(_))
        ));
        let cand = oyster::db::accounts::get_key_migration_candidate(db, &account_id)
            .await
            .unwrap()
            .unwrap();
        assert!(cand.key_migrating_since.is_none());
        let (status, body) = put_blob(app, &api_key, &bucket, "unlocked.txt", b"y").await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
    });
}
