//! Helpers for the `dead_letter_orphans` table.
//!
//! See `routes::blobs::compensation` for the only writer.

use crate::AccountId;

/// Insert (or refresh) a dead-letter row for an orphaned on-chain
/// `PooledBlob`. Uses `ON CONFLICT (blob_id, account_id) DO UPDATE` so
/// repeated failed compensations for the same blob just overwrite the
/// recorded error strings — the row represents "one orphan to reap",
/// which a single dedup-aware reaper picks up.
pub async fn insert_orphan(
    pool: &super::DbPool,
    blob_id: &str,
    account_id: &AccountId,
    pool_id: Option<&str>,
    encoded_size: i64,
    original_db_error: &str,
    compensation_error: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "INSERT INTO dead_letter_orphans \
             (blob_id, account_id, pool_id, encoded_size, \
              original_db_error, compensation_error) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (blob_id, account_id) DO UPDATE SET \
             pool_id = excluded.pool_id, \
             encoded_size = excluded.encoded_size, \
             original_db_error = excluded.original_db_error, \
             compensation_error = excluded.compensation_error",
    ))
    .bind(blob_id)
    .bind(account_id)
    .bind(pool_id)
    .bind(encoded_size)
    .bind(original_db_error)
    .bind(compensation_error)
    .execute(pool)
    .await?;
    Ok(())
}

/// Count the dead-letter rows for a given `(blob_id, account_id)` —
/// the verification surface for the compensation dead-letter path in
/// integration tests. Not used by production code paths.
pub async fn count_orphans_for(
    pool: &super::DbPool,
    blob_id: &str,
    account_id: &AccountId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&super::sql(
        "SELECT COUNT(*) FROM dead_letter_orphans WHERE blob_id = ? AND account_id = ?",
    ))
    .bind(blob_id)
    .bind(account_id)
    .fetch_one(pool)
    .await
}
