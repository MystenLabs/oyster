use axum::{extract::FromRequestParts, http::request::Parts};
use blake2::{Blake2s256, Digest};
use rand::RngExt;

use crate::{AccountId, AppState, db, error::AppError};

const API_KEY_BYTES: usize = 32;

/// Generate a random 32-byte hex-encoded API key.
pub fn generate_api_key() -> String {
    let mut bytes = [0u8; API_KEY_BYTES];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

/// Compute the BLAKE2s-256 hash of a raw API key (hex-encoded).
pub fn hash_api_key(raw_key: &str) -> String {
    let mut hasher = Blake2s256::new();
    hasher.update(raw_key.as_bytes());
    hex::encode(hasher.finalize())
}

/// Return the first 8 characters of a raw API key as a prefix for identification.
pub fn key_prefix(raw_key: &str) -> String {
    raw_key[..8].to_string()
}

/// Extractor that authenticates an incoming request via a Bearer API key.
pub struct AuthenticatedAccount {
    /// The account ID associated with the valid API key.
    pub account_id: AccountId,
}

impl FromRequestParts<AppState> for AuthenticatedAccount {
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

        let key_hash = hash_api_key(raw_key);
        let api_key = db::api_keys::find_by_hash(&state.db, &key_hash)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?
            .ok_or(AppError::Unauthorized)?;

        Ok(AuthenticatedAccount {
            account_id: api_key.account_id,
        })
    }
}
