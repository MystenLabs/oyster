use sqlx::Row;

use crate::{AccountId, models::Bucket};

fn row_to_bucket(row: sqlx::any::AnyRow) -> Bucket {
    Bucket {
        name: row.get("name"),
        account_id: row.get("account_id"),
        created_at: row.get("created_at"),
    }
}

/// Create a new bucket for the given account.
pub async fn create_bucket(
    pool: &super::DbPool,
    account_id: &AccountId,
    name: &str,
) -> Result<Bucket, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "INSERT INTO buckets (name, account_id) VALUES (?, ?) RETURNING name, account_id, created_at",
    ))
    .bind(name)
    .bind(account_id)
    .fetch_one(pool)
    .await?;

    Ok(row_to_bucket(row))
}

/// List buckets for an account with cursor-based pagination.
pub async fn list_buckets(
    pool: &super::DbPool,
    account_id: &AccountId,
    after_created_at: Option<&str>,
    after_name: Option<&str>,
    limit: i64,
) -> Result<Vec<Bucket>, sqlx::Error> {
    let rows = match (after_created_at, after_name) {
        (Some(created_at), Some(name)) => {
            sqlx::query(&super::sql(
                "SELECT name, account_id, created_at FROM buckets \
                 WHERE account_id = ? AND (created_at, name) > (?, ?) \
                 ORDER BY created_at, name LIMIT ?",
            ))
            .bind(account_id)
            .bind(created_at)
            .bind(name)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        _ => {
            sqlx::query(&super::sql(
                "SELECT name, account_id, created_at FROM buckets \
                 WHERE account_id = ? ORDER BY created_at, name LIMIT ?",
            ))
            .bind(account_id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
    };
    Ok(rows.into_iter().map(row_to_bucket).collect())
}

/// Fetch a single bucket by name, scoped to the given account.
pub async fn get_bucket(
    pool: &super::DbPool,
    bucket_name: &str,
    account_id: &AccountId,
) -> Result<Option<Bucket>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT name, account_id, created_at FROM buckets WHERE name = ? AND account_id = ?",
    ))
    .bind(bucket_name)
    .bind(account_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(row_to_bucket))
}

/// Delete a bucket. Returns `true` if the bucket existed.
pub async fn delete_bucket(
    pool: &super::DbPool,
    bucket_name: &str,
    account_id: &AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "DELETE FROM buckets WHERE name = ? AND account_id = ?",
    ))
    .bind(bucket_name)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
