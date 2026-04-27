use sqlx::Row;
use uuid::Uuid;

use crate::{
    AppId,
    models::{AdminKey, AdminKeyWithBearerToken},
};

/// Insert a new admin key and return it with the plaintext bearer token.
pub async fn create_admin_key(
    pool: &super::DbPool,
    app_id: &AppId,
    key_hash: &str,
    prefix: &str,
    raw_key: &str,
) -> Result<AdminKeyWithBearerToken, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let row = sqlx::query(&super::sql(
        "INSERT INTO app_admin_keys (id, app_id, key_hash, prefix) VALUES (?, ?, ?, ?) RETURNING id, app_id, prefix, created_at",
    ))
    .bind(&id)
    .bind(app_id)
    .bind(key_hash)
    .bind(prefix)
    .fetch_one(pool)
    .await?;

    Ok(AdminKeyWithBearerToken {
        id: row.get("id"),
        app_id: row.get("app_id"),
        prefix: row.get("prefix"),
        bearer_token: raw_key.to_string(),
        created_at: row.get("created_at"),
    })
}

/// Look up an active (non-revoked) admin key by its hash.
pub async fn find_active_by_hash(
    pool: &super::DbPool,
    key_hash: &str,
) -> Result<Option<AdminKey>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT id, app_id, prefix, created_at, revoked_at FROM app_admin_keys WHERE key_hash = ? AND revoked_at IS NULL",
    ))
    .bind(key_hash)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| AdminKey {
        id: r.get("id"),
        app_id: r.get("app_id"),
        prefix: r.get("prefix"),
        created_at: r.get("created_at"),
        revoked_at: r.get("revoked_at"),
    }))
}

/// List all admin keys for an app (including revoked).
pub async fn list_admin_keys(
    pool: &super::DbPool,
    app_id: &AppId,
) -> Result<Vec<AdminKey>, sqlx::Error> {
    let rows = sqlx::query(&super::sql(
        "SELECT id, app_id, prefix, created_at, revoked_at FROM app_admin_keys WHERE app_id = ? ORDER BY created_at",
    ))
    .bind(app_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| AdminKey {
            id: r.get("id"),
            app_id: r.get("app_id"),
            prefix: r.get("prefix"),
            created_at: r.get("created_at"),
            revoked_at: r.get("revoked_at"),
        })
        .collect())
}

/// Revoke an admin key by its id. Returns `true` if a key was actually revoked.
pub async fn revoke_admin_key(pool: &super::DbPool, key_id: &str) -> Result<bool, sqlx::Error> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let result = sqlx::query(&super::sql(
        "UPDATE app_admin_keys SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL",
    ))
    .bind(&now)
    .bind(key_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth, db};

    async fn test_pool() -> super::super::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    async fn make_app(pool: &super::super::DbPool, name: &str) -> AppId {
        db::apps::create_app(pool, name, "owner@example.com", None)
            .await
            .unwrap()
            .id
    }

    fn fresh_key() -> (String, String, String) {
        let raw = auth::generate_api_key();
        let hash = auth::hash_api_key(&raw);
        let prefix = auth::key_prefix(&raw);
        (raw, hash, prefix)
    }

    #[tokio::test]
    async fn create_and_find_round_trip() {
        let pool = test_pool().await;
        let app_id = make_app(&pool, "round-trip").await;
        let (raw, hash, prefix) = fresh_key();
        let created = create_admin_key(&pool, &app_id, &hash, &prefix, &raw)
            .await
            .unwrap();
        assert_eq!(created.app_id, app_id);
        assert_eq!(created.bearer_token, raw);
        assert_eq!(created.prefix, prefix);

        let found = find_active_by_hash(&pool, &hash).await.unwrap().unwrap();
        assert_eq!(found.id, created.id);
        assert_eq!(found.app_id, app_id);
        assert!(found.revoked_at.is_none());
    }

    #[tokio::test]
    async fn revoked_key_is_inactive() {
        let pool = test_pool().await;
        let app_id = make_app(&pool, "revoke").await;
        let (raw, hash, prefix) = fresh_key();
        let created = create_admin_key(&pool, &app_id, &hash, &prefix, &raw)
            .await
            .unwrap();

        let revoked = revoke_admin_key(&pool, &created.id).await.unwrap();
        assert!(revoked);

        let found = find_active_by_hash(&pool, &hash).await.unwrap();
        assert!(found.is_none());

        // A second revoke is a no-op.
        let revoked_again = revoke_admin_key(&pool, &created.id).await.unwrap();
        assert!(!revoked_again);
    }

    #[tokio::test]
    async fn revoke_unknown_key_is_noop() {
        let pool = test_pool().await;
        let revoked = revoke_admin_key(&pool, "no-such-id").await.unwrap();
        assert!(!revoked);
    }

    #[tokio::test]
    async fn list_returns_all_keys_ordered() {
        let pool = test_pool().await;
        let app_id = make_app(&pool, "list").await;
        let (raw1, hash1, prefix1) = fresh_key();
        let (raw2, hash2, prefix2) = fresh_key();

        let first = create_admin_key(&pool, &app_id, &hash1, &prefix1, &raw1)
            .await
            .unwrap();
        let second = create_admin_key(&pool, &app_id, &hash2, &prefix2, &raw2)
            .await
            .unwrap();

        let keys = list_admin_keys(&pool, &app_id).await.unwrap();
        assert_eq!(keys.len(), 2);
        let ids: Vec<&str> = keys.iter().map(|k| k.id.as_str()).collect();
        assert!(ids.contains(&first.id.as_str()));
        assert!(ids.contains(&second.id.as_str()));
    }

    #[tokio::test]
    async fn two_keys_per_app_both_findable() {
        let pool = test_pool().await;
        let app_id = make_app(&pool, "two-keys").await;
        let (raw1, hash1, prefix1) = fresh_key();
        let (raw2, hash2, prefix2) = fresh_key();
        let first = create_admin_key(&pool, &app_id, &hash1, &prefix1, &raw1)
            .await
            .unwrap();
        let second = create_admin_key(&pool, &app_id, &hash2, &prefix2, &raw2)
            .await
            .unwrap();

        let found1 = find_active_by_hash(&pool, &hash1).await.unwrap().unwrap();
        let found2 = find_active_by_hash(&pool, &hash2).await.unwrap().unwrap();
        assert_eq!(found1.id, first.id);
        assert_eq!(found2.id, second.id);

        // Revoke one; the other still authenticates.
        revoke_admin_key(&pool, &first.id).await.unwrap();
        assert!(find_active_by_hash(&pool, &hash1).await.unwrap().is_none());
        assert!(find_active_by_hash(&pool, &hash2).await.unwrap().is_some());
    }
}
