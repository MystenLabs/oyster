//! Admin API routes authenticated via per-app admin keys (AuthenticatedApp).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use crate::{
    AccountId,
    AppId,
    AppState,
    app_admin::AuthenticatedApp,
    auth,
    db,
    error::AppError,
    models::{
        AccessKey,
        AccessKeyWithSecret,
        ApiKeyWithBearerToken,
        CreateAccountRequest,
        CreateAccountResponse,
        ErrorResponse,
    },
};

/// Maximum number of active access keys per account.
const MAX_ACCESS_KEYS: i64 = 3;

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
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
)]
/// Create a new account owned by the authenticated app.
pub async fn create_account(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    body: Option<Json<CreateAccountRequest>>,
) -> Result<(StatusCode, Json<CreateAccountResponse>), AppError> {
    let name = body.and_then(|b| b.0.name);
    let account = db::accounts::create_account(&state.db, &auth.app_id, name.as_deref()).await?;

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

#[utoipa::path(
    post,
    path = "/accounts/{account_id}/api-keys",
    tag = "Admin",
    security(("bearer" = [])),
    params(("account_id" = AccountId, Path, description = "Account ID")),
    responses(
        (status = 201, description = "API key created", body = ApiKeyWithBearerToken),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Account not found", body = ErrorResponse),
    ),
)]
/// Create a new API key for an account owned by the authenticated app.
pub async fn admin_create_api_key(
    State(state): State<AppState>,
    auth: AuthenticatedApp,
    Path(account_id): Path<AccountId>,
) -> Result<(StatusCode, Json<ApiKeyWithBearerToken>), AppError> {
    verify_account_ownership(&state.db, &account_id, &auth.app_id).await?;

    let raw_key = auth::generate_api_key();
    let key_hash = auth::hash_api_key(&raw_key);
    let prefix = auth::key_prefix(&raw_key);

    let api_key =
        db::api_keys::create_api_key(&state.db, &account_id, &key_hash, &prefix, &raw_key).await?;

    Ok((StatusCode::CREATED, Json(api_key)))
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
