//! Admin-key authentication, app extraction from requests, and admin-key
//! issuance shared by the CLI and the web signup flow.

use std::fmt;

use axum::{extract::FromRequestParts, http::request::Parts};

use crate::{AppId, AppState, auth, db, error::AppError, models::AdminKeyWithBearerToken};

/// Error from [`issue_admin_key`].
#[derive(Debug)]
pub enum IssueAdminKeyError {
    /// The app already has the maximum number of active admin keys.
    LimitReached(i64),
    /// Underlying database error.
    Db(sqlx::Error),
}

impl fmt::Display for IssueAdminKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitReached(limit) => write!(
                f,
                "admin key limit reached ({limit}); revoke an unused key first"
            ),
            Self::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for IssueAdminKeyError {}

impl From<sqlx::Error> for IssueAdminKeyError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

impl From<IssueAdminKeyError> for AppError {
    fn from(e: IssueAdminKeyError) -> Self {
        match e {
            IssueAdminKeyError::LimitReached(_) => AppError::Conflict(e.to_string()),
            IssueAdminKeyError::Db(e) => e.into(),
        }
    }
}

/// Issue a new admin key for an app, returning the record with the raw
/// bearer token (which is never stored — only its hash is persisted).
///
/// `max_active` caps the number of active (non-revoked) keys the app
/// may hold: the self-serve web path passes
/// `Config::max_admin_keys_per_app`, while the `oyster app` CLI passes
/// `None` as an operator escape hatch. The count-then-insert is not
/// atomic; two perfectly concurrent issuances can land one key over
/// the cap, which is acceptable for an abuse guardrail.
pub async fn issue_admin_key(
    pool: &db::DbPool,
    app_id: &AppId,
    max_active: Option<i64>,
) -> Result<AdminKeyWithBearerToken, IssueAdminKeyError> {
    if let Some(limit) = max_active {
        let count = db::app_admin_keys::count_active_admin_keys(pool, app_id).await?;
        if count >= limit {
            return Err(IssueAdminKeyError::LimitReached(limit));
        }
    }

    let raw = auth::generate_api_key();
    let hash = auth::hash_api_key(&raw);
    let prefix = auth::key_prefix(&raw);
    let key = db::app_admin_keys::create_admin_key(pool, app_id, &hash, &prefix, &raw).await?;
    tracing::info!(app_id = %app_id, key_id = %key.id, prefix = %key.prefix, "issued admin key");
    Ok(key)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> db::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    async fn make_app(pool: &db::DbPool) -> AppId {
        db::apps::create_app(pool, "cap-test", "owner@example.com")
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn cap_rejects_issuance_at_limit() {
        let pool = test_pool().await;
        let app_id = make_app(&pool).await;

        for _ in 0..2 {
            issue_admin_key(&pool, &app_id, Some(2)).await.unwrap();
        }
        let err = issue_admin_key(&pool, &app_id, Some(2)).await.unwrap_err();
        assert!(matches!(err, IssueAdminKeyError::LimitReached(2)));

        // The error maps to HTTP 409 for the web path.
        assert!(matches!(AppError::from(err), AppError::Conflict(_)));
    }

    #[tokio::test]
    async fn revoked_keys_do_not_count_toward_cap() {
        let pool = test_pool().await;
        let app_id = make_app(&pool).await;

        let first = issue_admin_key(&pool, &app_id, Some(1)).await.unwrap();
        db::app_admin_keys::revoke_admin_key(&pool, &first.id)
            .await
            .unwrap();

        // Rotation is never blocked by the cap.
        issue_admin_key(&pool, &app_id, Some(1)).await.unwrap();
    }

    #[tokio::test]
    async fn no_cap_bypasses_limit() {
        let pool = test_pool().await;
        let app_id = make_app(&pool).await;

        for _ in 0..7 {
            issue_admin_key(&pool, &app_id, None).await.unwrap();
        }
        let count = db::app_admin_keys::count_active_admin_keys(&pool, &app_id)
            .await
            .unwrap();
        assert_eq!(count, 7);
    }

    #[tokio::test]
    async fn issued_key_authenticates_by_hash() {
        let pool = test_pool().await;
        let app_id = make_app(&pool).await;

        let key = issue_admin_key(&pool, &app_id, Some(5)).await.unwrap();
        let found =
            db::app_admin_keys::find_active_by_hash(&pool, &auth::hash_api_key(&key.bearer_token))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(found.app_id, app_id);
        assert_eq!(found.id, key.id);
    }
}
