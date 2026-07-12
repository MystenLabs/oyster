use sqlx::Row;

use crate::{UserId, auth};

/// Format used to encode `expires_at` when binding to TEXT columns.
/// Matches the `datetime('now')` / `to_char(...)` defaults on the
/// `web_sessions` table — same width and ordering, so lexicographic
/// comparison against the stored values is monotonic.
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

fn now_string() -> String {
    chrono::Utc::now().format(TIMESTAMP_FORMAT).to_string()
}

/// Create a browser session for a user and return the raw session
/// token destined for the cookie. Only the Blake2s-256 hash of the
/// token is stored; the raw value exists solely in the return value,
/// mirroring how API keys are handled.
pub async fn create_session(
    pool: &super::DbPool,
    user_id: &UserId,
    ttl: chrono::Duration,
) -> Result<String, sqlx::Error> {
    let raw_token = auth::generate_api_key();
    let token_hash = auth::hash_api_key(&raw_token);
    let id = uuid::Uuid::new_v4().to_string();
    let expires_at = (chrono::Utc::now() + ttl)
        .format(TIMESTAMP_FORMAT)
        .to_string();

    sqlx::query(&super::sql(
        "INSERT INTO web_sessions (id, user_id, token_hash, expires_at) VALUES (?, ?, ?, ?)",
    ))
    .bind(&id)
    .bind(user_id)
    .bind(&token_hash)
    .bind(&expires_at)
    .execute(pool)
    .await?;

    Ok(raw_token)
}

/// Resolve an unexpired session by the hash of its cookie token,
/// returning the owning user's ID, or `None` when the session is
/// unknown or expired.
pub async fn find_active_by_hash(
    pool: &super::DbPool,
    token_hash: &str,
) -> Result<Option<UserId>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT user_id FROM web_sessions WHERE token_hash = ? AND expires_at > ?",
    ))
    .bind(token_hash)
    .bind(now_string())
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| r.get("user_id")))
}

/// Delete a session by the hash of its cookie token (logout). Returns
/// `true` if a session was actually deleted.
pub async fn delete_by_hash(pool: &super::DbPool, token_hash: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql("DELETE FROM web_sessions WHERE token_hash = ?"))
        .bind(token_hash)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Delete all expired sessions (periodic sweep). Returns the number of
/// rows removed. Safe to run concurrently from multiple replicas —
/// concurrent deletes of the same rows are harmless.
pub async fn delete_expired(pool: &super::DbPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "DELETE FROM web_sessions WHERE expires_at <= ?",
    ))
    .bind(now_string())
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Delete a single user's expired sessions — opportunistic cleanup run
/// when that user signs in again, keeping the table tidy even if the
/// periodic sweep is not running.
pub async fn delete_expired_for_user(
    pool: &super::DbPool,
    user_id: &UserId,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "DELETE FROM web_sessions WHERE user_id = ? AND expires_at <= ?",
    ))
    .bind(user_id)
    .bind(now_string())
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, users::IdentityProvider};

    async fn test_pool() -> db::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    async fn make_user(pool: &db::DbPool, sub: &str) -> UserId {
        db::users::create_user_with_identity(
            pool,
            IdentityProvider::Google,
            sub,
            &format!("{sub}@example.com"),
            None,
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn create_and_find_round_trip() {
        let pool = test_pool().await;
        let user_id = make_user(&pool, "sess-1").await;

        let raw = create_session(&pool, &user_id, chrono::Duration::hours(8))
            .await
            .unwrap();
        let found = find_active_by_hash(&pool, &auth::hash_api_key(&raw))
            .await
            .unwrap();
        assert_eq!(found, Some(user_id));
    }

    #[tokio::test]
    async fn raw_token_is_never_stored() {
        let pool = test_pool().await;
        let user_id = make_user(&pool, "sess-raw").await;

        let raw = create_session(&pool, &user_id, chrono::Duration::hours(8))
            .await
            .unwrap();
        // Looking up by the raw token (instead of its hash) must miss.
        let found = find_active_by_hash(&pool, &raw).await.unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn expired_session_is_not_found() {
        let pool = test_pool().await;
        let user_id = make_user(&pool, "sess-2").await;

        let raw = create_session(&pool, &user_id, chrono::Duration::seconds(-1))
            .await
            .unwrap();
        let found = find_active_by_hash(&pool, &auth::hash_api_key(&raw))
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn logout_deletes_session() {
        let pool = test_pool().await;
        let user_id = make_user(&pool, "sess-3").await;

        let raw = create_session(&pool, &user_id, chrono::Duration::hours(8))
            .await
            .unwrap();
        let hash = auth::hash_api_key(&raw);

        assert!(delete_by_hash(&pool, &hash).await.unwrap());
        assert!(find_active_by_hash(&pool, &hash).await.unwrap().is_none());
        // Second delete is a no-op.
        assert!(!delete_by_hash(&pool, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn sweep_removes_only_expired() {
        let pool = test_pool().await;
        let user_id = make_user(&pool, "sess-4").await;

        let live = create_session(&pool, &user_id, chrono::Duration::hours(8))
            .await
            .unwrap();
        create_session(&pool, &user_id, chrono::Duration::seconds(-1))
            .await
            .unwrap();
        create_session(&pool, &user_id, chrono::Duration::seconds(-100))
            .await
            .unwrap();

        let removed = delete_expired(&pool).await.unwrap();
        assert_eq!(removed, 2);
        let found = find_active_by_hash(&pool, &auth::hash_api_key(&live))
            .await
            .unwrap();
        assert_eq!(found, Some(user_id));
    }

    #[tokio::test]
    async fn per_user_cleanup_is_scoped() {
        let pool = test_pool().await;
        let alice = make_user(&pool, "sess-alice").await;
        let bob = make_user(&pool, "sess-bob").await;

        create_session(&pool, &alice, chrono::Duration::seconds(-1))
            .await
            .unwrap();
        let bob_expired = create_session(&pool, &bob, chrono::Duration::seconds(-1))
            .await
            .unwrap();

        let removed = delete_expired_for_user(&pool, &alice).await.unwrap();
        assert_eq!(removed, 1);

        // Bob's expired session is untouched by Alice's cleanup (it no
        // longer authenticates, but the row still exists for the sweep).
        let bob_removed = delete_expired_for_user(&pool, &bob).await.unwrap();
        assert_eq!(bob_removed, 1);
        let _ = bob_expired;
    }
}
