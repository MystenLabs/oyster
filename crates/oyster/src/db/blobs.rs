use sqlx::Row;

use crate::{
    AccountId,
    models::{BlobMetadata, ExpiringBlob},
};

fn row_to_blob(row: sqlx::any::AnyRow) -> BlobMetadata {
    BlobMetadata {
        key: row.get("key"),
        blob_id: row.get("blob_id"),
        bucket_name: row.get("bucket_name"),
        account_id: row.get("account_id"),
        content_type: row.get("content_type"),
        size: row.get("size"),
        md5: row.get("md5"),
        sui_object_id: row.get("sui_object_id"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
    }
}

const BLOB_COLUMNS: &str = "key, blob_id, bucket_name, account_id, content_type, size, md5, sui_object_id, created_at, expires_at";

/// Count the total number of blobs.
pub async fn count_blobs(pool: &super::DbPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&super::sql("SELECT COUNT(*) FROM blobs"))
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
    expires_at: &str,
    sui_object_id: Option<&str>,
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
        "INSERT INTO blobs (key, blob_id, bucket_name, account_id, content_type, size, md5, expires_at, sui_object_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
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
        .bind(expires_at)
        .bind(sui_object_id)
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
    /// On-chain Sui object ID, if applicable.
    pub sui_object_id: Option<String>,
}

/// Delete a blob by bucket name, key, and account, returning its IDs if it existed.
pub async fn delete_blob(
    pool: &super::DbPool,
    bucket_name: &str,
    key: &str,
    account_id: &AccountId,
) -> Result<Option<DeletedBlobInfo>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "DELETE FROM blobs WHERE bucket_name = ? AND key = ? AND account_id = ? RETURNING blob_id, sui_object_id",
    ))
    .bind(bucket_name)
    .bind(key)
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| DeletedBlobInfo {
        blob_id: r.get("blob_id"),
        sui_object_id: r.get("sui_object_id"),
    }))
}

/// Count how many blob metadata rows reference the given blob ID.
pub async fn count_references(pool: &super::DbPool, blob_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&super::sql("SELECT COUNT(*) FROM blobs WHERE blob_id = ?"))
        .bind(blob_id)
        .fetch_one(pool)
        .await
}

/// Fetch blobs with on-chain storage that expire before the given cutoff.
pub async fn get_expiring_blobs(
    pool: &super::DbPool,
    before: &str,
    limit: i64,
) -> Result<Vec<BlobMetadata>, sqlx::Error> {
    let query = format!(
        "SELECT {BLOB_COLUMNS} \
         FROM blobs \
         WHERE sui_object_id IS NOT NULL AND expires_at IS NOT NULL AND expires_at < ? \
         ORDER BY expires_at \
         LIMIT ?",
    );
    let rows = sqlx::query(&super::sql(&query))
        .bind(before)
        .bind(limit)
        .fetch_all(pool)
        .await?;

    Ok(rows.into_iter().map(row_to_blob).collect())
}

/// Fetch expiring blobs that have on-chain storage approaching expiry,
/// joining through accounts → apps to include the per-app webhook URL.
pub async fn get_expiring_blobs_with_accounts(
    pool: &super::DbPool,
    before: &str,
    limit: i64,
) -> Result<Vec<ExpiringBlob>, sqlx::Error> {
    sqlx::query_as::<_, ExpiringBlob>(&super::sql(
        "SELECT b.account_id, b.sui_object_id, b.size, b.expires_at, a.webhook_url \
         FROM blobs b \
         JOIN accounts acc ON b.account_id = acc.id \
         JOIN apps a ON acc.app_id = a.id \
         WHERE b.sui_object_id IS NOT NULL \
           AND b.expires_at IS NOT NULL \
           AND b.expires_at < ? \
         ORDER BY b.expires_at \
         LIMIT ?",
    ))
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Update the expiration timestamp for a blob identified by its Sui object ID.
pub async fn update_blob_expires_at(
    pool: &super::DbPool,
    sui_object_id: &str,
    new_expires_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "UPDATE blobs SET expires_at = ? WHERE sui_object_id = ?",
    ))
    .bind(new_expires_at)
    .bind(sui_object_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete all blobs in a bucket, returning their IDs for backend cleanup.
pub async fn delete_blobs_in_bucket(
    pool: &super::DbPool,
    bucket_name: &str,
) -> Result<Vec<DeletedBlobInfo>, sqlx::Error> {
    // TODO: This does not delete the actual blobs, it just deletes them from the blobs table.
    // We should also delete the blobs from storage.
    let rows = sqlx::query(&super::sql(
        "DELETE FROM blobs WHERE bucket_name = ? RETURNING blob_id, sui_object_id",
    ))
    .bind(bucket_name)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DeletedBlobInfo {
            blob_id: r.get("blob_id"),
            sui_object_id: r.get("sui_object_id"),
        })
        .collect())
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
    async fn get_expiring_blobs_returns_approaching() {
        let pool = test_pool().await;
        let (account_id, bucket_name) = seed_account_and_bucket(&pool).await;

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        insert_blob(
            &pool,
            "file1.txt",
            "blob-hash-1",
            &bucket_name,
            &account_id,
            "text/plain",
            100,
            "d41d8cd98f00b204e9800998ecf8427e",
            &expires_at,
            Some("0xabc123"),
        )
        .await
        .unwrap();

        let cutoff = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(7))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let blobs = get_expiring_blobs(&pool, &cutoff, 100).await.unwrap();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].sui_object_id.as_deref(), Some("0xabc123"));
    }

    #[tokio::test]
    async fn get_expiring_blobs_skips_no_sui_object() {
        let pool = test_pool().await;
        let (account_id, bucket_name) = seed_account_and_bucket(&pool).await;

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        insert_blob(
            &pool,
            "file2.txt",
            "blob-hash-2",
            &bucket_name,
            &account_id,
            "text/plain",
            100,
            "d41d8cd98f00b204e9800998ecf8427e",
            &expires_at,
            None,
        )
        .await
        .unwrap();

        let cutoff = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(7))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let blobs = get_expiring_blobs(&pool, &cutoff, 100).await.unwrap();
        assert_eq!(blobs.len(), 0);
    }

    #[tokio::test]
    async fn update_blob_expires_at_works() {
        let pool = test_pool().await;
        let (account_id, bucket_name) = seed_account_and_bucket(&pool).await;

        let expires_at = "2026-03-01 00:00:00";
        let blob = insert_blob(
            &pool,
            "file4.txt",
            "blob-hash-4",
            &bucket_name,
            &account_id,
            "text/plain",
            100,
            "d41d8cd98f00b204e9800998ecf8427e",
            expires_at,
            Some("0xupdate789"),
        )
        .await
        .unwrap();

        let new_expires_at = "2026-06-01 00:00:00";
        update_blob_expires_at(&pool, "0xupdate789", new_expires_at)
            .await
            .unwrap();

        let updated = get_blob_by_key(&pool, &blob.bucket_name, &blob.key)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.expires_at.as_deref(), Some(new_expires_at));
    }

    #[tokio::test]
    async fn get_expiring_blobs_with_accounts_returns_account() {
        let pool = test_pool().await;
        let (account_id, bucket_name) = seed_account_and_bucket(&pool).await;

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        insert_blob(
            &pool,
            "pa1.txt",
            "blob-hash-pa1",
            &bucket_name,
            &account_id,
            "text/plain",
            100,
            "d41d8cd98f00b204e9800998ecf8427e",
            &expires_at,
            Some("0xpa1"),
        )
        .await
        .unwrap();

        let cutoff = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(7))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let blobs = get_expiring_blobs_with_accounts(&pool, &cutoff, 100)
            .await
            .unwrap();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].sui_object_id, "0xpa1");
        assert_eq!(blobs[0].account_id, account_id);
    }

    #[tokio::test]
    async fn get_expiring_blobs_with_accounts_returns_both() {
        let pool = test_pool().await;
        let (acct1, bucket1) = seed_account_and_bucket(&pool).await;
        let (acct2, bucket2) = seed_account_and_bucket(&pool).await;

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        insert_blob(
            &pool,
            "mixed1.txt",
            "blob-hash-mixed1",
            &bucket1,
            &acct1,
            "text/plain",
            100,
            "d41d8cd98f00b204e9800998ecf8427e",
            &expires_at,
            Some("0xmixed1"),
        )
        .await
        .unwrap();

        insert_blob(
            &pool,
            "mixed2.txt",
            "blob-hash-mixed2",
            &bucket2,
            &acct2,
            "text/plain",
            200,
            "d41d8cd98f00b204e9800998ecf8427e",
            &expires_at,
            Some("0xmixed2"),
        )
        .await
        .unwrap();

        let cutoff = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(7))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let blobs = get_expiring_blobs_with_accounts(&pool, &cutoff, 100)
            .await
            .unwrap();
        assert_eq!(blobs.len(), 2);
    }
}
