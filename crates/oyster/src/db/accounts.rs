use sqlx::Row;

use crate::{AccountId, models::Account};

/// Insert a new account.
pub async fn create_account(pool: &super::DbPool) -> Result<Account, sqlx::Error> {
    let id = AccountId::new();
    let name = id.to_string();
    let row = sqlx::query(&super::sql(
        "INSERT INTO accounts (id, name) VALUES (?, ?) RETURNING id, name, created_at, updated_at",
    ))
    .bind(&id)
    .bind(&name)
    .fetch_one(pool)
    .await?;

    Ok(Account {
        id: row.get("id"),
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
        "SELECT id, name, created_at, updated_at FROM accounts WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| Account {
        id: r.get("id"),
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
        assert_eq!(account.name, account.id.to_string());
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
        let result = get_account(&pool, &AccountId::new()).await.unwrap();
        assert!(result.is_none());
    }
}
