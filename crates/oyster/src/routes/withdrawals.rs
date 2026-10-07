//! Admin fund-withdrawal routes (admin-key authenticated).
//!
//! Moving SUI/WAL out of an account's Pearl wallet is the one admin
//! operation that is both irreversible and directly valuable to an
//! attacker, so it is not granted to "whoever holds an admin key". The
//! controls, each independent of the others:
//!
//! 1. **Kill switch.** `OYSTER_WITHDRAWALS_ENABLED` (default off): the
//!    routes answer 404 until an operator turns them on.
//! 2. **Pre-registered destination with cooldown.** Funds can only go
//!    to the address registered for the account, and only once that
//!    registration is older than `OYSTER_WITHDRAWAL_ADDRESS_COOLDOWN_SECS`.
//!    Registering or replacing the address is audited and pushed to the
//!    app's webhook, so a hostile registration is visible while the
//!    legitimate operator can still revoke the compromised key.
//! 3. **Dual control.** A withdrawal is requested by one admin key and
//!    must be approved by a *different* active admin key of the same
//!    app. Admin keys cannot be minted through the admin API (only the
//!    CLI or the signup dashboard), so a single stolen key cannot
//!    self-approve.
//! 4. **Bounded window.** A pending request expires after
//!    `OYSTER_WITHDRAWAL_REQUEST_TTL_SECS`; the destination and cooldown
//!    are re-checked at approval time, not just at request time.
//! 5. **Ledger.** Every request, approval, cancellation and outcome is a
//!    row in `withdrawals` plus an audit event naming the admin keys
//!    involved, and completions are pushed to the webhook.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{Duration as ChronoDuration, Utc};
use sui_types::base_types::SuiAddress;

use crate::{
    AccountId, AppId, AppState,
    app_admin::AuthenticatedApp,
    db::{
        self,
        withdrawals::{Withdrawal, WithdrawalStatus},
    },
    error::AppError,
    models::{
        CreateWithdrawalRequest, ErrorResponse, SetWithdrawalAddressRequest,
        WithdrawalAddressResponse, WithdrawalListResponse, WithdrawalResponse,
    },
    webhook::{
        self, EVENT_TYPE_WITHDRAWAL_COMPLETED, EVENT_TYPE_WITHDRAWAL_REQUESTED,
        WithdrawalEventPayload,
    },
    withdrawal::{self, WithdrawalAmounts, WithdrawalExecError},
};

/// The routes do not exist unless the operator has enabled them.
fn ensure_enabled(state: &AppState) -> Result<(), AppError> {
    if state.config.withdrawals_enabled {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

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

/// Parse and normalize a Sui address (`0x` + 64 hex, lowercase).
fn parse_address(raw: &str) -> Result<SuiAddress, AppError> {
    raw.trim()
        .parse::<SuiAddress>()
        .map_err(|e| AppError::BadRequest(format!("invalid Sui address: {e}")))
}

fn address_response(a: db::withdrawals::WithdrawalAddress) -> WithdrawalAddressResponse {
    let usable_now = db::withdrawals::parse_ts(&a.usable_at).is_some_and(|t| t <= Utc::now());
    WithdrawalAddressResponse {
        account_id: a.account_id,
        address: a.address,
        registered_at: a.registered_at,
        usable_at: a.usable_at,
        usable_now,
    }
}

fn withdrawal_response(w: Withdrawal) -> WithdrawalResponse {
    WithdrawalResponse {
        id: w.id,
        account_id: w.account_id,
        destination: w.destination,
        sui_mist: w.sui_mist,
        wal_frost: w.wal_frost,
        all: w.drain,
        status: w.status.as_str().to_string(),
        requested_by_admin_key_id: w.requested_by_admin_key_id,
        approved_by_admin_key_id: w.approved_by_admin_key_id,
        tx_digest: w.tx_digest,
        error: w.error,
        created_at: w.created_at,
        expires_at: w.expires_at,
        updated_at: w.updated_at,
    }
}

/// Fire-and-forget webhook delivery; never blocks the response.
fn push_event(state: &AppState, app_id: AppId, payload: WithdrawalEventPayload) {
    let db = state.db.clone();
    tokio::spawn(async move {
        webhook::notify_app_withdrawal_event(&db, &app_id, &payload).await;
    });
}

#[utoipa::path(
    put,
    path = "/admin/accounts/{account_id}/withdrawal-address",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    request_body(content = SetWithdrawalAddressRequest, content_type = "application/json"),
    responses(
        (status = 200, description = "Address registered; withdrawals to it can be approved from `usable_at`", body = WithdrawalAddressResponse),
        (status = 400, description = "Invalid Sui address", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Account belongs to another app", body = ErrorResponse),
        (status = 404, description = "Account not found, or withdrawals disabled on this deployment", body = ErrorResponse),
    ),
)]
/// Register (or replace) the only address this account's funds may be
/// withdrawn to. Replacing it restarts the cooldown. Audited and pushed
/// to the app webhook as `account.withdrawal_address_set`.
pub async fn set_withdrawal_address(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
    Json(body): Json<SetWithdrawalAddressRequest>,
) -> Result<Json<WithdrawalAddressResponse>, AppError> {
    ensure_enabled(&state)?;
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;
    let address = parse_address(&body.address)?;
    let address_str = address.to_string();

    let now = Utc::now();
    let usable_at = now
        + ChronoDuration::seconds(
            i64::try_from(state.config.withdrawal_address_cooldown_secs).unwrap_or(i64::MAX / 4),
        );
    let previous = db::withdrawals::get_withdrawal_address(&state.db, &account_id).await?;
    let registered = db::withdrawals::set_withdrawal_address(
        &state.db,
        &account_id,
        &address_str,
        &auth.admin_key_id,
        now,
        usable_at,
    )
    .await?;

    db::audit_events::record_audit_event(
        &state.db,
        &auth.app_id,
        Some(&auth.admin_key_id),
        "account.withdrawal_address_set",
        serde_json::json!({
            "account_id": account_id.to_string(),
            "address": address_str,
            "previous_address": previous.as_ref().map(|p| p.address.clone()),
            "usable_at": registered.usable_at,
        }),
    )
    .await?;
    tracing::info!(
        app_id = %auth.app_id, %account_id, address = %address_str,
        usable_at = %registered.usable_at, admin_key_id = %auth.admin_key_id,
        "withdrawal address registered",
    );
    push_event(
        &state,
        auth.app_id,
        WithdrawalEventPayload::address_set(
            account_id,
            address_str,
            registered.usable_at.clone(),
            auth.admin_key_id.clone(),
        ),
    );

    Ok(Json(address_response(registered)))
}

#[utoipa::path(
    get,
    path = "/admin/accounts/{account_id}/withdrawal-address",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    responses(
        (status = 200, description = "The registered address", body = WithdrawalAddressResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Account belongs to another app", body = ErrorResponse),
        (status = 404, description = "No address registered, account not found, or withdrawals disabled", body = ErrorResponse),
    ),
)]
/// The account's registered withdrawal address, if any.
pub async fn get_withdrawal_address(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
) -> Result<Json<WithdrawalAddressResponse>, AppError> {
    ensure_enabled(&state)?;
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;
    let a = db::withdrawals::get_withdrawal_address(&state.db, &account_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(address_response(a)))
}

#[utoipa::path(
    delete,
    path = "/admin/accounts/{account_id}/withdrawal-address",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    responses(
        (status = 204, description = "Address cleared; pending requests can no longer be approved"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Account belongs to another app", body = ErrorResponse),
        (status = 404, description = "No address registered, account not found, or withdrawals disabled", body = ErrorResponse),
    ),
)]
/// Clear the registered withdrawal address. Any pending request stays
/// on the ledger but can no longer be approved.
pub async fn clear_withdrawal_address(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
) -> Result<StatusCode, AppError> {
    ensure_enabled(&state)?;
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;
    let previous = db::withdrawals::get_withdrawal_address(&state.db, &account_id).await?;
    if !db::withdrawals::clear_withdrawal_address(&state.db, &account_id).await? {
        return Err(AppError::NotFound);
    }
    db::audit_events::record_audit_event(
        &state.db,
        &auth.app_id,
        Some(&auth.admin_key_id),
        "account.withdrawal_address_cleared",
        serde_json::json!({
            "account_id": account_id.to_string(),
            "previous_address": previous.map(|p| p.address),
        }),
    )
    .await?;
    tracing::info!(app_id = %auth.app_id, %account_id, admin_key_id = %auth.admin_key_id, "withdrawal address cleared");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/admin/accounts/{account_id}/withdrawals",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    request_body(content = CreateWithdrawalRequest, content_type = "application/json"),
    responses(
        (status = 202, description = "Request recorded as `pending`; a different admin key must approve it", body = WithdrawalResponse),
        (status = 400, description = "Invalid amounts", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Account belongs to another app", body = ErrorResponse),
        (status = 404, description = "Account not found, or withdrawals disabled", body = ErrorResponse),
        (status = 409, description = "No withdrawal address registered, or a pending request already exists", body = ErrorResponse),
    ),
)]
/// Request a withdrawal to the account's registered address. Nothing
/// moves until a *different* admin key approves the request.
pub async fn create_withdrawal(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
    Json(body): Json<CreateWithdrawalRequest>,
) -> Result<(StatusCode, Json<WithdrawalResponse>), AppError> {
    ensure_enabled(&state)?;
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;
    let amounts = WithdrawalAmounts {
        sui_mist: body.sui_mist,
        wal_frost: body.wal_frost,
        drain: body.all.unwrap_or(false),
    };
    amounts.validate().map_err(AppError::BadRequest)?;
    let to_i64 = |v: Option<u64>| -> Result<Option<i64>, AppError> {
        v.map(|n| {
            i64::try_from(n).map_err(|_| AppError::BadRequest("amount is out of range".into()))
        })
        .transpose()
    };
    let sui_mist = to_i64(amounts.sui_mist)?;
    let wal_frost = to_i64(amounts.wal_frost)?;

    let address = db::withdrawals::get_withdrawal_address(&state.db, &account_id)
        .await?
        .ok_or_else(|| {
            AppError::Conflict(
                "no withdrawal address registered for this account; \
                 register one and wait out the cooldown first"
                    .into(),
            )
        })?;

    let now = Utc::now();
    if let Some(pending) =
        db::withdrawals::get_live_pending_withdrawal(&state.db, &account_id, now).await?
    {
        return Err(AppError::Conflict(format!(
            "withdrawal {} is already pending approval; approve or cancel it first",
            pending.id
        )));
    }

    let expires_at = now
        + ChronoDuration::seconds(
            i64::try_from(state.config.withdrawal_request_ttl_secs).unwrap_or(i64::MAX / 4),
        );
    let w = db::withdrawals::create_withdrawal(
        &state.db,
        &account_id,
        &auth.app_id,
        &address.address,
        sui_mist,
        wal_frost,
        amounts.drain,
        &auth.admin_key_id,
        now,
        expires_at,
    )
    .await?;

    db::audit_events::record_audit_event(
        &state.db,
        &auth.app_id,
        Some(&auth.admin_key_id),
        "account.withdrawal_requested",
        serde_json::json!({
            "account_id": account_id.to_string(),
            "withdrawal_id": w.id,
            "destination": w.destination,
            "sui_mist": w.sui_mist,
            "wal_frost": w.wal_frost,
            "all": w.drain,
            "expires_at": w.expires_at,
        }),
    )
    .await?;
    tracing::info!(
        app_id = %auth.app_id, %account_id, withdrawal_id = %w.id,
        destination = %w.destination, sui_mist = ?w.sui_mist, wal_frost = ?w.wal_frost,
        all = w.drain, admin_key_id = %auth.admin_key_id, "withdrawal requested",
    );
    push_event(
        &state,
        auth.app_id,
        WithdrawalEventPayload::from_withdrawal(
            EVENT_TYPE_WITHDRAWAL_REQUESTED,
            &w,
            &auth.admin_key_id,
        ),
    );

    Ok((StatusCode::ACCEPTED, Json(withdrawal_response(w))))
}

#[utoipa::path(
    post,
    path = "/admin/withdrawals/{withdrawal_id}/approve",
    tag = "Admin",
    security(("bearer" = [])),
    params(("withdrawal_id" = String, Path, description = "Withdrawal request ID")),
    responses(
        (status = 200, description = "Transfer landed on-chain; `tx_digest` set", body = WithdrawalResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Approver is the requester, or the request belongs to another app", body = ErrorResponse),
        (status = 404, description = "Unknown request, or withdrawals disabled", body = ErrorResponse),
        (status = 409, description = "Request is not pending, has expired, or the destination is no longer the registered, matured address", body = ErrorResponse),
        (status = 502, description = "On-chain transfer failed; the request is now `failed`", body = ErrorResponse),
        (status = 503, description = "Account is mid key-rotation, or this deployment has no chain access", body = ErrorResponse),
    ),
)]
/// Approve and execute a pending withdrawal. The approving admin key
/// must differ from the one that made the request.
pub async fn approve_withdrawal(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(withdrawal_id): Path<String>,
) -> Result<Json<WithdrawalResponse>, AppError> {
    ensure_enabled(&state)?;
    let w = db::withdrawals::get_withdrawal(&state.db, &withdrawal_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if w.app_id != auth.app_id {
        return Err(AppError::Forbidden(
            "withdrawal does not belong to this app".into(),
        ));
    }
    if w.requested_by_admin_key_id == auth.admin_key_id {
        return Err(AppError::Forbidden(
            "a withdrawal must be approved by a different admin key than the one that requested it"
                .into(),
        ));
    }
    let now = Utc::now();
    if w.status != WithdrawalStatus::Pending {
        return Err(AppError::Conflict(format!(
            "withdrawal is {}, not pending",
            w.status.as_str()
        )));
    }
    if w.is_expired(now) {
        return Err(AppError::Conflict(
            "withdrawal request has expired; create a new one".into(),
        ));
    }

    // Policy is re-checked at approval: the address must still be the
    // registered one and its cooldown must have elapsed by now.
    let registered = db::withdrawals::get_withdrawal_address(&state.db, &w.account_id)
        .await?
        .ok_or_else(|| {
            AppError::Conflict("withdrawal address has been cleared since the request".into())
        })?;
    if registered.address != w.destination {
        return Err(AppError::Conflict(
            "withdrawal address has changed since the request; create a new request".into(),
        ));
    }
    let usable = db::withdrawals::parse_ts(&registered.usable_at).is_some_and(|t| t <= now);
    if !usable {
        return Err(AppError::Conflict(format!(
            "withdrawal address is still in its cooldown until {} UTC",
            registered.usable_at
        )));
    }

    // Execution prerequisites, checked before the state transition so a
    // misconfigured deployment leaves the request approvable later.
    let (key_version, migrating) =
        db::accounts::get_key_version_and_migrating(&state.db, &w.account_id)
            .await?
            .ok_or(AppError::NotFound)?;
    if migrating {
        return Err(AppError::ServiceUnavailable(
            "account key migration in progress; retry shortly".into(),
        ));
    }
    let (read_client, pearl, rpc_url) = match (
        state.read_client.as_ref(),
        state.pearl.as_ref(),
        state.config.sui_rpc_url.as_deref(),
    ) {
        (Some(rc), Some(p), Some(url)) => (rc, p, url),
        _ => {
            return Err(AppError::ServiceUnavailable(
                "this deployment has no on-chain access; withdrawals cannot be executed".into(),
            ));
        }
    };
    let destination = parse_address(&w.destination)?;
    let amounts = WithdrawalAmounts {
        sui_mist: w.sui_mist.map(|v| v as u64),
        wal_frost: w.wal_frost.map(|v| v as u64),
        drain: w.drain,
    };

    // Claim the request. Only one approver can win this CAS.
    let claimed = db::withdrawals::transition_withdrawal(
        &state.db,
        &w.id,
        WithdrawalStatus::Pending,
        WithdrawalStatus::Executing,
        Some(&auth.admin_key_id),
        None,
        None,
        now,
    )
    .await?;
    if !claimed {
        return Err(AppError::Conflict(
            "withdrawal was approved or cancelled concurrently".into(),
        ));
    }

    let result = withdrawal::execute_withdrawal(
        read_client,
        pearl,
        rpc_url,
        &w.account_id,
        key_version,
        destination,
        &amounts,
    )
    .await;

    let (final_status, digest, error) = match &result {
        Ok(d) => (WithdrawalStatus::Completed, Some(d.to_string()), None),
        Err(e) => (WithdrawalStatus::Failed, None, Some(e.to_string())),
    };
    db::withdrawals::transition_withdrawal(
        &state.db,
        &w.id,
        WithdrawalStatus::Executing,
        final_status,
        None,
        digest.as_deref(),
        error.as_deref(),
        Utc::now(),
    )
    .await?;
    db::audit_events::record_audit_event(
        &state.db,
        &auth.app_id,
        Some(&auth.admin_key_id),
        "account.withdrawal_approved",
        serde_json::json!({
            "account_id": w.account_id.to_string(),
            "withdrawal_id": w.id,
            "destination": w.destination,
            "sui_mist": w.sui_mist,
            "wal_frost": w.wal_frost,
            "all": w.drain,
            "requested_by_admin_key_id": w.requested_by_admin_key_id,
            "status": final_status.as_str(),
            "tx_digest": digest,
            "error": error,
        }),
    )
    .await?;

    match result {
        Ok(d) => {
            tracing::info!(
                app_id = %auth.app_id, account_id = %w.account_id, withdrawal_id = %w.id,
                destination = %w.destination, tx_digest = %d,
                requested_by = %w.requested_by_admin_key_id, approved_by = %auth.admin_key_id,
                "withdrawal completed",
            );
            let updated = db::withdrawals::get_withdrawal(&state.db, &w.id)
                .await?
                .ok_or(AppError::NotFound)?;
            push_event(
                &state,
                auth.app_id,
                WithdrawalEventPayload::from_withdrawal(
                    EVENT_TYPE_WITHDRAWAL_COMPLETED,
                    &updated,
                    &auth.admin_key_id,
                ),
            );
            Ok(Json(withdrawal_response(updated)))
        }
        Err(e) => {
            tracing::error!(
                app_id = %auth.app_id, account_id = %w.account_id, withdrawal_id = %w.id,
                error = %e, "withdrawal failed",
            );
            Err(match e {
                WithdrawalExecError::InsufficientBalance(msg) => {
                    AppError::Conflict(format!("withdrawal failed: {msg}"))
                }
                WithdrawalExecError::Invalid(msg) => {
                    AppError::BadRequest(format!("withdrawal failed: {msg}"))
                }
                WithdrawalExecError::Upstream(msg) => {
                    AppError::BlobStore(crate::blob_store::BlobStoreError::Upstream(format!(
                        "withdrawal transfer: {msg}"
                    )))
                }
            })
        }
    }
}

#[utoipa::path(
    post,
    path = "/admin/withdrawals/{withdrawal_id}/cancel",
    tag = "Admin",
    security(("bearer" = [])),
    params(("withdrawal_id" = String, Path, description = "Withdrawal request ID")),
    responses(
        (status = 200, description = "Request cancelled", body = WithdrawalResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Request belongs to another app", body = ErrorResponse),
        (status = 404, description = "Unknown request, or withdrawals disabled", body = ErrorResponse),
        (status = 409, description = "Request is not pending", body = ErrorResponse),
    ),
)]
/// Cancel a pending withdrawal. Any admin key of the app may cancel,
/// including the requester: cancelling is the safe direction.
pub async fn cancel_withdrawal(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(withdrawal_id): Path<String>,
) -> Result<Json<WithdrawalResponse>, AppError> {
    ensure_enabled(&state)?;
    let w = db::withdrawals::get_withdrawal(&state.db, &withdrawal_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if w.app_id != auth.app_id {
        return Err(AppError::Forbidden(
            "withdrawal does not belong to this app".into(),
        ));
    }
    let cancelled = db::withdrawals::transition_withdrawal(
        &state.db,
        &w.id,
        WithdrawalStatus::Pending,
        WithdrawalStatus::Cancelled,
        None,
        None,
        None,
        Utc::now(),
    )
    .await?;
    if !cancelled {
        return Err(AppError::Conflict(format!(
            "withdrawal is {}, not pending",
            w.status.as_str()
        )));
    }
    db::audit_events::record_audit_event(
        &state.db,
        &auth.app_id,
        Some(&auth.admin_key_id),
        "account.withdrawal_cancelled",
        serde_json::json!({
            "account_id": w.account_id.to_string(),
            "withdrawal_id": w.id,
        }),
    )
    .await?;
    tracing::info!(app_id = %auth.app_id, withdrawal_id = %w.id, admin_key_id = %auth.admin_key_id, "withdrawal cancelled");
    let updated = db::withdrawals::get_withdrawal(&state.db, &w.id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(withdrawal_response(updated)))
}

#[utoipa::path(
    get,
    path = "/admin/withdrawals/{withdrawal_id}",
    tag = "Admin",
    security(("bearer" = [])),
    params(("withdrawal_id" = String, Path, description = "Withdrawal request ID")),
    responses(
        (status = 200, description = "The withdrawal", body = WithdrawalResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Request belongs to another app", body = ErrorResponse),
        (status = 404, description = "Unknown request, or withdrawals disabled", body = ErrorResponse),
    ),
)]
/// Fetch one withdrawal request.
pub async fn get_withdrawal(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(withdrawal_id): Path<String>,
) -> Result<Json<WithdrawalResponse>, AppError> {
    ensure_enabled(&state)?;
    let w = db::withdrawals::get_withdrawal(&state.db, &withdrawal_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if w.app_id != auth.app_id {
        return Err(AppError::Forbidden(
            "withdrawal does not belong to this app".into(),
        ));
    }
    Ok(Json(withdrawal_response(w)))
}

#[utoipa::path(
    get,
    path = "/admin/accounts/{account_id}/withdrawals",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    responses(
        (status = 200, description = "All withdrawal requests for the account, newest first", body = WithdrawalListResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Account belongs to another app", body = ErrorResponse),
        (status = 404, description = "Account not found, or withdrawals disabled", body = ErrorResponse),
    ),
)]
/// List an account's withdrawal requests.
pub async fn list_withdrawals(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
) -> Result<Json<WithdrawalListResponse>, AppError> {
    ensure_enabled(&state)?;
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;
    let rows = db::withdrawals::list_withdrawals(&state.db, &account_id).await?;
    Ok(Json(WithdrawalListResponse {
        withdrawals: rows.into_iter().map(withdrawal_response).collect(),
    }))
}
