use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use crate::{
    AppState,
    auth::{self, AuthenticatedAccount},
    db,
    error::AppError,
    models::{ApiKeyWithSecret, CreateAccountResponse, ErrorResponse, WalletInfo, WalletsResponse},
};

#[utoipa::path(
    post,
    path = "/account/api-keys",
    tag = "Account",
    security(("bearer" = [])),
    responses(
        (status = 201, description = "API key created", body = ApiKeyWithSecret),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// Generate a new API key for the authenticated account.
pub async fn create_api_key(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
) -> Result<(StatusCode, Json<ApiKeyWithSecret>), AppError> {
    let raw_key = auth::generate_api_key();
    let key_hash = auth::hash_api_key(&raw_key);
    let prefix = auth::key_prefix(&raw_key);

    let api_key =
        db::api_keys::create_api_key(&state.db, &auth.account_id, &key_hash, &prefix, &raw_key)
            .await?;

    Ok((StatusCode::CREATED, Json(api_key)))
}

#[utoipa::path(
    delete,
    path = "/account/api-keys/{key_id}",
    tag = "Account",
    security(("bearer" = [])),
    params(("key_id" = String, Path, description = "API key ID to revoke")),
    responses(
        (status = 204, description = "API key revoked"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "API key not found", body = ErrorResponse),
    ),
)]
/// Revoke an API key by its ID. Only the owner of the key can revoke it.
pub async fn revoke_api_key(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path(key_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let revoked = db::api_keys::revoke_api_key(&state.db, &key_id, &auth.account_id).await?;
    if revoked {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

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
    path = "/account/wallets",
    tag = "Account",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Wallet information", body = WalletsResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// Get wallet information for the authenticated account.
pub async fn get_wallets(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
) -> Result<Json<WalletsResponse>, AppError> {
    let account = db::accounts::get_account(&state.db, &auth.account_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let Some(pearl_account_id) = account.pearl_account_id else {
        return Ok(Json(WalletsResponse {
            provisioned: false,
            wallets: vec![],
        }));
    };

    let Some(ref pearl) = state.pearl else {
        return Ok(Json(WalletsResponse {
            provisioned: true,
            wallets: vec![],
        }));
    };

    let address = pearl
        .get_address(&pearl_account_id)
        .await
        .map_err(|e| AppError::Internal(format!("Pearl get_address failed: {e}")))?;

    Ok(Json(WalletsResponse {
        provisioned: true,
        wallets: vec![WalletInfo { address }],
    }))
}

// Debug endpoint

#[utoipa::path(
    post,
    path = "/debug/create-account",
    tag = "Debug",
    responses(
        (status = 201, description = "Account created", body = CreateAccountResponse),
    ),
)]
/// Create a new account with an initial API key. Only available when debug endpoints are enabled.
/// If Pearl is connected, automatically provisions a wallet for the new account.
pub async fn debug_create_account(
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<CreateAccountResponse>), AppError> {
    let pearl_account_id = if let Some(ref pearl) = state.pearl {
        let resp = pearl
            .create_account()
            .await
            .map_err(|e| AppError::Internal(format!("Pearl account creation failed: {e}")))?;
        Some(resp.account_id)
    } else {
        None
    };

    let account = db::accounts::create_account(&state.db, pearl_account_id.as_deref()).await?;

    let raw_key = auth::generate_api_key();
    let key_hash = auth::hash_api_key(&raw_key);
    let prefix = auth::key_prefix(&raw_key);

    let api_key =
        db::api_keys::create_api_key(&state.db, &account.id, &key_hash, &prefix, &raw_key).await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateAccountResponse {
            account_id: account.id,
            api_key,
        }),
    ))
}
