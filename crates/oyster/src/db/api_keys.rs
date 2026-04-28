use sqlx::Row;
use uuid::Uuid;

use crate::{
    AccountId,
    models::{ApiKey, ApiKeyMetadata, ApiKeyWithBearerToken},
};

/// Insert a new API key and return it with the plaintext bearer token.
pub async fn create_api_key(
    pool: &super::DbPool,
    account_id: &AccountId,
    key_hash: &str,
    prefix: &str,
    raw_key: &str,
    note: &str,
) -> Result<ApiKeyWithBearerToken, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let row = sqlx::query(&super::sql(
        "INSERT INTO api_keys (id, account_id, key_hash, prefix, note) \
         VALUES (?, ?, ?, ?, ?) \
         RETURNING id, prefix, created_at",
    ))
    .bind(&id)
    .bind(account_id)
    .bind(key_hash)
    .bind(prefix)
    .bind(note)
    .fetch_one(pool)
    .await?;

    Ok(ApiKeyWithBearerToken {
        id: row.get("id"),
        prefix: row.get("prefix"),
        bearer_token: raw_key.to_string(),
        created_at: row.get("created_at"),
    })
}

/// Look up an active (non-revoked) API key by its hash.
pub async fn find_by_hash(
    pool: &super::DbPool,
    key_hash: &str,
) -> Result<Option<ApiKey>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT id, account_id, prefix, created_at, revoked_at FROM api_keys WHERE key_hash = ? AND revoked_at IS NULL",
    ))
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

/// Count active (non-revoked) API keys for an account.
pub async fn count_active_api_keys(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT COUNT(*) AS cnt FROM api_keys \
         WHERE account_id = ? AND revoked_at IS NULL",
    ))
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<i64, _>("cnt"))
}

/// List all API keys for an account, returning metadata only (never the bearer secret).
pub async fn list_api_keys_by_account(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<Vec<ApiKeyMetadata>, sqlx::Error> {
    let rows = sqlx::query(&super::sql(
        "SELECT id, prefix, note, created_at, revoked_at \
         FROM api_keys WHERE account_id = ? ORDER BY created_at",
    ))
    .bind(account_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ApiKeyMetadata {
            id: r.get("id"),
            prefix: r.get("prefix"),
            note: r.get("note"),
            created_at: r.get("created_at"),
            revoked_at: r.get("revoked_at"),
        })
        .collect())
}

/// Revoke an API key. Returns `true` if a key was actually revoked.
pub async fn revoke_api_key(
    pool: &super::DbPool,
    key_id: &str,
    account_id: &AccountId,
) -> Result<bool, sqlx::Error> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let result = sqlx::query(&super::sql(
        "UPDATE api_keys SET revoked_at = ? WHERE id = ? AND account_id = ? AND revoked_at IS NULL",
    ))
    .bind(&now)
    .bind(key_id)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use crate::{AppId, db};

    async fn test_pool() -> db::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    async fn make_key(
        pool: &db::DbPool,
        account_id: &crate::AccountId,
        note: &str,
    ) -> crate::models::ApiKeyWithBearerToken {
        let raw = crate::auth::generate_api_key();
        let hash = crate::auth::hash_api_key(&raw);
        let prefix = crate::auth::key_prefix(&raw);
        super::create_api_key(pool, account_id, &hash, &prefix, &raw, note)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn count_active_api_keys_zero_initially() {
        let pool = test_pool().await;
        let account = db::accounts::create_account(&pool, &AppId::INTERNAL, None)
            .await
            .unwrap();
        assert_eq!(
            super::count_active_api_keys(&pool, &account.id)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn count_active_api_keys_increments_on_create() {
        let pool = test_pool().await;
        let account = db::accounts::create_account(&pool, &AppId::INTERNAL, None)
            .await
            .unwrap();
        make_key(&pool, &account.id, "api").await;
        assert_eq!(
            super::count_active_api_keys(&pool, &account.id)
                .await
                .unwrap(),
            1
        );
        make_key(&pool, &account.id, "api").await;
        assert_eq!(
            super::count_active_api_keys(&pool, &account.id)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn count_active_api_keys_excludes_revoked() {
        let pool = test_pool().await;
        let account = db::accounts::create_account(&pool, &AppId::INTERNAL, None)
            .await
            .unwrap();
        let k = make_key(&pool, &account.id, "api").await;
        assert_eq!(
            super::count_active_api_keys(&pool, &account.id)
                .await
                .unwrap(),
            1
        );
        super::revoke_api_key(&pool, &k.id, &account.id)
            .await
            .unwrap();
        assert_eq!(
            super::count_active_api_keys(&pool, &account.id)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn list_api_keys_by_account_returns_metadata_no_secret() {
        let pool = test_pool().await;
        let account = db::accounts::create_account(&pool, &AppId::INTERNAL, None)
            .await
            .unwrap();
        let created = make_key(&pool, &account.id, "deploy").await;
        let list = super::list_api_keys_by_account(&pool, &account.id)
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, created.id);
        assert_eq!(list[0].prefix, created.prefix);
        assert_eq!(list[0].note, "deploy");
        assert!(list[0].revoked_at.is_none());
        // The metadata struct does not contain a bearer_token / key_hash field —
        // enforced by Rust's type system at compile time.
    }

    #[tokio::test]
    async fn create_api_key_persists_note() {
        let pool = test_pool().await;
        let account = db::accounts::create_account(&pool, &AppId::INTERNAL, None)
            .await
            .unwrap();
        make_key(&pool, &account.id, "ci-key").await;
        let list = super::list_api_keys_by_account(&pool, &account.id)
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].note, "ci-key");
    }
}
