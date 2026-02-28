use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::models::{ApiKey, ApiKeyWithSecret};

/// Insert a new API key and return it with the plaintext secret.
pub async fn create_api_key(
    pool: &SqlitePool,
    account_id: &str,
    key_hash: &str,
    prefix: &str,
    raw_key: &str,
) -> Result<ApiKeyWithSecret, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let row = sqlx::query(
        "INSERT INTO api_keys (id, account_id, key_hash, prefix) VALUES (?, ?, ?, ?) RETURNING id, prefix, created_at",
    )
    .bind(&id)
    .bind(account_id)
    .bind(key_hash)
    .bind(prefix)
    .fetch_one(pool)
    .await?;

    Ok(ApiKeyWithSecret {
        id: row.get("id"),
        prefix: row.get("prefix"),
        secret: raw_key.to_string(),
        created_at: row.get("created_at"),
    })
}

/// Look up an active (non-revoked) API key by its hash.
pub async fn find_by_hash(
    pool: &SqlitePool,
    key_hash: &str,
) -> Result<Option<ApiKey>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, account_id, prefix, created_at, revoked_at FROM api_keys WHERE key_hash = ? AND revoked_at IS NULL",
    )
    .bind(key_hash)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| ApiKey {
        id: r.get("id"),
        account_id: r.get("account_id"),
        prefix: r.get("prefix"),
        created_at: r.get("created_at"),
        revoked_at: r.get("revoked_at"),
    }))
}

/// Revoke an API key. Returns `true` if a key was actually revoked.
pub async fn revoke_api_key(
    pool: &SqlitePool,
    key_id: &str,
    account_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE api_keys SET revoked_at = datetime('now') WHERE id = ? AND account_id = ? AND revoked_at IS NULL",
    )
    .bind(key_id)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
