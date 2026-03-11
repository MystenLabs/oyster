use sqlx::Row;
use uuid::Uuid;

use crate::models::Account;

/// Insert a new account.
pub async fn create_account(pool: &super::DbPool) -> Result<Account, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let row =
        sqlx::query("INSERT INTO accounts (id) VALUES (?) RETURNING id, created_at, updated_at")
            .bind(&id)
            .fetch_one(pool)
            .await?;

    Ok(Account {
        id: row.get("id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

/// Fetch an account by ID, returning `None` if it does not exist.
pub async fn get_account(pool: &super::DbPool, id: &str) -> Result<Option<Account>, sqlx::Error> {
    let row = sqlx::query("SELECT id, created_at, updated_at FROM accounts WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(|r| Account {
        id: r.get("id"),
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
        let account = create_account(&pool).await.unwrap();
        assert!(!account.id.is_empty());
    }

    #[tokio::test]
    async fn get_account_returns_created() {
        let pool = test_pool().await;
        let account = create_account(&pool).await.unwrap();
        let fetched = get_account(&pool, &account.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, account.id);
    }

    #[tokio::test]
    async fn get_account_returns_none_for_missing() {
        let pool = test_pool().await;
        let result = get_account(&pool, "no-such-id").await.unwrap();
        assert!(result.is_none());
    }
}
