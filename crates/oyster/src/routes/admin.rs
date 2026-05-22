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
        AccountSummary,
        ApiKeyMetadata,
        ApiKeyWithBearerToken,
        App,
        AppWithPublicKey,
        CreateAccountRequest,
        CreateAccountResponse,
        CreateApiKeyRequest,
        ErrorResponse,
        SetWebhookUrlRequest,
    },
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
