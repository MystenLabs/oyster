use super::DbPool;
use crate::{error::Error, models::Account};

/// Insert a new account.
pub async fn create_account(pool: &DbPool) -> Result<Account, Error> {
    let id = uuid::Uuid::new_v4().to_string();

    sqlx::query("INSERT INTO accounts (id) VALUES (?)")
        .bind(&id)
        .execute(pool)
        .await?;

    get_account(pool, &id).await
}

/// Fetch an account by ID, returning `AccountNotFound` if it does not exist.
pub async fn get_account(pool: &DbPool, id: &str) -> Result<Account, Error> {
    sqlx::query_as::<_, Account>(
        "SELECT id, created_at, updated_at
         FROM accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(Error::AccountNotFound)
}

/// Check that an account exists, returning `AccountNotFound` if not.
pub async fn account_exists(pool: &DbPool, id: &str) -> Result<(), Error> {
    let exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM accounts WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    match exists {
        Some(_) => Ok(()),
        None => Err(Error::AccountNotFound),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> DbPool {
        db::create_pool("sqlite::memory:")
            .await
            .expect("in-memory pool")
    }

    #[tokio::test]
    async fn create_account_returns_well_formed_account() {
        let pool = test_pool().await;

        let account = create_account(&pool).await.unwrap();

        assert!(!account.id.is_empty());
        assert!(!account.created_at.is_empty());
        assert!(!account.updated_at.is_empty());
    }

    #[tokio::test]
    async fn get_account_not_found() {
        let pool = test_pool().await;

        let err = get_account(&pool, "nonexistent-id").await.unwrap_err();
        assert!(
            matches!(err, Error::AccountNotFound),
            "expected AccountNotFound, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn account_exists_returns_ok() {
        let pool = test_pool().await;

        let account = create_account(&pool).await.unwrap();
        account_exists(&pool, &account.id).await.unwrap();
    }

    #[tokio::test]
    async fn account_exists_returns_error_for_missing() {
        let pool = test_pool().await;

        let err = account_exists(&pool, "nonexistent-id").await.unwrap_err();
        assert!(
            matches!(err, Error::AccountNotFound),
            "expected AccountNotFound, got: {err:?}"
        );
    }
}
