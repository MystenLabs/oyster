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

/// Set the StoragePool ObjectID for an account. Idempotent: only populates
/// the column when it's currently NULL (lazy-creation narrowing — first
/// writer wins). Returns true if a row was updated.
pub async fn set_storage_pool_object_id(
    pool: &super::DbPool,
    account_id: &AccountId,
    object_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "UPDATE accounts SET storage_pool_object_id = ? \
         WHERE id = ? AND storage_pool_object_id IS NULL",
    ))
    .bind(object_id)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Get the StoragePool ObjectID for an account, or `None` if unset or the
/// account doesn't exist.
pub async fn get_storage_pool_object_id(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(&super::sql(
        "SELECT storage_pool_object_id FROM accounts WHERE id = ?",
    ))
    .bind(account_id)
    .fetch_optional(pool)
    .await
    .map(|opt: Option<Option<String>>| opt.flatten())
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
    async fn get_storage_pool_object_id_is_none_for_fresh_account() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let result = get_storage_pool_object_id(&pool, &account.id)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_storage_pool_object_id_is_none_for_missing_account() {
        let pool = test_pool().await;
        let result = get_storage_pool_object_id(&pool, &AccountId::new())
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn set_storage_pool_object_id_sets_when_null() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let updated = set_storage_pool_object_id(&pool, &account.id, "0xabc")
            .await
            .unwrap();
        assert!(updated);
        let fetched = get_storage_pool_object_id(&pool, &account.id)
            .await
            .unwrap();
        assert_eq!(fetched.as_deref(), Some("0xabc"));
    }

    #[tokio::test]
    async fn set_storage_pool_object_id_is_idempotent() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let first = set_storage_pool_object_id(&pool, &account.id, "0xabc")
            .await
            .unwrap();
        assert!(first);
        let second = set_storage_pool_object_id(&pool, &account.id, "0xdef")
            .await
            .unwrap();
        assert!(!second);
        let fetched = get_storage_pool_object_id(&pool, &account.id)
            .await
            .unwrap();
        assert_eq!(fetched.as_deref(), Some("0xabc"));
    }

    #[tokio::test]
    async fn set_storage_pool_object_id_noop_for_missing_account() {
        let pool = test_pool().await;
        let updated = set_storage_pool_object_id(&pool, &AccountId::new(), "0xabc")
            .await
            .unwrap();
        assert!(!updated);
    }
}
