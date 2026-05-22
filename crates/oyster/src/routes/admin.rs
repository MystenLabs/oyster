//! Admin API routes authenticated via per-app admin keys (AuthenticatedApp).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use walrus_core::{EncodingType, encoding::encoded_blob_length_for_n_shards};
use walrus_sui::client::ReadClient as _;

use crate::{
    AccountId,
    AppId,
    AppState,
    admin_storage_pool::{self, DecreaseError},
    app_admin::AuthenticatedApp,
    auth,
    blob_store::BlobStoreError,
    db,
    error::AppError,
    models::{
        AccessKey,
        AccessKeyWithSecret,
        AccountSummary,
        ApiKeyMetadata,
        ApiKeyWithBearerToken,
        App,
        AppWithPublicKey,
        CreateAccountRequest,
        CreateAccountResponse,
        CreateApiKeyRequest,
        ErrorResponse,
        PoolOnChainState,
        SetWebhookUrlRequest,
        UpdateMaxStorageRequest,
        UpdateMaxStorageResponse,
    },
    sui_object_reader,
    validation,
    webhook_keys,
};

/// Maximum number of active access keys per account.
const MAX_ACCESS_KEYS: i64 = 3;

/// Maximum number of active API keys per account.
const MAX_API_KEYS_PER_ACCOUNT: i64 = 3;

/// Fetch an account and verify it belongs to the authenticated app.
async fn verify_account_ownership(
    db: &db::DbPool,
    account_id: &AccountId,
    app_id: &AppId,
) -> Result<(), AppError> {
    let account = db::accounts::get_account(db, account_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if account.app_id != *app_id {
        return Err(AppError::Forbidden(
            "account does not belong to this app".into(),
        ));
    }
    Ok(())
}

#[utoipa::path(
    post,
    path = "/accounts",
    tag = "Admin",
    security(("bearer" = [])),
    request_body(content = CreateAccountRequest, content_type = "application/json"),
    responses(
        (status = 201, description = "Account created", body = CreateAccountResponse),
        (status = 400, description = "Invalid request (e.g. non-positive max_unencoded_bytes)", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// Create a new account owned by the authenticated app.
pub async fn create_account(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    body: Option<Json<CreateAccountRequest>>,
) -> Result<(StatusCode, Json<CreateAccountResponse>), AppError> {
    let body = body.map(|b| b.0);
    let name = body.as_ref().and_then(|b| b.name.clone());
    let max_unencoded_bytes = body.as_ref().and_then(|b| b.max_unencoded_bytes);
    if let Some(cap) = max_unencoded_bytes
        && cap <= 0
    {
        return Err(AppError::BadRequest(
            "max_unencoded_bytes must be a positive integer".into(),
        ));
    }
    let note = body.and_then(|b| b.note).unwrap_or_else(|| "api".into());
    let account = db::accounts::create_account(
        &state.db,
        &auth.app_id,
        name.as_deref(),
        max_unencoded_bytes,
    )
    .await?;

    let raw_key = auth::generate_api_key();
    let key_hash = auth::hash_api_key(&raw_key);
    let prefix = auth::key_prefix(&raw_key);

    let api_key =
        db::api_keys::create_api_key(&state.db, &account.id, &key_hash, &prefix, &raw_key, &note)
            .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateAccountResponse {
            account_id: account.id,
            api_key,
        }),
    ))
}

#[utoipa::path(
    post,
    path = "/accounts/{account_id}/api-keys",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    request_body(content = CreateApiKeyRequest, content_type = "application/json"),
    responses(
        (status = 201, description = "API key created", body = ApiKeyWithBearerToken),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Account not found", body = ErrorResponse),
        (status = 409, description = "API key limit reached", body = ErrorResponse),
    ),
)]
/// Create a new API key for an account owned by the authenticated app.
pub async fn admin_create_api_key(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
    body: Option<Json<CreateApiKeyRequest>>,
) -> Result<(StatusCode, Json<ApiKeyWithBearerToken>), AppError> {
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;

    let count = db::api_keys::count_active_api_keys(&state.db, &account_id).await?;
    if count >= MAX_API_KEYS_PER_ACCOUNT {
        return Err(AppError::Conflict(format!(
            "api key limit reached ({MAX_API_KEYS_PER_ACCOUNT})"
        )));
    }

    let note = body.and_then(|b| b.0.note).unwrap_or_else(|| "api".into());
    let raw_key = auth::generate_api_key();
    let key_hash = auth::hash_api_key(&raw_key);
    let prefix = auth::key_prefix(&raw_key);

    let api_key =
        db::api_keys::create_api_key(&state.db, &account_id, &key_hash, &prefix, &raw_key, &note)
            .await?;

    Ok((StatusCode::CREATED, Json(api_key)))
}

#[utoipa::path(
    get,
    path = "/accounts",
    tag = "Admin",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Accounts owned by the authenticated app", body = Vec<AccountSummary>),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// List accounts owned by the authenticated app, with active API key counts.
pub async fn list_accounts(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
) -> Result<Json<Vec<AccountSummary>>, AppError> {
    let summaries = db::accounts::list_account_summaries_by_app(&state.db, &auth.app_id).await?;
    Ok(Json(summaries))
}

#[utoipa::path(
    get,
    path = "/accounts/{account_id}/api-keys",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    responses(
        (status = 200, description = "API keys for the account", body = Vec<ApiKeyMetadata>),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Account not found", body = ErrorResponse),
    ),
)]
/// List API key metadata for an account owned by the authenticated app.
/// Never returns the bearer secret.
pub async fn list_api_keys_for_account(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
) -> Result<Json<Vec<ApiKeyMetadata>>, AppError> {
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;
    let keys = db::api_keys::list_api_keys_by_account(&state.db, &account_id).await?;
    Ok(Json(keys))
}

#[utoipa::path(
    delete,
    path = "/accounts/{account_id}/api-keys/{key_id}",
    tag = "Admin",
    security(("bearer" = [])),
    params(
        ("account_id" = AccountId, Path, description = "Account ID"),
        ("key_id" = String, Path, description = "API key ID to revoke"),
    ),
    responses(
        (status = 204, description = "API key revoked"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "API key not found", body = ErrorResponse),
    ),
)]
/// Revoke an API key for an account owned by the authenticated app.
pub async fn admin_revoke_api_key(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path((account_id, key_id)): Path<(AccountId, String)>,
) -> Result<StatusCode, AppError> {
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;

    let revoked = db::api_keys::revoke_api_key(&state.db, &key_id, &account_id).await?;
    if revoked {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

#[utoipa::path(
    post,
    path = "/accounts/{account_id}/access-keys",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    responses(
        (status = 201, description = "Access key created", body = AccessKeyWithSecret),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Account not found", body = ErrorResponse),
        (status = 409, description = "Access key limit reached", body = ErrorResponse),
    ),
)]
/// Create a new S3 access key for an account owned by the authenticated app.
pub async fn admin_create_access_key(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
) -> Result<(StatusCode, Json<AccessKeyWithSecret>), AppError> {
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;

    let count = db::access_keys::count_access_keys(&state.db, &account_id).await?;
    if count >= MAX_ACCESS_KEYS {
        return Err(AppError::Conflict(format!(
            "access key limit reached ({MAX_ACCESS_KEYS})"
        )));
    }
    let key = db::access_keys::create_access_key(&state.db, &account_id).await?;
    Ok((StatusCode::CREATED, Json(key)))
}

#[utoipa::path(
    get,
    path = "/accounts/{account_id}/access-keys",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    responses(
        (status = 200, description = "List of access keys", body = Vec<AccessKey>),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Account not found", body = ErrorResponse),
    ),
)]
/// List S3 access keys for an account owned by the authenticated app.
pub async fn admin_list_access_keys(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
) -> Result<Json<Vec<AccessKey>>, AppError> {
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;

    let keys = db::access_keys::list_access_keys(&state.db, &account_id).await?;
    Ok(Json(keys))
}

#[utoipa::path(
    delete,
    path = "/accounts/{account_id}/access-keys/{access_key_id}",
    tag = "Admin",
    security(("bearer" = [])),
    params(
        ("account_id" = AccountId, Path, description = "Account ID"),
        ("access_key_id" = String, Path, description = "Access key ID to revoke"),
    ),
    responses(
        (status = 204, description = "Access key revoked"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Access key not found", body = ErrorResponse),
    ),
)]
/// Revoke an S3 access key for an account owned by the authenticated app.
pub async fn admin_delete_access_key(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path((account_id, access_key_id)): Path<(AccountId, String)>,
) -> Result<StatusCode, AppError> {
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;

    let revoked =
        db::access_keys::delete_access_key(&state.db, &access_key_id, &account_id).await?;
    if revoked {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

#[utoipa::path(
    get,
    path = "/admin/app",
    tag = "Admin",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The authenticated app, including the webhook public key", body = AppWithPublicKey),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// Return the authenticated app, including its current webhook URL and
/// public key. Useful when an admin lost the response from `set_webhook_url`.
pub async fn get_app(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
) -> Result<Json<AppWithPublicKey>, AppError> {
    let app = db::apps::get_app(&state.db, &auth.app_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(AppWithPublicKey::from(app)))
}

#[utoipa::path(
    put,
    path = "/admin/app/webhook",
    tag = "Admin",
    security(("bearer" = [])),
    request_body(content = SetWebhookUrlRequest, content_type = "application/json"),
    responses(
        (status = 200, description = "Webhook URL set; response includes the freshly-generated public key", body = AppWithPublicKey),
        (status = 400, description = "Invalid webhook URL", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// Register or rotate the webhook URL for the authenticated app. Each call
/// generates a fresh Ed25519 keypair; the returned public key is the only
/// way to verify subsequent webhook deliveries.
pub async fn set_webhook_url(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Json(body): Json<SetWebhookUrlRequest>,
) -> Result<Json<AppWithPublicKey>, AppError> {
    validation::validate_webhook_url(&body.webhook_url, state.config.allow_http_webhook_scheme)
        .map_err(AppError::BadRequest)?;
    let parsed = url::Url::parse(&body.webhook_url).expect("validated above");

    let (signing_key, public_key_bytes) = webhook_keys::generate_keypair();
    let private_b64 = webhook_keys::encode(signing_key.as_bytes());
    let public_b64 = webhook_keys::encode(&public_key_bytes);

    let app = db::apps::set_app_webhook(
        &state.db,
        &auth.app_id,
        &body.webhook_url,
        &public_b64,
        &private_b64,
    )
    .await?;

    let fingerprint = webhook_keys::fingerprint(&public_key_bytes);
    db::audit_events::record_audit_event(
        &state.db,
        &auth.app_id,
        Some(&auth.admin_key_id),
        "webhook.url_set",
        serde_json::json!({
            "host": parsed.host_str(),
            "public_key_fingerprint": fingerprint,
        }),
    )
    .await?;
    tracing::info!(
        app_id = %auth.app_id,
        host = ?parsed.host_str(),
        %fingerprint,
        "webhook url set",
    );

    Ok(Json(AppWithPublicKey::from(app)))
}

#[utoipa::path(
    delete,
    path = "/admin/app/webhook",
    tag = "Admin",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Webhook URL cleared", body = App),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// Clear the webhook URL and discard the keypair for the authenticated app.
/// Subsequent extension failures will not deliver a webhook.
pub async fn clear_webhook_url(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
) -> Result<Json<App>, AppError> {
    let app = db::apps::clear_app_webhook(&state.db, &auth.app_id).await?;
    db::audit_events::record_audit_event(
        &state.db,
        &auth.app_id,
        Some(&auth.admin_key_id),
        "webhook.url_cleared",
        serde_json::json!({}),
    )
    .await?;
    tracing::info!(app_id = %auth.app_id, "webhook url cleared");
    Ok(Json(app))
}

#[utoipa::path(
    put,
    path = "/accounts/{account_id}/max-storage",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    request_body(content = UpdateMaxStorageRequest, content_type = "application/json"),
    responses(
        (
            status = 200,
            description = "Cap updated; response carries the post-update on-chain pool snapshot \
                           and the shrink tx digest when a shrink was submitted",
            body = UpdateMaxStorageResponse,
        ),
        (status = 400, description = "Invalid request, would orphan data, or shrink aborted", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Account not found", body = ErrorResponse),
        (status = 503, description = "On-chain shrink unavailable in this configuration", body = ErrorResponse),
    ),
)]
/// Update an account's per-account `max_unencoded_bytes` cap.
///
/// Logic:
/// 1. If the account has no on-chain `StoragePool` yet, just update the DB
///    cap — no on-chain action is needed because future uploads enforce the
///    new cap before any lazy-create.
/// 2. Otherwise read the on-chain pool's
///    `reserved_encoded_capacity_bytes` / `used_encoded_bytes` and compute
///    `threshold = f(new_cap)` (same `f` as the upload-side cap check).
/// 3. If `used_encoded > threshold`, reject 400 — lowering the cap would
///    orphan currently-stored data.
/// 4. If `reserved_encoded > threshold`, submit a Pearl-signed
///    `decrease_storage_pool_capacity_by_size` PTB. The contract's own
///    assertion guarantees the chain refuses to cut into `used_encoded`,
///    so a concurrent race surfaces as a structured 400 with the chain's
///    MoveAbort context.
/// 5. Persist the new cap and reconcile DB pool counters to the
///    post-shrink on-chain truth only when the shrink succeeded (or none
///    was needed).
pub async fn update_max_storage(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
    Json(body): Json<UpdateMaxStorageRequest>,
) -> Result<Json<UpdateMaxStorageResponse>, AppError> {
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;
    if body.max_unencoded_bytes <= 0 {
        return Err(AppError::BadRequest(
            "max_unencoded_bytes must be a positive integer".into(),
        ));
    }
    let new_cap = body.max_unencoded_bytes;
    let new_cap_u64 = new_cap as u64;

    // Snapshot the old cap before any DB write so the audit event can
    // record the transition the admin actually requested.
    let old_cap = db::accounts::get_max_unencoded_bytes(&state.db, &account_id)
        .await?
        .ok_or(AppError::NotFound)?;

    // (1) DB-only fast path: no on-chain pool yet.
    let pool_state = db::accounts::get_storage_pool(&state.db, &account_id).await?;
    let Some(pool_state) = pool_state else {
        let updated =
            db::accounts::set_max_unencoded_bytes(&state.db, &account_id, new_cap).await?;
        if !updated {
            return Err(AppError::NotFound);
        }
        record_max_storage_audit(
            &state.db,
            &auth.app_id,
            &auth.admin_key_id,
            &account_id,
            old_cap,
            new_cap,
            None,
            None,
        )
        .await?;
        tracing::info!(
            app_id = %auth.app_id, account_id = %account_id,
            old_max = old_cap, new_max = new_cap,
            "max_unencoded_bytes updated (no on-chain pool)",
        );
        return Ok(Json(UpdateMaxStorageResponse {
            account_id,
            max_unencoded_bytes: new_cap,
            pool: None,
            shrink_tx_digest: None,
        }));
    };

    // The account has a pool, so we need the SuiReadClient and Pearl
    // connection to (potentially) submit a shrink PTB. Both are
    // populated when the server is configured for direct-Walrus mode;
    // integration tests with no on-chain pool never reach this branch.
    let read_client = state.read_client.as_ref().ok_or_else(|| {
        AppError::ServiceUnavailable(
            "on-chain pool shrink requires a SuiReadClient (server is not in direct-Walrus mode)"
                .into(),
        )
    })?;
    let pearl = state.pearl.as_ref().ok_or_else(|| {
        AppError::ServiceUnavailable(
            "on-chain pool shrink requires a Pearl connection (server is not in direct-Walrus mode)"
                .into(),
        )
    })?;
    let rpc_url = state
        .config
        .sui_rpc_url
        .as_ref()
        .ok_or_else(|| AppError::ServiceUnavailable("SUI_RPC_URL is not configured".into()))?;

    let pool_object_id: sui_types::base_types::ObjectID =
        pool_state.object_id.parse().map_err(|e| {
            AppError::Internal(format!(
                "invalid stored pool ObjectID {}: {e}",
                pool_state.object_id
            ))
        })?;

    // (2) Read on-chain truth — never trust DB counters here; they
    // can drift (Phase 2 race) and the cap-orphan check must be exact.
    let on_chain = sui_object_reader::read_storage_pool_state(rpc_url, pool_object_id)
        .await
        .map_err(|e| {
            AppError::BlobStore(BlobStoreError::Upstream(format!(
                "read storage pool state: {e}"
            )))
        })?;
    let used_encoded = on_chain.used_encoded_bytes;
    let mut reserved_encoded = on_chain.reserved_encoded_bytes;

    let n_shards = read_client.n_shards().await.map_err(|e| {
        AppError::BlobStore(BlobStoreError::Upstream(format!("read n_shards: {e}")))
    })?;
    // Same saturating fallback as `storage_cap::enforce_storage_cap`:
    // if the new cap exceeds the single-blob encoded limit for
    // `n_shards`, the threshold is effectively `u64::MAX` and no
    // orphan/shrink is possible.
    let threshold = encoded_blob_length_for_n_shards(n_shards, new_cap_u64, EncodingType::RS2)
        .unwrap_or(u64::MAX);

    // (3) Hard 400 if lowering would orphan currently-stored data.
    if used_encoded > threshold {
        return Err(AppError::MaxStorageWouldOrphan {
            max_unencoded_bytes: new_cap,
            used_encoded_bytes: clamp_u64_to_i64(used_encoded),
            threshold_encoded: clamp_u64_to_i64(threshold),
        });
    }

    // (4) Conditional shrink.
    let mut shrink_tx_digest: Option<String> = None;
    if reserved_encoded > threshold {
        let extract_size = reserved_encoded - threshold;
        match admin_storage_pool::decrease_storage_pool_capacity(
            read_client.as_ref(),
            pearl,
            rpc_url,
            &account_id,
            pool_object_id,
            extract_size,
        )
        .await
        {
            Ok(outcome) => {
                shrink_tx_digest = Some(outcome.tx_digest.to_string());
                reserved_encoded = reserved_encoded.saturating_sub(extract_size);
                // Reconcile DB pool counters to on-chain post-shrink
                // truth so the next upload's `grow_by` math starts
                // from the right reserved value.
                db::accounts::reconcile_pool_after_drift(
                    &state.db,
                    &account_id,
                    clamp_u64_to_i64(reserved_encoded),
                    clamp_u64_to_i64(used_encoded),
                )
                .await?;
            }
            Err(DecreaseError::WouldOrphan { description, .. }) => {
                return Err(AppError::MaxStorageShrinkAborted {
                    description,
                    max_unencoded_bytes: new_cap,
                    extract_size: clamp_u64_to_i64(extract_size),
                });
            }
            Err(DecreaseError::Internal(msg)) => {
                return Err(AppError::Internal(msg));
            }
            Err(DecreaseError::Upstream(msg)) => {
                return Err(AppError::BlobStore(BlobStoreError::Upstream(msg)));
            }
        }
    }

    // (5) Persist the new cap only after the on-chain side succeeded.
    let updated = db::accounts::set_max_unencoded_bytes(&state.db, &account_id, new_cap).await?;
    if !updated {
        return Err(AppError::NotFound);
    }

    let extract_size_i64 = shrink_tx_digest
        .as_ref()
        .map(|_| clamp_u64_to_i64(on_chain.reserved_encoded_bytes - reserved_encoded));
    record_max_storage_audit(
        &state.db,
        &auth.app_id,
        &auth.admin_key_id,
        &account_id,
        old_cap,
        new_cap,
        extract_size_i64,
        shrink_tx_digest.as_deref(),
    )
    .await?;
    tracing::info!(
        app_id = %auth.app_id, account_id = %account_id,
        old_max = old_cap, new_max = new_cap,
        shrink_extract_size = ?extract_size_i64,
        shrink_tx_digest = ?shrink_tx_digest,
        "max_unencoded_bytes updated",
    );

    Ok(Json(UpdateMaxStorageResponse {
        account_id,
        max_unencoded_bytes: new_cap,
        pool: Some(PoolOnChainState {
            reserved_encoded_bytes: clamp_u64_to_i64(reserved_encoded),
            used_encoded_bytes: clamp_u64_to_i64(used_encoded),
        }),
        shrink_tx_digest,
    }))
}

/// Saturate-on-overflow `u64 → i64` for JSON-body fields and DB
/// `BIGINT` columns. The on-chain counters fit `i64` by construction
/// in the deployments we care about; this is purely defensive.
fn clamp_u64_to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

#[allow(clippy::too_many_arguments)]
async fn record_max_storage_audit(
    db: &db::DbPool,
    app_id: &AppId,
    admin_key_id: &str,
    account_id: &AccountId,
    old_max: i64,
    new_max: i64,
    shrink_extract_size: Option<i64>,
    shrink_tx_digest: Option<&str>,
) -> Result<(), AppError> {
    db::audit_events::record_audit_event(
        db,
        app_id,
        Some(admin_key_id),
        "account.max_storage_updated",
        serde_json::json!({
            "account_id": account_id.to_string(),
            "old_max": old_max,
            "new_max": new_max,
            "shrink_extract_size": shrink_extract_size,
            "shrink_tx_digest": shrink_tx_digest,
        }),
    )
    .await?;
    Ok(())
}
