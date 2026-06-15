use rand::RngExt;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    AccountId,
    models::{AccessKey, AccessKeyWithSecret},
};

/// Full access key record including the secret — used for SigV4 verification.
pub struct AccessKeyRecord {
    /// Owning account ID.
    pub account_id: AccountId,
    /// The raw secret access key (needed for HMAC verification).
    pub secret_access_key: String,
}

/// Generate a 20-character access key ID: "OYAK" prefix + 16 uppercase alphanumeric chars.
fn generate_access_key_id() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rng();
    let suffix: String = (0..16)
        .map(|_| CHARSET[rng.random_range(0..CHARSET.len())] as char)
        .collect();
    format!("OYAK{suffix}")
}

/// Generate a 40-character hex secret access key from 20 random bytes.
fn generate_secret_access_key() -> String {
    let mut bytes = [0u8; 20];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

/// Insert a new access key and return it with the plaintext secret.
pub async fn create_access_key(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<AccessKeyWithSecret, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let access_key_id = generate_access_key_id();
    let secret_access_key = generate_secret_access_key();

    let row = sqlx::query(&super::sql(
        "INSERT INTO access_keys (id, account_id, access_key_id, secret_access_key) VALUES (?, ?, ?, ?) RETURNING access_key_id, created_at",
    ))
    .bind(&id)
    .bind(account_id)
    .bind(&access_key_id)
    .bind(&secret_access_key)
    .fetch_one(pool)
    .await?;

    Ok(AccessKeyWithSecret {
        access_key_id: row.get("access_key_id"),
        secret_access_key,
        created_at: row.get("created_at"),
    })
}

/// Count active (non-revoked) access keys for an account.
pub async fn count_access_keys(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT COUNT(*) as cnt FROM access_keys WHERE account_id = ? AND revoked_at IS NULL",
    ))
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<i64, _>("cnt"))
}

/// List all access keys for an account (never returns secrets).
pub async fn list_access_keys(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<Vec<AccessKey>, sqlx::Error> {
    let rows = sqlx::query(&super::sql(
        "SELECT access_key_id, created_at, revoked_at FROM access_keys WHERE account_id = ? ORDER BY created_at",
    ))
    .bind(account_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| AccessKey {
            access_key_id: r.get("access_key_id"),
            created_at: r.get("created_at"),
            revoked_at: r.get("revoked_at"),
        })
        .collect())
}

/// Look up an active access key by its public key ID. Returns the full record
/// including the secret — needed for SigV4 HMAC verification.
pub async fn find_by_access_key_id(
    pool: &super::DbPool,
    access_key_id: &str,
) -> Result<Option<AccessKeyRecord>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT account_id, secret_access_key FROM access_keys WHERE access_key_id = ? AND revoked_at IS NULL",
    ))
    .bind(access_key_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| AccessKeyRecord {
        account_id: r.get("account_id"),
        secret_access_key: r.get("secret_access_key"),
    }))
}

/// Soft-revoke an access key. Returns `true` if a key was actually revoked.
pub async fn delete_access_key(
    pool: &super::DbPool,
    access_key_id: &str,
    account_id: &AccountId,
) -> Result<bool, sqlx::Error> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let result = sqlx::query(&super::sql(
        "UPDATE access_keys SET revoked_at = ? WHERE access_key_id = ? AND account_id = ? AND revoked_at IS NULL",
    ))
    .bind(&now)
    .bind(access_key_id)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use crate::db;

    #[tokio::test]
    async fn create_access_key_fields() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        let account =
            db::accounts::create_account(&pool, &crate::AppId::INTERNAL, None, None, None)
                .await
                .unwrap();

        let key = super::create_access_key(&pool, &account.id).await.unwrap();
        assert!(key.access_key_id.starts_with("OYAK"));
        assert_eq!(key.access_key_id.len(), 20);
        assert_eq!(key.secret_access_key.len(), 40);
        assert!(!key.created_at.is_empty());
    }

    #[tokio::test]
    async fn count_active_keys() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        let account =
            db::accounts::create_account(&pool, &crate::AppId::INTERNAL, None, None, None)
                .await
                .unwrap();

        assert_eq!(
            super::count_access_keys(&pool, &account.id).await.unwrap(),
            0
        );

        super::create_access_key(&pool, &account.id).await.unwrap();
        assert_eq!(
            super::count_access_keys(&pool, &account.id).await.unwrap(),
            1
        );

        super::create_access_key(&pool, &account.id).await.unwrap();
        assert_eq!(
            super::count_access_keys(&pool, &account.id).await.unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn list_excludes_secrets() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        let account =
            db::accounts::create_account(&pool, &crate::AppId::INTERNAL, None, None, None)
                .await
                .unwrap();

        let created = super::create_access_key(&pool, &account.id).await.unwrap();
        let list = super::list_access_keys(&pool, &account.id).await.unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].access_key_id, created.access_key_id);
        assert!(list[0].revoked_at.is_none());
    }

    #[tokio::test]
    async fn delete_soft_revokes() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        let account =
            db::accounts::create_account(&pool, &crate::AppId::INTERNAL, None, None, None)
                .await
                .unwrap();

        let key = super::create_access_key(&pool, &account.id).await.unwrap();
        let revoked = super::delete_access_key(&pool, &key.access_key_id, &account.id)
            .await
            .unwrap();
        assert!(revoked);

        // Double-delete returns false
        let revoked2 = super::delete_access_key(&pool, &key.access_key_id, &account.id)
            .await
            .unwrap();
        assert!(!revoked2);

        // find_by_access_key_id returns None for revoked key
        let found = super::find_by_access_key_id(&pool, &key.access_key_id)
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn revoked_keys_dont_count_toward_limit() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        let account =
            db::accounts::create_account(&pool, &crate::AppId::INTERNAL, None, None, None)
                .await
                .unwrap();

        let key = super::create_access_key(&pool, &account.id).await.unwrap();
        assert_eq!(
            super::count_access_keys(&pool, &account.id).await.unwrap(),
            1
        );

        super::delete_access_key(&pool, &key.access_key_id, &account.id)
            .await
            .unwrap();
        assert_eq!(
            super::count_access_keys(&pool, &account.id).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn find_by_access_key_id_returns_record() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        let account =
            db::accounts::create_account(&pool, &crate::AppId::INTERNAL, None, None, None)
                .await
                .unwrap();

        let key = super::create_access_key(&pool, &account.id).await.unwrap();
        let found = super::find_by_access_key_id(&pool, &key.access_key_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.account_id, account.id);
        assert_eq!(found.secret_access_key, key.secret_access_key);
    }
}
