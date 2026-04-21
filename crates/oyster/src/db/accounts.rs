use sqlx::Row;

use crate::{AccountId, AppId, models::Account};

/// Insert a new account belonging to the given app.
pub async fn create_account(
    pool: &super::DbPool,
    app_id: &AppId,
    name: Option<&str>,
) -> Result<Account, sqlx::Error> {
    let id = AccountId::new();
    let name = name.map_or_else(|| id.to_string(), |n| n.to_string());
    let row = sqlx::query(&super::sql(
        "INSERT INTO accounts (id, app_id, name) VALUES (?, ?, ?) RETURNING id, app_id, name, created_at, updated_at",
    ))
    .bind(&id)
    .bind(app_id)
    .bind(&name)
    .fetch_one(pool)
    .await?;

    Ok(Account {
        id: row.get("id"),
        app_id: row.get("app_id"),
        name: row.get("name"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

/// Fetch an account by ID, returning `None` if it does not exist.
pub async fn get_account(
    pool: &super::DbPool,
    id: &AccountId,
) -> Result<Option<Account>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT id, app_id, name, created_at, updated_at FROM accounts WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| Account {
        id: r.get("id"),
        app_id: r.get("app_id"),
        name: r.get("name"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }))
}

/// Count the total number of accounts.
pub async fn count_accounts(pool: &super::DbPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&super::sql("SELECT COUNT(*) FROM accounts"))
        .fetch_one(pool)
        .await
}

/// Persistent per-account `StoragePool` accounting state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePoolState {
    /// On-chain Sui `ObjectID` of the pool.
    pub object_id: String,
    /// Epoch at which the current reservation expires.
    pub end_epoch: i64,
    /// Total encoded bytes reserved on the pool.
    pub reserved_encoded_bytes: i64,
    /// Encoded bytes currently consumed by registered blobs.
    pub used_encoded_bytes: i64,
}

/// Persist a freshly-created `StoragePool` for an account. Idempotent: only
/// populates the columns when the pool is currently `NULL` (first writer
/// wins); returns `true` on first-writer win, `false` on race loss.
pub async fn set_storage_pool(
    pool: &super::DbPool,
    account_id: &AccountId,
    object_id: &str,
    end_epoch: i64,
    reserved_encoded_bytes: i64,
    used_encoded_bytes: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "UPDATE accounts SET \
             storage_pool_object_id = ?, \
             pool_end_epoch = ?, \
             pool_reserved_encoded_bytes = ?, \
             pool_used_encoded_bytes = ? \
         WHERE id = ? AND storage_pool_object_id IS NULL",
    ))
    .bind(object_id)
    .bind(end_epoch)
    .bind(reserved_encoded_bytes)
    .bind(used_encoded_bytes)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Fetch the current `StoragePool` state for an account, or `None` if the
/// account hasn't lazy-created a pool yet (or doesn't exist).
pub async fn get_storage_pool(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<Option<StoragePoolState>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT storage_pool_object_id, pool_end_epoch, \
                pool_reserved_encoded_bytes, pool_used_encoded_bytes \
         FROM accounts WHERE id = ?",
    ))
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let object_id: Option<String> = row.get("storage_pool_object_id");
    let end_epoch: Option<i64> = row.get("pool_end_epoch");
    let reserved: Option<i64> = row.get("pool_reserved_encoded_bytes");
    let used: Option<i64> = row.get("pool_used_encoded_bytes");
    match (object_id, end_epoch, reserved, used) {
        (Some(object_id), Some(end_epoch), Some(reserved), Some(used)) => {
            Ok(Some(StoragePoolState {
                object_id,
                end_epoch,
                reserved_encoded_bytes: reserved,
                used_encoded_bytes: used,
            }))
        }
        _ => Ok(None),
    }
}

/// Bump reserved + used byte counters after a successful `register_pooled_blob`
/// transaction. `added_reserved_bytes` may be zero when the existing
/// reservation already covered the new blob.
pub async fn update_pool_after_register(
    pool: &super::DbPool,
    account_id: &AccountId,
    added_reserved_bytes: i64,
    added_used_bytes: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "UPDATE accounts SET \
             pool_reserved_encoded_bytes = pool_reserved_encoded_bytes + ?, \
             pool_used_encoded_bytes = pool_used_encoded_bytes + ? \
         WHERE id = ?",
    ))
    .bind(added_reserved_bytes)
    .bind(added_used_bytes)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Decrement the used-byte counter after a successful `delete_pooled_blob`.
pub async fn update_pool_after_delete(
    pool: &super::DbPool,
    account_id: &AccountId,
    freed_used_bytes: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "UPDATE accounts SET \
             pool_used_encoded_bytes = pool_used_encoded_bytes - ? \
         WHERE id = ?",
    ))
    .bind(freed_used_bytes)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> super::super::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn create_account_works() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        assert_eq!(account.name, account.id.to_string());
        assert_eq!(account.app_id, AppId::INTERNAL);
    }

    #[tokio::test]
    async fn create_account_with_name() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, Some("my-account"))
            .await
            .unwrap();
        assert_eq!(account.name, "my-account");
        assert_eq!(account.app_id, AppId::INTERNAL);
    }

    #[tokio::test]
    async fn get_account_returns_created() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let fetched = get_account(&pool, &account.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, account.id);
        assert_eq!(fetched.app_id, AppId::INTERNAL);
    }

    #[tokio::test]
    async fn get_account_returns_none_for_missing() {
        let pool = test_pool().await;
        let result = get_account(&pool, &AccountId::new()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_storage_pool_is_none_for_fresh_account() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let result = get_storage_pool(&pool, &account.id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_storage_pool_is_none_for_missing_account() {
        let pool = test_pool().await;
        let result = get_storage_pool(&pool, &AccountId::new()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn set_storage_pool_sets_full_state() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let updated = set_storage_pool(&pool, &account.id, "0xabc", 42, 1_000, 0)
            .await
            .unwrap();
        assert!(updated);
        let fetched = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(fetched.object_id, "0xabc");
        assert_eq!(fetched.end_epoch, 42);
        assert_eq!(fetched.reserved_encoded_bytes, 1_000);
        assert_eq!(fetched.used_encoded_bytes, 0);
    }

    #[tokio::test]
    async fn set_storage_pool_is_idempotent() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let first = set_storage_pool(&pool, &account.id, "0xabc", 42, 1_000, 0)
            .await
            .unwrap();
        assert!(first);
        let second = set_storage_pool(&pool, &account.id, "0xdef", 99, 5_000, 50)
            .await
            .unwrap();
        assert!(!second);
        let fetched = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(fetched.object_id, "0xabc");
        assert_eq!(fetched.end_epoch, 42);
        assert_eq!(fetched.reserved_encoded_bytes, 1_000);
        assert_eq!(fetched.used_encoded_bytes, 0);
    }

    #[tokio::test]
    async fn set_storage_pool_noop_for_missing_account() {
        let pool = test_pool().await;
        let updated = set_storage_pool(&pool, &AccountId::new(), "0xabc", 42, 1_000, 0)
            .await
            .unwrap();
        assert!(!updated);
    }

    #[tokio::test]
    async fn update_pool_after_register_bumps_both() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        set_storage_pool(&pool, &account.id, "0xabc", 42, 1_000, 0)
            .await
            .unwrap();
        update_pool_after_register(&pool, &account.id, 500, 300)
            .await
            .unwrap();
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.reserved_encoded_bytes, 1_500);
        assert_eq!(state.used_encoded_bytes, 300);
    }

    #[tokio::test]
    async fn update_pool_after_delete_decrements_used() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        set_storage_pool(&pool, &account.id, "0xabc", 42, 1_000, 400)
            .await
            .unwrap();
        update_pool_after_delete(&pool, &account.id, 250)
            .await
            .unwrap();
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.reserved_encoded_bytes, 1_000);
        assert_eq!(state.used_encoded_bytes, 150);
    }
}
