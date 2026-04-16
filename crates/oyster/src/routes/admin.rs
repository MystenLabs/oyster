//! Admin API routes authenticated via JWT (AuthenticatedApp).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use crate::{
    AccountId,
    AppId,
    AppState,
    app_auth::{self, AuthenticatedApp, JWT_GRACE_SECS},
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
        TokenRefreshResponse,
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

#[utoipa::path(
    post,
    path = "/apps/token-refresh",
    tag = "Admin",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Token refreshed", body = TokenRefreshResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Refresh not allowed", body = ErrorResponse),
    ),
)]
/// Refresh a JWT token. Accepts tokens up to 48h past expiry.
pub async fn token_refresh(
    State(state): State<AppState>,
    req: axum::http::request::Parts,
) -> Result<Json<TokenRefreshResponse>, AppError> {
    let auth_header = req
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            tracing::warn!("token refresh: missing or non-ASCII authorization header");
            AppError::Unauthorized
        })?;

    let token = auth_header.strip_prefix("Bearer ").ok_or_else(|| {
        tracing::warn!("token refresh: authorization header missing 'Bearer ' prefix");
        AppError::Unauthorized
    })?;

    let secret = state.config.jwt_secret.as_deref().ok_or_else(|| {
        tracing::error!("token refresh: OYSTER_JWT_SECRET is not configured");
        AppError::Unauthorized
    })?;

    let claims =
        app_auth::verify_jwt_with_grace(token, secret, &state.db, JWT_GRACE_SECS as u64).await?;

    let now = jsonwebtoken::get_current_timestamp() as i64;
    let expired_ago = now - claims.exp;
    tracing::info!(
        app_id = %claims.sub,
        jti = %claims.jti,
        exp = claims.exp,
        expired_ago_secs = expired_ago,
        "token refresh: JWT verified"
    );

    let app_id: AppId = claims.sub.parse().map_err(|_| {
        tracing::warn!(sub = %claims.sub, "token refresh: invalid app_id in sub claim");
        AppError::Unauthorized
    })?;

    let app = db::apps::get_app(&state.db, &app_id)
        .await?
        .ok_or_else(|| {
            tracing::warn!(%app_id, "token refresh: app not found");
            AppError::Unauthorized
        })?;

    if !app.allow_refresh_jwt {
        tracing::warn!(%app_id, "token refresh: refresh not allowed for app");
        return Err(AppError::Forbidden(
            "token refresh is not allowed for this app".into(),
        ));
    }

    // Blacklist the old JTI.
    let not_after = chrono::DateTime::from_timestamp(claims.exp + JWT_GRACE_SECS, 0)
        .ok_or_else(|| AppError::Internal("invalid expiry timestamp".into()))?
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    db::jwt_blacklist::blacklist_jti(&state.db, &claims.jti, &not_after).await?;

    let access_token =
        app_auth::sign_jwt(&app_id, secret).map_err(|e| AppError::Internal(e.to_string()))?;

    tracing::info!(
        %app_id,
        old_jti = %claims.jti,
        "token refresh: issued new token, old JTI blacklisted"
    );

    Ok(Json(TokenRefreshResponse {
        access_token,
        token_type: "Bearer".into(),
        expires_in: 86400,
    }))
}
