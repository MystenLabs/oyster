use axum::{Json, extract::State, http::StatusCode};

use crate::{
    AppState,
    auth::AuthenticatedAccount,
    error::AppError,
    models::{ErrorResponse, WalletResponse},
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

    let address = pearl
        .get_address(&auth.account_id)
        .await
        .map_err(|e| AppError::Internal(format!("Pearl get_address failed: {e}")))?;

    Ok(Json(WalletResponse { address }))
}
