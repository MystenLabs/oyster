use sqlx::Row;
use uuid::Uuid;

use crate::models::Account;

/// Insert a new account, optionally linked to a Pearl wallet.
pub async fn create_account(
    pool: &super::DbPool,
    pearl_account_id: Option<&str>,
) -> Result<Account, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let row = sqlx::query(
        "INSERT INTO accounts (id, pearl_account_id) VALUES (?, ?) RETURNING id, pearl_account_id, created_at, updated_at",
    )
    .bind(&id)
    .bind(pearl_account_id)
    .fetch_one(pool)
    .await?;

    Ok(Account {
        id: row.get("id"),
        pearl_account_id: row.get("pearl_account_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

/// Fetch an account by ID, returning `None` if it does not exist.
pub async fn get_account(pool: &super::DbPool, id: &str) -> Result<Option<Account>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, pearl_account_id, created_at, updated_at FROM accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| Account {
        id: r.get("id"),
        pearl_account_id: r.get("pearl_account_id"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }))
}

/// Count the total number of accounts.
pub async fn count_accounts(pool: &super::DbPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM accounts")
        .fetch_one(pool)
        .await
}

/// Link an existing account to a Pearl wallet. Returns `true` if a row was updated.
pub async fn set_pearl_account_id(
    pool: &super::DbPool,
    account_id: &str,
    pearl_account_id: &str,
) -> Result<bool, sqlx::Error> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let result =
        sqlx::query("UPDATE accounts SET pearl_account_id = ?, updated_at = ? WHERE id = ?")
            .bind(pearl_account_id)
            .bind(&now)
            .bind(account_id)
            .execute(pool)
            .await?;

    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> super::super::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn create_account_without_pearl() {
        let pool = test_pool().await;
        let account = create_account(&pool, None).await.unwrap();
        assert!(!account.id.is_empty());
        assert!(account.pearl_account_id.is_none());
    }

    #[tokio::test]
    async fn create_account_with_pearl() {
        let pool = test_pool().await;
        let account = create_account(&pool, Some("pearl-123")).await.unwrap();
        assert!(!account.id.is_empty());
        assert_eq!(account.pearl_account_id.as_deref(), Some("pearl-123"));
    }

    #[tokio::test]
    async fn set_pearl_account_id_updates_existing() {
        let pool = test_pool().await;
        let account = create_account(&pool, None).await.unwrap();
        assert!(account.pearl_account_id.is_none());

        let updated = set_pearl_account_id(&pool, &account.id, "pearl-456")
            .await
            .unwrap();
        assert!(updated);

        let fetched = get_account(&pool, &account.id).await.unwrap().unwrap();
        assert_eq!(fetched.pearl_account_id.as_deref(), Some("pearl-456"));
    }

    #[tokio::test]
    async fn set_pearl_account_id_nonexistent_account() {
        let pool = test_pool().await;
        let updated = set_pearl_account_id(&pool, "no-such-id", "pearl-789")
            .await
            .unwrap();
        assert!(!updated);
    }
}
