use sqlx::Row;

use crate::{AccountId, models::BlobMetadata};

fn row_to_blob(row: sqlx::any::AnyRow) -> BlobMetadata {
    BlobMetadata {
        key: row.get("key"),
        blob_id: row.get("blob_id"),
        bucket_name: row.get("bucket_name"),
        account_id: row.get("account_id"),
        content_type: row.get("content_type"),
        size: row.get("size"),
        md5: row.get("md5"),
        pooled_blob_object_id: row.get("pooled_blob_object_id"),
        encoded_size: row.get("encoded_size"),
        created_at: row.get("created_at"),
    }
}

const BLOB_COLUMNS: &str = "key, blob_id, bucket_name, account_id, content_type, size, md5, pooled_blob_object_id, encoded_size, created_at";

/// Count the total number of blobs.
pub async fn count_blobs(pool: &super::DbPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&super::sql("SELECT COUNT(*) FROM blobs"))
        .fetch_one(pool)
        .await
}

/// Count how many blobs exist in a given bucket.
pub async fn count_blobs_in_bucket(
    pool: &super::DbPool,
    bucket_name: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&super::sql(
        "SELECT COUNT(*) FROM blobs WHERE bucket_name = ?",
    ))
    .bind(bucket_name)
    .fetch_one(pool)
    .await
}

/// Insert a new blob metadata row (or overwrite on key conflict) and return it.
#[allow(clippy::too_many_arguments)]
pub async fn insert_blob(
    pool: &super::DbPool,
    key: &str,
    blob_id: &str,
    bucket_name: &str,
    account_id: &AccountId,
    content_type: &str,
    size: i64,
    md5: &str,
    pooled_blob_object_id: Option<&str>,
    encoded_size: Option<i64>,
    certified_epoch: Option<i64>,
) -> Result<BlobMetadata, sqlx::Error> {
    // Delete-then-insert for cross-DB upsert compatibility
    sqlx::query(&super::sql(
        "DELETE FROM blobs WHERE bucket_name = ? AND key = ?",
    ))
    .bind(bucket_name)
    .bind(key)
    .execute(pool)
    .await?;

    let query = format!(
        "INSERT INTO blobs (key, blob_id, bucket_name, account_id, content_type, size, md5, pooled_blob_object_id, encoded_size, certified_epoch) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING {BLOB_COLUMNS}",
    );
    let row = sqlx::query(&super::sql(&query))
        .bind(key)
        .bind(blob_id)
        .bind(bucket_name)
        .bind(account_id)
        .bind(content_type)
        .bind(size)
        .bind(md5)
        .bind(pooled_blob_object_id)
        .bind(encoded_size)
        .bind(certified_epoch)
        .fetch_one(pool)
        .await?;

    Ok(row_to_blob(row))
}

/// Fetch blob metadata by its content-addressed blob ID.
#[allow(dead_code)]
pub async fn get_blob_by_blob_id(
    pool: &super::DbPool,
    blob_id: &str,
) -> Result<Option<BlobMetadata>, sqlx::Error> {
    let query = format!("SELECT {BLOB_COLUMNS} FROM blobs WHERE blob_id = ? LIMIT 1");
    let row = sqlx::query(&super::sql(&query))
        .bind(blob_id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(row_to_blob))
}

/// Find an existing `PooledBlob` ObjectID for the given account and blob ID,
/// if any blob row already references this content under that account.
pub async fn find_pooled_blob_object_id_for_account(
    pool: &super::DbPool,
    account_id: &AccountId,
    blob_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(&super::sql(
        "SELECT pooled_blob_object_id FROM blobs \
         WHERE account_id = ? AND blob_id = ? AND pooled_blob_object_id IS NOT NULL LIMIT 1",
    ))
    .bind(account_id)
    .bind(blob_id)
    .fetch_optional(pool)
    .await
    .map(|opt: Option<Option<String>>| opt.flatten())
}

/// What the upload-path dedup needs to know about content this account
/// already stores: the on-chain `PooledBlob` to reuse and whether Oyster
/// has seen it certified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupRow {
    /// On-chain `PooledBlob` object ID.
    pub pooled_blob_object_id: String,
    /// `blobs.certified_epoch`; `None` means not known to be certified.
    pub certified_epoch: Option<i64>,
}

/// Find a row for `(account, blob_id)` that references an on-chain
/// `PooledBlob`, preferring one already stamped certified so that the
/// dedup short-circuit needs no chain read when any row knows the
/// answer.
pub async fn find_dedup_row_for_account(
    pool: &super::DbPool,
    account_id: &AccountId,
    blob_id: &str,
) -> Result<Option<DedupRow>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT pooled_blob_object_id, certified_epoch FROM blobs \
         WHERE account_id = ? AND blob_id = ? AND pooled_blob_object_id IS NOT NULL \
         ORDER BY CASE WHEN certified_epoch IS NULL THEN 1 ELSE 0 END LIMIT 1",
    ))
    .bind(account_id)
    .bind(blob_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| DedupRow {
        pooled_blob_object_id: r.get("pooled_blob_object_id"),
        certified_epoch: r.get("certified_epoch"),
    }))
}

/// Stamp `certified_epoch` on every row of `(account, blob_id)` that
/// does not have one yet. Returns the number of rows updated.
pub async fn set_certified_epoch(
    pool: &super::DbPool,
    account_id: &AccountId,
    blob_id: &str,
    certified_epoch: i64,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "UPDATE blobs SET certified_epoch = ? \
         WHERE account_id = ? AND blob_id = ? AND certified_epoch IS NULL",
    ))
    .bind(certified_epoch)
    .bind(account_id)
    .bind(blob_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Fetch blob metadata by bucket name and key.
pub async fn get_blob_by_key(
    pool: &super::DbPool,
    bucket_name: &str,
    key: &str,
) -> Result<Option<BlobMetadata>, sqlx::Error> {
    let query = format!("SELECT {BLOB_COLUMNS} FROM blobs WHERE bucket_name = ? AND key = ?");
    let row = sqlx::query(&super::sql(&query))
        .bind(bucket_name)
        .bind(key)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(row_to_blob))
}

/// List blobs in a bucket with cursor-based pagination.
pub async fn list_blobs_in_bucket(
    pool: &super::DbPool,
    bucket_name: &str,
    account_id: &AccountId,
    after_created_at: Option<&str>,
    after_key: Option<&str>,
    limit: i64,
) -> Result<Vec<BlobMetadata>, sqlx::Error> {
    let rows = match (after_created_at, after_key) {
        (Some(created_at), Some(key)) => {
            let query = format!(
                "SELECT {BLOB_COLUMNS} \
                 FROM blobs WHERE bucket_name = ? AND account_id = ? AND (created_at, key) > (?, ?) \
                 ORDER BY created_at, key LIMIT ?",
            );
            sqlx::query(&super::sql(&query))
                .bind(bucket_name)
                .bind(account_id)
                .bind(created_at)
                .bind(key)
                .bind(limit)
                .fetch_all(pool)
                .await?
        }
        _ => {
            let query = format!(
                "SELECT {BLOB_COLUMNS} \
                 FROM blobs WHERE bucket_name = ? AND account_id = ? ORDER BY created_at, key LIMIT ?",
            );
            sqlx::query(&super::sql(&query))
                .bind(bucket_name)
                .bind(account_id)
                .bind(limit)
                .fetch_all(pool)
                .await?
        }
    };
    Ok(rows.into_iter().map(row_to_blob).collect())
}

/// Update a blob's content type.
pub async fn update_blob_metadata(
    pool: &super::DbPool,
    bucket_name: &str,
    key: &str,
    account_id: &AccountId,
    content_type: &str,
) -> Result<Option<BlobMetadata>, sqlx::Error> {
    sqlx::query(&super::sql(
        "UPDATE blobs SET content_type = ? WHERE bucket_name = ? AND key = ? AND account_id = ?",
    ))
    .bind(content_type)
    .bind(bucket_name)
    .bind(key)
    .bind(account_id)
    .execute(pool)
    .await?;

    get_blob_by_key(pool, bucket_name, key).await
}

/// Information returned when a blob is deleted.
pub struct DeletedBlobInfo {
    /// Content-addressed blob ID.
    pub blob_id: String,
    /// On-chain Sui object ID of the `PooledBlob`, if applicable.
    pub pooled_blob_object_id: Option<String>,
    /// Walrus-encoded size in bytes, if the row was registered on-chain.
    pub encoded_size: Option<i64>,
}

/// Delete a blob by bucket name, key, and account, returning its IDs if it existed.
pub async fn delete_blob(
    pool: &super::DbPool,
    bucket_name: &str,
    key: &str,
    account_id: &AccountId,
) -> Result<Option<DeletedBlobInfo>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "DELETE FROM blobs WHERE bucket_name = ? AND key = ? AND account_id = ? RETURNING blob_id, pooled_blob_object_id, encoded_size",
    ))
    .bind(bucket_name)
    .bind(key)
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| DeletedBlobInfo {
        blob_id: r.get("blob_id"),
        pooled_blob_object_id: r.get("pooled_blob_object_id"),
        encoded_size: r.get("encoded_size"),
    }))
}

/// A `blobs` row that references an on-chain `PooledBlob` but has no
/// `certified_epoch`: input to `oysterd blobs repair`.
#[derive(Debug, Clone)]
pub struct UncertifiedReference {
    /// Owning account.
    pub account_id: AccountId,
    /// Bucket name.
    pub bucket_name: String,
    /// Object key.
    pub key: String,
    /// Walrus blob ID.
    pub blob_id: String,
    /// On-chain `PooledBlob` object ID.
    pub pooled_blob_object_id: String,
    /// Walrus-encoded size, if the row recorded it.
    pub encoded_size: Option<i64>,
}

/// Every row referencing a `PooledBlob` without a `certified_epoch`,
/// optionally for one account, grouped by account and blob.
pub async fn list_uncertified_references(
    pool: &super::DbPool,
    account_id: Option<&AccountId>,
) -> Result<Vec<UncertifiedReference>, sqlx::Error> {
    let base = "SELECT account_id, bucket_name, key, blob_id, pooled_blob_object_id, encoded_size \
                FROM blobs WHERE pooled_blob_object_id IS NOT NULL AND certified_epoch IS NULL";
    let rows = match account_id {
        Some(account_id) => {
            sqlx::query(&super::sql(&format!(
                "{base} AND account_id = ? ORDER BY account_id, blob_id, created_at, key"
            )))
            .bind(account_id)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query(&super::sql(&format!(
                "{base} ORDER BY account_id, blob_id, created_at, key"
            )))
            .fetch_all(pool)
            .await?
        }
    };
    Ok(rows
        .into_iter()
        .map(|r| UncertifiedReference {
            account_id: r.get("account_id"),
            bucket_name: r.get("bucket_name"),
            key: r.get("key"),
            blob_id: r.get("blob_id"),
            pooled_blob_object_id: r.get("pooled_blob_object_id"),
            encoded_size: r.get("encoded_size"),
        })
        .collect())
}

/// One key referencing a blob, for operator reports.
#[derive(Debug, Clone)]
pub struct BlobReference {
    /// Bucket name.
    pub bucket_name: String,
    /// Object key.
    pub key: String,
    /// Walrus-encoded size, if the row recorded it.
    pub encoded_size: Option<i64>,
}

/// Every `(bucket, key)` of `account` that references `blob_id`.
pub async fn list_references(
    pool: &super::DbPool,
    account_id: &AccountId,
    blob_id: &str,
) -> Result<Vec<BlobReference>, sqlx::Error> {
    let rows = sqlx::query(&super::sql(
        "SELECT bucket_name, key, encoded_size FROM blobs \
         WHERE account_id = ? AND blob_id = ? ORDER BY created_at, key",
    ))
    .bind(account_id)
    .bind(blob_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| BlobReference {
            bucket_name: r.get("bucket_name"),
            key: r.get("key"),
            encoded_size: r.get("encoded_size"),
        })
        .collect())
}

/// Remove every row of `account` that references `blob_id` (and their
/// tags, via the FK cascade where configured). Returns rows removed.
pub async fn delete_references_for_blob(
    pool: &super::DbPool,
    account_id: &AccountId,
    blob_id: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "DELETE FROM blobs WHERE account_id = ? AND blob_id = ?",
    ))
    .bind(account_id)
    .bind(blob_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Count how many blob metadata rows reference the given blob ID.
pub async fn count_references(pool: &super::DbPool, blob_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&super::sql("SELECT COUNT(*) FROM blobs WHERE blob_id = ?"))
        .bind(blob_id)
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

    async fn seed_account_and_bucket(pool: &super::super::DbPool) -> (AccountId, String) {
        let account_id = AccountId::new();
        let bucket_name = format!("test-bucket-{}", uuid::Uuid::new_v4());
        sqlx::query(&super::super::sql("INSERT INTO accounts (id) VALUES (?)"))
            .bind(&account_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(&super::super::sql(
            "INSERT INTO buckets (name, account_id) VALUES (?, ?)",
        ))
        .bind(&bucket_name)
        .bind(&account_id)
        .execute(pool)
        .await
        .unwrap();
        (account_id, bucket_name)
    }

    #[tokio::test]
    async fn insert_blob_persists_encoded_size() {
        let pool = test_pool().await;
        let (account_id, bucket_name) = seed_account_and_bucket(&pool).await;

        insert_blob(
            &pool,
            "file.txt",
            "blob-hash-encoded",
            &bucket_name,
            &account_id,
            "text/plain",
            100,
            "d41d8cd98f00b204e9800998ecf8427e",
            Some("0xpool"),
            Some(123),
            Some(7),
        )
        .await
        .unwrap();

        let fetched = get_blob_by_key(&pool, &bucket_name, "file.txt")
            .await
            .unwrap()
            .expect("blob row");
        assert_eq!(fetched.encoded_size, Some(123));
    }

    #[tokio::test]
    async fn delete_blob_returns_encoded_size() {
        let pool = test_pool().await;
        let (account_id, bucket_name) = seed_account_and_bucket(&pool).await;

        insert_blob(
            &pool,
            "file.txt",
            "blob-hash-delete",
            &bucket_name,
            &account_id,
            "text/plain",
            100,
            "d41d8cd98f00b204e9800998ecf8427e",
            None,
            Some(123),
            None,
        )
        .await
        .unwrap();

        let info = delete_blob(&pool, &bucket_name, "file.txt", &account_id)
            .await
            .unwrap()
            .expect("delete returns info");
        assert_eq!(info.encoded_size, Some(123));
    }
}
