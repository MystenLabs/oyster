use sqlx::Row;

use crate::auth;

/// Format used to encode `expires_at` when binding to TEXT columns.
/// Matches the `datetime('now')` / `to_char(...)` defaults on the
/// `oauth_attempts` table, so lexicographic comparison against the
/// stored values is monotonic.
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

fn now_string() -> String {
    chrono::Utc::now().format(TIMESTAMP_FORMAT).to_string()
}

/// Persist one Turnstile-cleared OAuth attempt and return the raw cookie
/// token destined for the `oyster_oauth` cookie. Only the Blake2s-256
/// hash of the token is stored; the raw value exists solely in the
/// return value, mirroring how web sessions and API keys are handled.
///
/// The record — not the cookie — is the source of truth for `state`,
/// `nonce`, and the PKCE verifier. Because it is written only after the
/// anti-bot check passes, a caller cannot conjure a valid attempt by
/// hand-crafting a cookie.
pub async fn create_attempt(
    pool: &super::DbPool,
    state: &str,
    nonce: &str,
    pkce_verifier: &str,
    ttl: chrono::Duration,
) -> Result<String, sqlx::Error> {
    let raw_token = auth::generate_api_key();
    let token_hash = auth::hash_api_key(&raw_token);
    let id = uuid::Uuid::new_v4().to_string();
    let expires_at = (chrono::Utc::now() + ttl)
        .format(TIMESTAMP_FORMAT)
        .to_string();

    sqlx::query(&super::sql(
        "INSERT INTO oauth_attempts \
             (id, token_hash, state, nonce, pkce_verifier, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    ))
    .bind(&id)
    .bind(&token_hash)
    .bind(state)
    .bind(nonce)
    .bind(pkce_verifier)
    .bind(&expires_at)
    .execute(pool)
    .await?;

    Ok(raw_token)
}

/// Atomically consume a non-expired attempt by the hash of its cookie
/// token, returning the stored `(state, nonce, pkce_verifier)`, or `None`
/// when no live record matches (unknown, expired, or already used).
///
/// The single `DELETE … RETURNING` is atomic on both SQLite (≥ 3.35) and
/// PostgreSQL, which gives the single-use guarantee: a replayed cookie,
/// or two concurrent callbacks racing on the same cookie, finds the row
/// already gone. Expiry is enforced here server-side rather than trusting
/// the browser's cookie `Max-Age`.
pub async fn consume_attempt(
    pool: &super::DbPool,
    token_hash: &str,
) -> Result<Option<(String, String, String)>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "DELETE FROM oauth_attempts WHERE token_hash = ? AND expires_at > ? \
         RETURNING state, nonce, pkce_verifier",
    ))
    .bind(token_hash)
    .bind(now_string())
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| (r.get("state"), r.get("nonce"), r.get("pkce_verifier"))))
}

/// Delete expired attempts (periodic sweep). Returns the number of rows
/// removed. Hygiene only — [`consume_attempt`] already rejects expired
/// rows at lookup time. Safe with multiple replicas: concurrent deletes
/// of the same rows are harmless.
pub async fn delete_expired(pool: &super::DbPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "DELETE FROM oauth_attempts WHERE expires_at <= ?",
    ))
    .bind(now_string())
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> db::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn create_then_consume_round_trip() {
        let pool = test_pool().await;
        let raw = create_attempt(&pool, "st", "no", "pk", chrono::Duration::minutes(10))
            .await
            .unwrap();

        let secrets = consume_attempt(&pool, &auth::hash_api_key(&raw))
            .await
            .unwrap();
        assert_eq!(
            secrets,
            Some(("st".into(), "no".into(), "pk".into())),
            "consume must return the stored secrets"
        );
    }

    #[tokio::test]
    async fn consume_is_single_use() {
        let pool = test_pool().await;
        let raw = create_attempt(&pool, "st", "no", "pk", chrono::Duration::minutes(10))
            .await
            .unwrap();
        let hash = auth::hash_api_key(&raw);

        assert!(consume_attempt(&pool, &hash).await.unwrap().is_some());
        // A replay of the same cookie finds the row already consumed.
        assert!(consume_attempt(&pool, &hash).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn raw_token_is_never_stored() {
        let pool = test_pool().await;
        let raw = create_attempt(&pool, "st", "no", "pk", chrono::Duration::minutes(10))
            .await
            .unwrap();
        // Looking up by the raw token instead of its hash must miss.
        assert!(consume_attempt(&pool, &raw).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn self_made_token_is_rejected() {
        let pool = test_pool().await;
        // No create_attempt: the attacker's fabricated cookie has no record.
        let forged = auth::hash_api_key("AAA.BBB.CCC");
        assert!(consume_attempt(&pool, &forged).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_attempt_is_not_consumable() {
        let pool = test_pool().await;
        let raw = create_attempt(&pool, "st", "no", "pk", chrono::Duration::seconds(-1))
            .await
            .unwrap();
        assert!(
            consume_attempt(&pool, &auth::hash_api_key(&raw))
                .await
                .unwrap()
                .is_none(),
            "an expired attempt must not be consumable"
        );
    }

    #[tokio::test]
    async fn sweep_removes_only_expired() {
        let pool = test_pool().await;
        let live = create_attempt(&pool, "s", "n", "p", chrono::Duration::minutes(10))
            .await
            .unwrap();
        create_attempt(&pool, "s", "n", "p", chrono::Duration::seconds(-1))
            .await
            .unwrap();
        create_attempt(&pool, "s", "n", "p", chrono::Duration::seconds(-100))
            .await
            .unwrap();

        let removed = delete_expired(&pool).await.unwrap();
        assert_eq!(removed, 2);
        // The live attempt survives the sweep and is still consumable.
        assert!(
            consume_attempt(&pool, &auth::hash_api_key(&live))
                .await
                .unwrap()
                .is_some()
        );
    }
}
