use std::collections::BTreeMap;

use sqlx::Row;

use crate::AccountId;

/// List all tags for a blob owned by `account_id`.
pub async fn list_tags(
    pool: &super::DbPool,
    account_id: &AccountId,
    bucket: &str,
    key: &str,
) -> Result<BTreeMap<String, String>, sqlx::Error> {
    let rows = sqlx::query(&super::sql(
        "SELECT tag_key, tag_value FROM blob_tags \
         WHERE account_id = ? AND bucket_name = ? AND key = ?",
    ))
    .bind(account_id)
    .bind(bucket)
    .bind(key)
    .fetch_all(pool)
    .await?;

    let mut map = BTreeMap::new();
    for row in rows {
        let k: String = row.get("tag_key");
        let v: String = row.get("tag_value");
        map.insert(k, v);
    }
    Ok(map)
}

/// Fetch a single tag's value.
#[allow(dead_code)]
pub async fn get_tag(
    pool: &super::DbPool,
    account_id: &AccountId,
    bucket: &str,
    key: &str,
    tag_key: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(&super::sql(
        "SELECT tag_value FROM blob_tags \
         WHERE account_id = ? AND bucket_name = ? AND key = ? AND tag_key = ?",
    ))
    .bind(account_id)
    .bind(bucket)
    .bind(key)
    .bind(tag_key)
    .fetch_optional(pool)
    .await
}

/// Insert or update a single tag.
pub async fn upsert_tag(
    pool: &super::DbPool,
    account_id: &AccountId,
    bucket: &str,
    key: &str,
    tag_key: &str,
    tag_value: &str,
) -> Result<(), sqlx::Error> {
    // Delete-then-insert for cross-DB upsert compatibility.
    let mut tx = pool.begin().await?;
    sqlx::query(&super::sql(
        "DELETE FROM blob_tags WHERE bucket_name = ? AND key = ? AND tag_key = ?",
    ))
    .bind(bucket)
    .bind(key)
    .bind(tag_key)
    .execute(&mut *tx)
    .await?;
    sqlx::query(&super::sql(
        "INSERT INTO blob_tags (account_id, bucket_name, key, tag_key, tag_value) \
         VALUES (?, ?, ?, ?, ?)",
    ))
    .bind(account_id)
    .bind(bucket)
    .bind(key)
    .bind(tag_key)
    .bind(tag_value)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Delete a single tag. Returns `true` when a row was removed.
pub async fn delete_tag(
    pool: &super::DbPool,
    account_id: &AccountId,
    bucket: &str,
    key: &str,
    tag_key: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "DELETE FROM blob_tags \
         WHERE account_id = ? AND bucket_name = ? AND key = ? AND tag_key = ?",
    ))
    .bind(account_id)
    .bind(bucket)
    .bind(key)
    .bind(tag_key)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Replace the entire tag set for a blob in a single transaction.
///
/// Used by REST PUT and S3 PutObjectTagging and the initial-tag-set path on
/// PUT blob.
pub async fn replace_all_tags(
    pool: &super::DbPool,
    account_id: &AccountId,
    bucket: &str,
    key: &str,
    tags: &[(String, String)],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(&super::sql(
        "DELETE FROM blob_tags WHERE bucket_name = ? AND key = ?",
    ))
    .bind(bucket)
    .bind(key)
    .execute(&mut *tx)
    .await?;
    for (tag_key, tag_value) in tags {
        sqlx::query(&super::sql(
            "INSERT INTO blob_tags (account_id, bucket_name, key, tag_key, tag_value) \
             VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(account_id)
        .bind(bucket)
        .bind(key)
        .bind(tag_key)
        .bind(tag_value)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Merge the given tags into the existing set (upsert per key).
pub async fn merge_tags(
    pool: &super::DbPool,
    account_id: &AccountId,
    bucket: &str,
    key: &str,
    tags: &[(String, String)],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for (tag_key, tag_value) in tags {
        sqlx::query(&super::sql(
            "DELETE FROM blob_tags WHERE bucket_name = ? AND key = ? AND tag_key = ?",
        ))
        .bind(bucket)
        .bind(key)
        .bind(tag_key)
        .execute(&mut *tx)
        .await?;
        sqlx::query(&super::sql(
            "INSERT INTO blob_tags (account_id, bucket_name, key, tag_key, tag_value) \
             VALUES (?, ?, ?, ?, ?)",
        ))
        .bind(account_id)
        .bind(bucket)
        .bind(key)
        .bind(tag_key)
        .bind(tag_value)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Remove every tag on a blob.
pub async fn clear_tags(
    pool: &super::DbPool,
    account_id: &AccountId,
    bucket: &str,
    key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "DELETE FROM blob_tags \
         WHERE account_id = ? AND bucket_name = ? AND key = ?",
    ))
    .bind(account_id)
    .bind(bucket)
    .bind(key)
    .execute(pool)
    .await?;
    Ok(())
}

/// Count tags for a blob. Not account-scoped; the S3 read paths resolve the
/// blob row under an ownership check first.
pub async fn count_tags(pool: &super::DbPool, bucket: &str, key: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&super::sql(
        "SELECT COUNT(*) FROM blob_tags WHERE bucket_name = ? AND key = ?",
    ))
    .bind(bucket)
    .bind(key)
    .fetch_one(pool)
    .await
}
