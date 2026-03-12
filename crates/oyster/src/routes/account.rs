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
    models::{
        AccessKey,
        AccessKeyWithSecret,
        ApiKeyWithSecret,
        CreateAccountResponse,
        ErrorResponse,
        WalletInfo,
        WalletResponse,
    },
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

// Access keys

/// Maximum number of active access keys per account.
const MAX_ACCESS_KEYS: i64 = 3;

#[utoipa::path(
    post,
    path = "/account/access-keys",
    tag = "Account",
    security(("bearer" = [])),
    responses(
        (status = 201, description = "Access key created", body = AccessKeyWithSecret),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 409, description = "Access key limit reached", body = ErrorResponse),
    ),
)]
/// Create a new S3 access key for the authenticated account. Limited to 3 active keys.
pub async fn create_access_key(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
) -> Result<(StatusCode, Json<AccessKeyWithSecret>), AppError> {
    let count = db::access_keys::count_access_keys(&state.db, &auth.account_id).await?;
    if count >= MAX_ACCESS_KEYS {
        return Err(AppError::Conflict(format!(
            "access key limit reached ({MAX_ACCESS_KEYS})"
        )));
    }
    let key = db::access_keys::create_access_key(&state.db, &auth.account_id).await?;
    Ok((StatusCode::CREATED, Json(key)))
}

#[utoipa::path(
    get,
    path = "/account/access-keys",
    tag = "Account",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "List of access keys", body = Vec<AccessKey>),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// List all S3 access keys for the authenticated account. Never returns secrets.
pub async fn list_access_keys(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
) -> Result<Json<Vec<AccessKey>>, AppError> {
    let keys = db::access_keys::list_access_keys(&state.db, &auth.account_id).await?;
    Ok(Json(keys))
}

#[utoipa::path(
    delete,
    path = "/account/access-keys/{access_key_id}",
    tag = "Account",
    security(("bearer" = [])),
    params(("access_key_id" = String, Path, description = "Access key ID to revoke")),
    responses(
        (status = 204, description = "Access key revoked"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Access key not found", body = ErrorResponse),
    ),
)]
/// Revoke an S3 access key by its access key ID. Only the owner can revoke it.
pub async fn delete_access_key(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
    Path(access_key_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let revoked =
        db::access_keys::delete_access_key(&state.db, &access_key_id, &auth.account_id).await?;
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
    path = "/account/wallet",
    tag = "Account",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Wallet information", body = WalletResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// Get wallet information for the authenticated account.
pub async fn get_wallet(
    State(state): State<AppState>,
    auth: AuthenticatedAccount,
) -> Result<Json<WalletResponse>, AppError> {
    let Some(ref pearl) = state.pearl else {
        return Ok(Json(WalletResponse {
            provisioned: false,
            wallet: None,
        }));
    };

    let address = pearl
        .get_address(&auth.account_id)
        .await
        .map_err(|e| AppError::Internal(format!("Pearl get_address failed: {e}")))?;

    Ok(Json(WalletResponse {
        provisioned: true,
        wallet: Some(WalletInfo { address }),
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
pub async fn debug_create_account(
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<CreateAccountResponse>), AppError> {
    let account = db::accounts::create_account(&state.db).await?;

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
