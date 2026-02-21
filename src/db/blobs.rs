use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::models::BlobMetadata;

fn row_to_blob(row: sqlx::sqlite::SqliteRow) -> BlobMetadata {
    BlobMetadata {
        object_id: row.get("object_id"),
        blob_id: row.get("blob_id"),
        bucket_id: row.get("bucket_id"),
        account_id: row.get("account_id"),
        content_type: row.get("content_type"),
        size: row.get("size"),
        auto_extend_duration: row.get("auto_extend_duration"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
    }
}

pub async fn insert_blob(
    pool: &SqlitePool,
    blob_id: &str,
    bucket_id: &str,
    account_id: &str,
    content_type: &str,
    size: i64,
    expires_at: &str,
) -> Result<BlobMetadata, sqlx::Error> {
    let object_id = Uuid::new_v4().to_string();
    let auto_extend_duration = "30d";
    let row = sqlx::query(
        "INSERT INTO blobs (object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, created_at, expires_at",
    )
    .bind(&object_id)
    .bind(blob_id)
    .bind(bucket_id)
    .bind(account_id)
    .bind(content_type)
    .bind(size)
    .bind(auto_extend_duration)
    .bind(expires_at)
    .fetch_one(pool)
    .await?;

    Ok(row_to_blob(row))
}

pub async fn get_blob_by_object_id(
    pool: &SqlitePool,
    object_id: &str,
) -> Result<Option<BlobMetadata>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, created_at, expires_at \
         FROM blobs WHERE object_id = ?",
    )
    .bind(object_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(row_to_blob))
}

#[allow(dead_code)]
pub async fn get_blob_by_blob_id(
    pool: &SqlitePool,
    blob_id: &str,
) -> Result<Option<BlobMetadata>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, created_at, expires_at \
         FROM blobs WHERE blob_id = ? LIMIT 1",
    )
    .bind(blob_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(row_to_blob))
}

pub async fn list_blobs_in_bucket(
    pool: &SqlitePool,
    bucket_id: &str,
    account_id: &str,
    after_created_at: Option<&str>,
    after_id: Option<&str>,
    limit: i64,
) -> Result<Vec<BlobMetadata>, sqlx::Error> {
    let rows = match (after_created_at, after_id) {
        (Some(created_at), Some(id)) => {
            sqlx::query(
                "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, created_at, expires_at \
                 FROM blobs WHERE bucket_id = ? AND account_id = ? AND (created_at, object_id) > (?, ?) \
                 ORDER BY created_at, object_id LIMIT ?",
            )
            .bind(bucket_id)
            .bind(account_id)
            .bind(created_at)
            .bind(id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        _ => {
            sqlx::query(
                "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, created_at, expires_at \
                 FROM blobs WHERE bucket_id = ? AND account_id = ? ORDER BY created_at, object_id LIMIT ?",
            )
            .bind(bucket_id)
            .bind(account_id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
    };
    Ok(rows.into_iter().map(row_to_blob).collect())
}

pub async fn update_blob_metadata(
    pool: &SqlitePool,
    object_id: &str,
    account_id: &str,
    content_type: Option<&str>,
    auto_extend_duration: Option<&str>,
) -> Result<Option<BlobMetadata>, sqlx::Error> {
    if let Some(ct) = content_type {
        if let Some(aed) = auto_extend_duration {
            sqlx::query(
                "UPDATE blobs SET content_type = ?, auto_extend_duration = ? WHERE object_id = ? AND account_id = ?",
            )
            .bind(ct)
            .bind(aed)
            .bind(object_id)
            .bind(account_id)
            .execute(pool)
            .await?;
        } else {
            sqlx::query("UPDATE blobs SET content_type = ? WHERE object_id = ? AND account_id = ?")
                .bind(ct)
                .bind(object_id)
                .bind(account_id)
                .execute(pool)
                .await?;
        }
    } else if let Some(aed) = auto_extend_duration {
        sqlx::query(
            "UPDATE blobs SET auto_extend_duration = ? WHERE object_id = ? AND account_id = ?",
        )
        .bind(aed)
        .bind(object_id)
        .bind(account_id)
        .execute(pool)
        .await?;
    }

    get_blob_by_object_id(pool, object_id).await
}

pub async fn delete_blob(
    pool: &SqlitePool,
    object_id: &str,
    account_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row =
        sqlx::query("DELETE FROM blobs WHERE object_id = ? AND account_id = ? RETURNING blob_id")
            .bind(object_id)
            .bind(account_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|r| r.get("blob_id")))
}

pub async fn count_references(pool: &SqlitePool, blob_id: &str) -> Result<i64, sqlx::Error> {
    let row = sqlx::query("SELECT COUNT(*) as count FROM blobs WHERE blob_id = ?")
        .bind(blob_id)
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i32, _>("count") as i64)
}

pub async fn delete_blobs_in_bucket(
    pool: &SqlitePool,
    bucket_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query("DELETE FROM blobs WHERE bucket_id = ? RETURNING blob_id")
        .bind(bucket_id)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|r| r.get("blob_id")).collect())
}
