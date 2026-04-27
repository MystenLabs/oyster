//! Admin-key authentication and app extraction from requests.

use axum::{extract::FromRequestParts, http::request::Parts};

use crate::{AppId, AppState, auth, db, error::AppError};

/// Extractor that authenticates an incoming admin-API request via a Bearer admin key.
pub struct AuthenticatedApp {
    /// The app ID that owns the admin key.
    pub app_id: AppId,
    /// The admin key id used for authentication.
    pub admin_key_id: String,
}

impl FromRequestParts<AppState> for AuthenticatedApp {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth_header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Unauthorized)?;

        let raw_key = auth_header
            .strip_prefix("Bearer ")
            .ok_or(AppError::Unauthorized)?;

        let key_hash = auth::hash_api_key(raw_key);
        let admin_key = db::app_admin_keys::find_active_by_hash(&state.db, &key_hash)
            .await?
            .ok_or(AppError::Unauthorized)?;

        Ok(AuthenticatedApp {
            app_id: admin_key.app_id,
            admin_key_id: admin_key.id,
        })
    }
}
