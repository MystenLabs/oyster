//! JWT signing, verification, and axum extractor for app authentication.

use axum::{extract::FromRequestParts, http::request::Parts};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppId, AppState, db, error::AppError};

/// JWT token lifetime: 24 hours.
const JWT_EXPIRY_SECS: i64 = 86400;

/// Grace window for token refresh: 48 hours past expiry.
pub const JWT_GRACE_SECS: i64 = 172800;

/// JWT issuer claim value.
const JWT_ISSUER: &str = "oyster";

/// JWT claims for app authentication.
#[derive(Debug, Serialize, Deserialize)]
pub struct AppClaims {
    /// Subject: the app ID (UUID string).
    pub sub: String,
    /// Issued-at (Unix timestamp).
    pub iat: i64,
    /// Expiration (Unix timestamp).
    pub exp: i64,
    /// Issuer (always "oyster").
    pub iss: String,
    /// Unique token identifier (UUID, for blacklisting).
    pub jti: String,
}

/// Sign a JWT for the given app.
pub fn sign_jwt(app_id: &AppId, secret: &str) -> Result<String, jsonwebtoken::errors::Error> {
    let now = jsonwebtoken::get_current_timestamp() as i64;
    let claims = AppClaims {
        sub: app_id.to_string(),
        iat: now,
        exp: now + JWT_EXPIRY_SECS,
        iss: JWT_ISSUER.into(),
        jti: Uuid::new_v4().to_string(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

/// Build a [`Validation`] configured for Oyster JWTs.
fn build_validation(leeway: u64) -> Validation {
    let mut v = Validation::new(Algorithm::HS256);
    v.set_issuer(&[JWT_ISSUER]);
    v.set_required_spec_claims(&["exp", "iss", "sub", "jti"]);
    v.validate_aud = false;
    v.leeway = leeway;
    v
}

/// Verify a JWT token, checking signature, expiry, issuer, and blacklist.
pub async fn verify_jwt(
    token: &str,
    secret: &str,
    pool: &db::DbPool,
) -> Result<AppClaims, AppError> {
    let validation = build_validation(0);
    let data = decode::<AppClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AppError::Unauthorized)?;

    if db::jwt_blacklist::is_blacklisted(pool, &data.claims.jti).await? {
        return Err(AppError::Unauthorized);
    }

    Ok(data.claims)
}

/// Verify a JWT with a grace window for token refresh.
///
/// Accepts tokens up to `grace_seconds` past expiry.
pub async fn verify_jwt_with_grace(
    token: &str,
    secret: &str,
    pool: &db::DbPool,
    grace_seconds: u64,
) -> Result<AppClaims, AppError> {
    let validation = build_validation(grace_seconds);
    let data = decode::<AppClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AppError::Unauthorized)?;

    if db::jwt_blacklist::is_blacklisted(pool, &data.claims.jti).await? {
        return Err(AppError::Unauthorized);
    }

    Ok(data.claims)
}

/// Axum extractor: authenticates an app via JWT in the Authorization header.
pub struct AuthenticatedApp {
    /// The authenticated app ID.
    pub app_id: AppId,
    /// The decoded JWT claims.
    pub claims: AppClaims,
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

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or(AppError::Unauthorized)?;

        // JWTs have exactly two '.' separators; hex API keys do not.
        if token.chars().filter(|&c| c == '.').count() != 2 {
            return Err(AppError::Unauthorized);
        }

        let secret = state
            .config
            .jwt_secret
            .as_deref()
            .ok_or(AppError::Unauthorized)?;

        let claims = verify_jwt(token, secret, &state.db).await?;

        let app_id: AppId = claims.sub.parse().map_err(|_| AppError::Unauthorized)?;

        // Verify the app exists.
        db::apps::get_app(&state.db, &app_id)
            .await?
            .ok_or(AppError::Unauthorized)?;

        Ok(AuthenticatedApp { app_id, claims })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "test-jwt-secret";

    async fn test_pool() -> db::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    fn sign_with_claims(claims: &AppClaims, secret: &str) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn sign_and_verify_roundtrip() {
        let pool = test_pool().await;
        let app_id = AppId::new();
        let token = sign_jwt(&app_id, TEST_SECRET).unwrap();
        let claims = verify_jwt(&token, TEST_SECRET, &pool).await.unwrap();
        assert_eq!(claims.sub, app_id.to_string());
        assert_eq!(claims.iss, JWT_ISSUER);
    }

    #[tokio::test]
    async fn expired_jwt_rejected() {
        let pool = test_pool().await;
        let claims = AppClaims {
            sub: AppId::new().to_string(),
            iat: 1000,
            exp: 1001,
            iss: JWT_ISSUER.into(),
            jti: Uuid::new_v4().to_string(),
        };
        let token = sign_with_claims(&claims, TEST_SECRET);
        let result = verify_jwt(&token, TEST_SECRET, &pool).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn bad_secret_rejected() {
        let pool = test_pool().await;
        let app_id = AppId::new();
        let token = sign_jwt(&app_id, TEST_SECRET).unwrap();
        let result = verify_jwt(&token, "wrong-secret", &pool).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn bad_issuer_rejected() {
        let pool = test_pool().await;
        let now = jsonwebtoken::get_current_timestamp() as i64;
        let claims = AppClaims {
            sub: AppId::new().to_string(),
            iat: now,
            exp: now + JWT_EXPIRY_SECS,
            iss: "not-oyster".into(),
            jti: Uuid::new_v4().to_string(),
        };
        let token = sign_with_claims(&claims, TEST_SECRET);
        let result = verify_jwt(&token, TEST_SECRET, &pool).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn blacklisted_jti_rejected() {
        let pool = test_pool().await;
        let app_id = AppId::new();
        let token = sign_jwt(&app_id, TEST_SECRET).unwrap();

        // Decode to get jti, then blacklist it.
        let claims = verify_jwt(&token, TEST_SECRET, &pool).await.unwrap();
        db::jwt_blacklist::blacklist_jti(&pool, &claims.jti, "2099-01-01 00:00:00")
            .await
            .unwrap();

        let result = verify_jwt(&token, TEST_SECRET, &pool).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn grace_window_accepts_recently_expired() {
        let pool = test_pool().await;
        let now = jsonwebtoken::get_current_timestamp() as i64;
        // Expired 1 hour ago.
        let claims = AppClaims {
            sub: AppId::new().to_string(),
            iat: now - 7200,
            exp: now - 3600,
            iss: JWT_ISSUER.into(),
            jti: Uuid::new_v4().to_string(),
        };
        let token = sign_with_claims(&claims, TEST_SECRET);
        let result = verify_jwt_with_grace(&token, TEST_SECRET, &pool, JWT_GRACE_SECS as u64).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn grace_window_rejects_long_expired() {
        let pool = test_pool().await;
        let now = jsonwebtoken::get_current_timestamp() as i64;
        // Expired 49 hours ago — outside the 48h grace window.
        let claims = AppClaims {
            sub: AppId::new().to_string(),
            iat: now - 200_000,
            exp: now - (49 * 3600),
            iss: JWT_ISSUER.into(),
            jti: Uuid::new_v4().to_string(),
        };
        let token = sign_with_claims(&claims, TEST_SECRET);
        let result = verify_jwt_with_grace(&token, TEST_SECRET, &pool, JWT_GRACE_SECS as u64).await;
        assert!(result.is_err());
    }
}
