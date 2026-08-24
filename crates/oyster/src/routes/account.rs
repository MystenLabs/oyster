use axum::{Json, extract::State, http::StatusCode};

use crate::{
    AppState,
    auth::AuthenticatedAccount,
    db,
    error::AppError,
    models::{ErrorResponse, ExtendRequestResponse, WalletResponse},
};

// Stubs

#[utoipa::path(
    put,
    path = "/account/billing",
    tag = "Account",
    security(("bearer" = [])),
    responses(
        (status = 501, description = "Not implemented", body = ErrorResponse),
    ),
)]
/// Update billing information for the authenticated account. Not yet implemented.
pub async fn update_billing() -> Result<StatusCode, AppError> {
    Err(AppError::NotImplemented)
}

#[utoipa::path(
    get,
    path = "/account/report",
    tag = "Account",
    security(("bearer" = [])),
    responses(
        (status = 501, description = "Not implemented", body = ErrorResponse),
    ),
)]
/// Retrieve a usage report for the authenticated account. Not yet implemented.
pub async fn get_report() -> Result<StatusCode, AppError> {
    Err(AppError::NotImplemented)
}

#[utoipa::path(
    post,
    path = "/account/transfer",
    tag = "Account",
    security(("bearer" = [])),
    responses(
        (status = 501, description = "Not implemented", body = ErrorResponse),
    ),
)]
/// Transfer ownership of resources to another account. Not yet implemented.
pub async fn transfer() -> Result<StatusCode, AppError> {
    Err(AppError::NotImplemented)
}

#[utoipa::path(
    get,
    path = "/account/wallet",
    tag = "Account",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Wallet information", body = WalletResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 503, description = "Wallet service unavailable", body = ErrorResponse),
    ),
)]
/// Get wallet information for the authenticated account.
pub async fn get_wallet(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
) -> Result<Json<WalletResponse>, AppError> {
    let Some(ref pearl) = state.pearl else {
        return Err(AppError::ServiceUnavailable(
            "wallet service not configured".into(),
        ));
    };

    let key_version = db::accounts::get_key_version(&state.db, &auth.account_id)
        .await?
        .ok_or_else(|| AppError::Internal("authenticated account has no row".into()))?;
    let address = pearl
        .get_address(&auth.account_id, key_version)
        .await
        .map_err(|e| AppError::Internal(format!("Pearl get_address failed: {e}")))?;

    Ok(Json(WalletResponse { address }))
}

#[utoipa::path(
    post,
    path = "/account/extend",
    tag = "Account",
    security(("bearer" = [])),
    responses(
        (status = 202, description = "Extension retry scheduled", body = ExtendRequestResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Account has no storage pool", body = ErrorResponse),
    ),
)]
/// Request an immediate storage-pool extension retry.
///
/// Call this after funding the account's wallet (see `/account/wallet` or
/// the `account.funding_required` webhook): it clears the extension
/// worker's retry backoff so the pool is re-attempted on the next worker
/// cycle instead of waiting out the exponential backoff. Issues no chain
/// transactions itself — the background worker performs the extension.
pub async fn request_extend(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
) -> Result<(StatusCode, Json<ExtendRequestResponse>), AppError> {
    let pool_state = db::accounts::get_storage_pool(&state.db, &auth.account_id)
        .await?
        .ok_or(AppError::NotFound)?;

    if !db::accounts::request_extension_retry(&state.db, &auth.account_id).await? {
        // Pool vanished between the two statements (e.g. concurrent
        // expired-pool reset) — same outcome as never having had one.
        return Err(AppError::NotFound);
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(ExtendRequestResponse {
            status: "scheduled".into(),
            pool_end_epoch: pool_state.end_epoch,
        }),
    ))
}
