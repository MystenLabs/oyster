use super::DbPool;
use crate::{
    error::Error,
    models::{CachedBalance, PendingTransaction},
};

pub async fn create_pending_transaction(
    pool: &DbPool,
    account_id: &str,
    estimated_sui_cost: i64,
    estimated_wal_cost: i64,
) -> Result<PendingTransaction, Error> {
    let id = uuid::Uuid::new_v4().to_string();

    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO pending_transactions (id, account_id, estimated_sui_cost, estimated_wal_cost)
         VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(account_id)
    .bind(estimated_sui_cost)
    .bind(estimated_wal_cost)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE accounts SET cached_sui_balance = cached_sui_balance - ?, cached_wal_balance = cached_wal_balance - ? WHERE id = ?",
    )
    .bind(estimated_sui_cost)
    .bind(estimated_wal_cost)
    .bind(account_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let ptx = sqlx::query_as::<_, PendingTransaction>(
        "SELECT id, account_id, tx_digest, estimated_sui_cost, estimated_wal_cost, actual_sui_cost, actual_wal_cost, status, created_at, resolved_at
         FROM pending_transactions WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(pool)
    .await?;

    Ok(ptx)
}

pub async fn confirm_transaction(
    pool: &DbPool,
    pending_tx_id: &str,
    tx_digest: &str,
    success: bool,
    actual_sui_cost: i64,
    actual_wal_cost: i64,
) -> Result<CachedBalance, Error> {
    let ptx = sqlx::query_as::<_, PendingTransaction>(
        "SELECT id, account_id, tx_digest, estimated_sui_cost, estimated_wal_cost, actual_sui_cost, actual_wal_cost, status, created_at, resolved_at
         FROM pending_transactions WHERE id = ?",
    )
    .bind(pending_tx_id)
    .fetch_optional(pool)
    .await?
    .ok_or(Error::PendingTransactionNotFound)?;

    if ptx.status != "pending" {
        return Err(Error::PendingTransactionAlreadyResolved);
    }

    let mut tx = pool.begin().await?;

    let (sui_correction, wal_correction) = if success {
        // Correction: we already deducted the estimate, now adjust for actual.
        // Add back estimate, then subtract actual = add (estimate - actual).
        (
            ptx.estimated_sui_cost - actual_sui_cost,
            ptx.estimated_wal_cost - actual_wal_cost,
        )
    } else {
        // Refund the full estimate.
        (ptx.estimated_sui_cost, ptx.estimated_wal_cost)
    };

    sqlx::query(
        "UPDATE accounts SET cached_sui_balance = cached_sui_balance + ?, cached_wal_balance = cached_wal_balance + ? WHERE id = ?",
    )
    .bind(sui_correction)
    .bind(wal_correction)
    .bind(&ptx.account_id)
    .execute(&mut *tx)
    .await?;

    let status = if success { "confirmed" } else { "failed" };
    let digest = if tx_digest.is_empty() {
        None
    } else {
        Some(tx_digest)
    };

    sqlx::query(
        "UPDATE pending_transactions SET status = ?, tx_digest = ?, actual_sui_cost = ?, actual_wal_cost = ?, resolved_at = datetime('now') WHERE id = ?",
    )
    .bind(status)
    .bind(digest)
    .bind(actual_sui_cost)
    .bind(actual_wal_cost)
    .bind(pending_tx_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    crate::db::accounts::get_balance(pool, &ptx.account_id).await
}

pub async fn get_stale_pending_transactions(
    pool: &DbPool,
    older_than_minutes: i64,
    limit: i64,
) -> Result<Vec<PendingTransaction>, Error> {
    let rows = sqlx::query_as::<_, PendingTransaction>(
        "SELECT id, account_id, tx_digest, estimated_sui_cost, estimated_wal_cost, actual_sui_cost, actual_wal_cost, status, created_at, resolved_at
         FROM pending_transactions
         WHERE status = 'pending'
           AND created_at < datetime('now', '-' || ? || ' minutes')
         ORDER BY created_at ASC
         LIMIT ?",
    )
    .bind(older_than_minutes)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

pub async fn timeout_pending_transaction(pool: &DbPool, pending_tx_id: &str) -> Result<(), Error> {
    let ptx = sqlx::query_as::<_, PendingTransaction>(
        "SELECT id, account_id, tx_digest, estimated_sui_cost, estimated_wal_cost, actual_sui_cost, actual_wal_cost, status, created_at, resolved_at
         FROM pending_transactions WHERE id = ?",
    )
    .bind(pending_tx_id)
    .fetch_optional(pool)
    .await?
    .ok_or(Error::PendingTransactionNotFound)?;

    if ptx.status != "pending" {
        return Err(Error::PendingTransactionAlreadyResolved);
    }

    let mut tx = pool.begin().await?;

    // Refund the estimate.
    sqlx::query(
        "UPDATE accounts SET cached_sui_balance = cached_sui_balance + ?, cached_wal_balance = cached_wal_balance + ? WHERE id = ?",
    )
    .bind(ptx.estimated_sui_cost)
    .bind(ptx.estimated_wal_cost)
    .bind(&ptx.account_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE pending_transactions SET status = 'timeout', resolved_at = datetime('now') WHERE id = ?",
    )
    .bind(pending_tx_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, models::CreateAccountRequest};

    async fn test_pool() -> DbPool {
        db::create_pool("sqlite::memory:")
            .await
            .expect("in-memory pool")
    }

    async fn create_test_account(pool: &DbPool) -> String {
        let req = CreateAccountRequest {
            min_sui_balance: 1000,
            min_wal_balance: 2000,
            top_up_target_sui: 5000,
            top_up_target_wal: 10000,
        };
        let account = db::accounts::create_account(pool, &req, "cred")
            .await
            .unwrap();

        // Set an initial cached balance.
        db::accounts::set_cached_balance(pool, &account.id, 10000, 20000)
            .await
            .unwrap();

        account.id
    }

    #[tokio::test]
    async fn create_pending_transaction_deducts_balance() {
        let pool = test_pool().await;
        let account_id = create_test_account(&pool).await;

        let ptx = create_pending_transaction(&pool, &account_id, 500, 1000)
            .await
            .unwrap();

        assert_eq!(ptx.account_id, account_id);
        assert_eq!(ptx.estimated_sui_cost, 500);
        assert_eq!(ptx.estimated_wal_cost, 1000);
        assert_eq!(ptx.status, "pending");

        let bal = db::accounts::get_balance(&pool, &account_id).await.unwrap();
        assert_eq!(bal.cached_sui_balance, 9500);
        assert_eq!(bal.cached_wal_balance, 19000);
    }

    #[tokio::test]
    async fn confirm_success_corrects_balance() {
        let pool = test_pool().await;
        let account_id = create_test_account(&pool).await;

        let ptx = create_pending_transaction(&pool, &account_id, 500, 1000)
            .await
            .unwrap();

        // Actual cost was less than estimated.
        let bal = confirm_transaction(&pool, &ptx.id, "digest123", true, 300, 800)
            .await
            .unwrap();

        // Started at 10000/20000, deducted 500/1000, then corrected by +200/+200.
        assert_eq!(bal.cached_sui_balance, 9700);
        assert_eq!(bal.cached_wal_balance, 19200);
    }

    #[tokio::test]
    async fn confirm_failure_refunds_estimate() {
        let pool = test_pool().await;
        let account_id = create_test_account(&pool).await;

        let ptx = create_pending_transaction(&pool, &account_id, 500, 1000)
            .await
            .unwrap();

        let bal = confirm_transaction(&pool, &ptx.id, "", false, 0, 0)
            .await
            .unwrap();

        // Full refund: back to original.
        assert_eq!(bal.cached_sui_balance, 10000);
        assert_eq!(bal.cached_wal_balance, 20000);
    }

    #[tokio::test]
    async fn confirm_already_resolved_returns_error() {
        let pool = test_pool().await;
        let account_id = create_test_account(&pool).await;

        let ptx = create_pending_transaction(&pool, &account_id, 500, 1000)
            .await
            .unwrap();

        confirm_transaction(&pool, &ptx.id, "digest", true, 500, 1000)
            .await
            .unwrap();

        let err = confirm_transaction(&pool, &ptx.id, "digest2", true, 500, 1000)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::PendingTransactionAlreadyResolved));
    }

    #[tokio::test]
    async fn timeout_refunds_and_updates_status() {
        let pool = test_pool().await;
        let account_id = create_test_account(&pool).await;

        let ptx = create_pending_transaction(&pool, &account_id, 500, 1000)
            .await
            .unwrap();

        timeout_pending_transaction(&pool, &ptx.id).await.unwrap();

        let bal = db::accounts::get_balance(&pool, &account_id).await.unwrap();
        assert_eq!(bal.cached_sui_balance, 10000);
        assert_eq!(bal.cached_wal_balance, 20000);
    }

    #[tokio::test]
    async fn get_stale_filters_by_age() {
        let pool = test_pool().await;
        let account_id = create_test_account(&pool).await;

        // Create a pending transaction (just created, so not stale).
        create_pending_transaction(&pool, &account_id, 100, 200)
            .await
            .unwrap();

        // No stale transactions when looking for ones older than 1 minute.
        let stale = get_stale_pending_transactions(&pool, 1, 100).await.unwrap();
        assert!(stale.is_empty());
    }

    #[tokio::test]
    async fn get_balance_returns_cached_values() {
        let pool = test_pool().await;
        let account_id = create_test_account(&pool).await;

        let bal = db::accounts::get_balance(&pool, &account_id).await.unwrap();
        assert_eq!(bal.cached_sui_balance, 10000);
        assert_eq!(bal.cached_wal_balance, 20000);
        assert_eq!(bal.min_sui_balance, 1000);
        assert_eq!(bal.min_wal_balance, 2000);
    }

    #[tokio::test]
    async fn set_cached_balance_updates_timestamp() {
        let pool = test_pool().await;
        let account_id = create_test_account(&pool).await;

        let bal = db::accounts::get_balance(&pool, &account_id).await.unwrap();
        assert!(bal.balance_updated_at.is_some());

        db::accounts::set_cached_balance(&pool, &account_id, 5000, 6000)
            .await
            .unwrap();

        let bal = db::accounts::get_balance(&pool, &account_id).await.unwrap();
        assert_eq!(bal.cached_sui_balance, 5000);
        assert_eq!(bal.cached_wal_balance, 6000);
        assert!(bal.balance_updated_at.is_some());
    }

    #[tokio::test]
    async fn get_random_account_id_empty() {
        let pool = test_pool().await;
        let result = db::accounts::get_random_account_id(&pool).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_random_account_id_populated() {
        let pool = test_pool().await;
        let account_id = create_test_account(&pool).await;
        let result = db::accounts::get_random_account_id(&pool).await.unwrap();
        assert_eq!(result, Some(account_id));
    }
}
