use sqlx::Row;
use uuid::Uuid;

use crate::{
    AccountId,
    models::{BlobMetadata, ExpiringBlob},
};

fn row_to_blob(row: sqlx::any::AnyRow) -> BlobMetadata {
    BlobMetadata {
        object_id: row.get("object_id"),
        blob_id: row.get("blob_id"),
        bucket_id: row.get("bucket_id"),
        account_id: row.get("account_id"),
        content_type: row.get("content_type"),
        size: row.get("size"),
        sui_object_id: row.get("sui_object_id"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
    }
}

/// Count the total number of blobs.
pub async fn count_blobs(pool: &super::DbPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&super::sql("SELECT COUNT(*) FROM blobs"))
        .fetch_one(pool)
        .await
}

/// Insert a new blob metadata row and return it.
#[allow(clippy::too_many_arguments)]
pub async fn insert_blob(
    pool: &super::DbPool,
    blob_id: &str,
    bucket_id: &str,
    account_id: &AccountId,
    content_type: &str,
    size: i64,
    expires_at: &str,
    sui_object_id: Option<&str>,
) -> Result<BlobMetadata, sqlx::Error> {
    let object_id = Uuid::new_v4().to_string();
    let row = sqlx::query(&super::sql(
        "INSERT INTO blobs (object_id, blob_id, bucket_id, account_id, content_type, size, expires_at, sui_object_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING object_id, blob_id, bucket_id, account_id, content_type, size, sui_object_id, created_at, expires_at",
    ))
    .bind(&object_id)
    .bind(blob_id)
    .bind(bucket_id)
    .bind(account_id)
    .bind(content_type)
    .bind(size)
    .bind(expires_at)
    .bind(sui_object_id)
    .fetch_one(pool)
    .await?;

    Ok(row_to_blob(row))
}

/// Fetch blob metadata by its internal object ID.
pub async fn get_blob_by_object_id(
    pool: &super::DbPool,
    object_id: &str,
) -> Result<Option<BlobMetadata>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, sui_object_id, created_at, expires_at \
         FROM blobs WHERE object_id = ?",
    ))
    .bind(object_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(row_to_blob))
}

/// Fetch blob metadata by its content-addressed blob ID.
#[allow(dead_code)]
pub async fn get_blob_by_blob_id(
    pool: &super::DbPool,
    blob_id: &str,
) -> Result<Option<BlobMetadata>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, sui_object_id, created_at, expires_at \
         FROM blobs WHERE blob_id = ? LIMIT 1",
    ))
    .bind(blob_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(row_to_blob))
}

/// List blobs in a bucket with cursor-based pagination.
pub async fn list_blobs_in_bucket(
    pool: &super::DbPool,
    bucket_id: &str,
    account_id: &AccountId,
    after_created_at: Option<&str>,
    after_id: Option<&str>,
    limit: i64,
) -> Result<Vec<BlobMetadata>, sqlx::Error> {
    let rows = match (after_created_at, after_id) {
        (Some(created_at), Some(id)) => {
            sqlx::query(&super::sql(
                "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, sui_object_id, created_at, expires_at \
                 FROM blobs WHERE bucket_id = ? AND account_id = ? AND (created_at, object_id) > (?, ?) \
                 ORDER BY created_at, object_id LIMIT ?",
            ))
            .bind(bucket_id)
            .bind(account_id)
            .bind(created_at)
            .bind(id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        _ => {
            sqlx::query(&super::sql(
                "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, sui_object_id, created_at, expires_at \
                 FROM blobs WHERE bucket_id = ? AND account_id = ? ORDER BY created_at, object_id LIMIT ?",
            ))
            .bind(bucket_id)
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
    object_id: &str,
    account_id: &AccountId,
    content_type: &str,
) -> Result<Option<BlobMetadata>, sqlx::Error> {
    sqlx::query(&super::sql(
        "UPDATE blobs SET content_type = ? WHERE object_id = ? AND account_id = ?",
    ))
    .bind(content_type)
    .bind(object_id)
    .bind(account_id)
    .execute(pool)
    .await?;

    get_blob_by_object_id(pool, object_id).await
}

/// Information returned when a blob is deleted.
pub struct DeletedBlobInfo {
    /// Content-addressed blob ID.
    pub blob_id: String,
    /// On-chain Sui object ID, if applicable.
    pub sui_object_id: Option<String>,
}

/// Delete a blob by object ID and account, returning its IDs if it existed.
pub async fn delete_blob(
    pool: &super::DbPool,
    object_id: &str,
    account_id: &AccountId,
) -> Result<Option<DeletedBlobInfo>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "DELETE FROM blobs WHERE object_id = ? AND account_id = ? RETURNING blob_id, sui_object_id",
    ))
    .bind(object_id)
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
    let rows = sqlx::query(&super::sql(
        "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, sui_object_id, created_at, expires_at \
         FROM blobs \
         WHERE sui_object_id IS NOT NULL AND expires_at IS NOT NULL AND expires_at < ? \
         ORDER BY expires_at \
         LIMIT ?",
    ))
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_blob).collect())
}

/// Fetch expiring blobs that have on-chain storage approaching expiry.
pub async fn get_expiring_blobs_with_accounts(
    pool: &super::DbPool,
    before: &str,
    limit: i64,
) -> Result<Vec<ExpiringBlob>, sqlx::Error> {
    sqlx::query_as::<_, ExpiringBlob>(&super::sql(
        "SELECT account_id, sui_object_id, size, expires_at \
         FROM blobs \
         WHERE sui_object_id IS NOT NULL \
           AND expires_at IS NOT NULL \
           AND expires_at < ? \
         ORDER BY expires_at \
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
    bucket_id: &str,
) -> Result<Vec<DeletedBlobInfo>, sqlx::Error> {
    // TODO: This does not delete the actual blobs, it just deletes them from the blobs table.
    // We should also delete the blobs from storage.
    let rows = sqlx::query(&super::sql(
        "DELETE FROM blobs WHERE bucket_id = ? RETURNING blob_id, sui_object_id",
    ))
    .bind(bucket_id)
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
        let bucket_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(&super::super::sql("INSERT INTO accounts (id) VALUES (?)"))
            .bind(&account_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(&super::super::sql(
            "INSERT INTO buckets (id, account_id, name) VALUES (?, ?, ?)",
        ))
        .bind(&bucket_id)
        .bind(&account_id)
        .bind("test-bucket")
        .execute(pool)
        .await
        .unwrap();
        (account_id, bucket_id)
    }

    #[tokio::test]
    async fn get_expiring_blobs_returns_approaching() {
        let pool = test_pool().await;
        let (account_id, bucket_id) = seed_account_and_bucket(&pool).await;

        // Insert a blob that expires in 3 days with a sui_object_id
        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        insert_blob(
            &pool,
            "blob-hash-1",
            &bucket_id,
            &account_id,
            "text/plain",
            100,
            &expires_at,
            Some("0xabc123"),
        )
        .await
        .unwrap();

        // Lookahead of 7 days should return it
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
        let (account_id, bucket_id) = seed_account_and_bucket(&pool).await;

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        // Insert blob without sui_object_id
        insert_blob(
            &pool,
            "blob-hash-2",
            &bucket_id,
            &account_id,
            "text/plain",
            100,
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
        let (account_id, bucket_id) = seed_account_and_bucket(&pool).await;

        let expires_at = "2026-03-01 00:00:00";
        let blob = insert_blob(
            &pool,
            "blob-hash-4",
            &bucket_id,
            &account_id,
            "text/plain",
            100,
            expires_at,
            Some("0xupdate789"),
        )
        .await
        .unwrap();

        let new_expires_at = "2026-06-01 00:00:00";
        update_blob_expires_at(&pool, "0xupdate789", new_expires_at)
            .await
            .unwrap();

        let updated = get_blob_by_object_id(&pool, &blob.object_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.expires_at.as_deref(), Some(new_expires_at));
    }

    #[tokio::test]
    async fn get_expiring_blobs_with_accounts_returns_account() {
        let pool = test_pool().await;
        let (account_id, bucket_id) = seed_account_and_bucket(&pool).await;

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        insert_blob(
            &pool,
            "blob-hash-pa1",
            &bucket_id,
            &account_id,
            "text/plain",
            100,
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
            "blob-hash-mixed1",
            &bucket1,
            &acct1,
            "text/plain",
            100,
            &expires_at,
            Some("0xmixed1"),
        )
        .await
        .unwrap();

        insert_blob(
            &pool,
            "blob-hash-mixed2",
            &bucket2,
            &acct2,
            "text/plain",
            200,
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
