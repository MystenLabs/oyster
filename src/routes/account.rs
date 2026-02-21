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
    models::{ApiKeyWithSecret, CreateAccountResponse},
};

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

pub async fn update_billing() -> Result<StatusCode, AppError> {
    Err(AppError::NotImplemented)
}

pub async fn get_report() -> Result<StatusCode, AppError> {
    Err(AppError::NotImplemented)
}

pub async fn transfer() -> Result<StatusCode, AppError> {
    Err(AppError::NotImplemented)
}

// Debug endpoint

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
