use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::models::Bucket;

fn row_to_bucket(row: sqlx::sqlite::SqliteRow) -> Bucket {
    Bucket {
        id: row.get("id"),
        account_id: row.get("account_id"),
        name: row.get("name"),
        created_at: row.get("created_at"),
    }
}

/// Create a new bucket for the given account.
pub async fn create_bucket(
    pool: &SqlitePool,
    account_id: &str,
    name: &str,
) -> Result<Bucket, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let row = sqlx::query(
        "INSERT INTO buckets (id, account_id, name) VALUES (?, ?, ?) RETURNING id, account_id, name, created_at",
    )
    .bind(&id)
    .bind(account_id)
    .bind(name)
    .fetch_one(pool)
    .await?;

    Ok(row_to_bucket(row))
}

/// List buckets for an account with cursor-based pagination.
pub async fn list_buckets(
    pool: &SqlitePool,
    account_id: &str,
    after_created_at: Option<&str>,
    after_id: Option<&str>,
    limit: i64,
) -> Result<Vec<Bucket>, sqlx::Error> {
    let rows = match (after_created_at, after_id) {
        (Some(created_at), Some(id)) => {
            sqlx::query(
                "SELECT id, account_id, name, created_at FROM buckets \
                 WHERE account_id = ? AND (created_at, id) > (?, ?) \
                 ORDER BY created_at, id LIMIT ?",
            )
            .bind(account_id)
            .bind(created_at)
            .bind(id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        _ => {
            sqlx::query(
                "SELECT id, account_id, name, created_at FROM buckets \
                 WHERE account_id = ? ORDER BY created_at, id LIMIT ?",
            )
            .bind(account_id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
    };
    Ok(rows.into_iter().map(row_to_bucket).collect())
}

/// Fetch a single bucket by ID, scoped to the given account.
pub async fn get_bucket(
    pool: &SqlitePool,
    bucket_id: &str,
    account_id: &str,
) -> Result<Option<Bucket>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, account_id, name, created_at FROM buckets WHERE id = ? AND account_id = ?",
    )
    .bind(bucket_id)
    .bind(account_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(row_to_bucket))
}

/// Delete a bucket. Returns `true` if the bucket existed.
pub async fn delete_bucket(
    pool: &SqlitePool,
    bucket_id: &str,
    account_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM buckets WHERE id = ? AND account_id = ?")
        .bind(bucket_id)
        .bind(account_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
