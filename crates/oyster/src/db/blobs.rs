use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::models::{BlobMetadata, ExpiringBlob};

fn row_to_blob(row: sqlx::sqlite::SqliteRow) -> BlobMetadata {
    BlobMetadata {
        object_id: row.get("object_id"),
        blob_id: row.get("blob_id"),
        bucket_id: row.get("bucket_id"),
        account_id: row.get("account_id"),
        content_type: row.get("content_type"),
        size: row.get("size"),
        auto_extend_duration: row.get("auto_extend_duration"),
        sui_object_id: row.get("sui_object_id"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_blob(
    pool: &SqlitePool,
    blob_id: &str,
    bucket_id: &str,
    account_id: &str,
    content_type: &str,
    size: i64,
    expires_at: &str,
    sui_object_id: Option<&str>,
) -> Result<BlobMetadata, sqlx::Error> {
    let object_id = Uuid::new_v4().to_string();
    let auto_extend_duration = "30d";
    let row = sqlx::query(
        "INSERT INTO blobs (object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, expires_at, sui_object_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, sui_object_id, created_at, expires_at",
    )
    .bind(&object_id)
    .bind(blob_id)
    .bind(bucket_id)
    .bind(account_id)
    .bind(content_type)
    .bind(size)
    .bind(auto_extend_duration)
    .bind(expires_at)
    .bind(sui_object_id)
    .fetch_one(pool)
    .await?;

    Ok(row_to_blob(row))
}

pub async fn get_blob_by_object_id(
    pool: &SqlitePool,
    object_id: &str,
) -> Result<Option<BlobMetadata>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, sui_object_id, created_at, expires_at \
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
        "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, sui_object_id, created_at, expires_at \
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
                "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, sui_object_id, created_at, expires_at \
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
                "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, sui_object_id, created_at, expires_at \
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

pub struct DeletedBlobInfo {
    pub blob_id: String,
    pub sui_object_id: Option<String>,
}

pub async fn delete_blob(
    pool: &SqlitePool,
    object_id: &str,
    account_id: &str,
) -> Result<Option<DeletedBlobInfo>, sqlx::Error> {
    let row = sqlx::query(
        "DELETE FROM blobs WHERE object_id = ? AND account_id = ? RETURNING blob_id, sui_object_id",
    )
    .bind(object_id)
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| DeletedBlobInfo {
        blob_id: r.get("blob_id"),
        sui_object_id: r.get("sui_object_id"),
    }))
}

pub async fn count_references(pool: &SqlitePool, blob_id: &str) -> Result<i64, sqlx::Error> {
    let row = sqlx::query("SELECT COUNT(*) as count FROM blobs WHERE blob_id = ?")
        .bind(blob_id)
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i32, _>("count") as i64)
}

pub async fn get_expiring_blobs(
    pool: &SqlitePool,
    before: &str,
    limit: i64,
) -> Result<Vec<BlobMetadata>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, sui_object_id, created_at, expires_at \
         FROM blobs \
         WHERE sui_object_id IS NOT NULL AND auto_extend_duration IS NOT NULL AND expires_at IS NOT NULL AND expires_at < ? \
         ORDER BY expires_at \
         LIMIT ?",
    )
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_blob).collect())
}

pub async fn get_expiring_blobs_with_accounts(
    pool: &SqlitePool,
    before: &str,
    limit: i64,
) -> Result<Vec<ExpiringBlob>, sqlx::Error> {
    sqlx::query_as::<_, ExpiringBlob>(
        "SELECT b.sui_object_id, b.size, b.expires_at, a.pearl_account_id, \
                a.min_sui_balance, a.min_wal_balance \
         FROM blobs b \
         JOIN accounts a ON b.account_id = a.id \
         WHERE b.sui_object_id IS NOT NULL \
           AND b.auto_extend_duration IS NOT NULL \
           AND b.expires_at IS NOT NULL \
           AND b.expires_at < ? \
           AND a.pearl_account_id IS NOT NULL \
         ORDER BY b.expires_at \
         LIMIT ?",
    )
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn update_blob_expires_at(
    pool: &SqlitePool,
    sui_object_id: &str,
    new_expires_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE blobs SET expires_at = ? WHERE sui_object_id = ?")
        .bind(new_expires_at)
        .bind(sui_object_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_blobs_in_bucket(
    pool: &SqlitePool,
    bucket_id: &str,
) -> Result<Vec<DeletedBlobInfo>, sqlx::Error> {
    let rows =
        sqlx::query("DELETE FROM blobs WHERE bucket_id = ? RETURNING blob_id, sui_object_id")
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

    async fn test_pool() -> SqlitePool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    async fn seed_account_and_bucket(pool: &SqlitePool) -> (String, String) {
        let account_id = uuid::Uuid::new_v4().to_string();
        let bucket_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO accounts (id) VALUES (?)")
            .bind(&account_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO buckets (id, account_id, name) VALUES (?, ?, ?)")
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
    async fn get_expiring_blobs_skips_no_auto_extend() {
        let pool = test_pool().await;
        let (account_id, bucket_id) = seed_account_and_bucket(&pool).await;

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        // Insert blob with sui_object_id but null auto_extend_duration
        let oid = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO blobs (object_id, blob_id, bucket_id, account_id, content_type, size, auto_extend_duration, expires_at, sui_object_id) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)",
        )
        .bind(&oid)
        .bind("blob-hash-3")
        .bind(&bucket_id)
        .bind(&account_id)
        .bind("text/plain")
        .bind(100i64)
        .bind(&expires_at)
        .bind("0xdef456")
        .execute(&pool)
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

    async fn seed_account_with_pearl(
        pool: &SqlitePool,
        pearl_account_id: &str,
    ) -> (String, String) {
        let account_id = uuid::Uuid::new_v4().to_string();
        let bucket_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO accounts (id, pearl_account_id) VALUES (?, ?)")
            .bind(&account_id)
            .bind(pearl_account_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO buckets (id, account_id, name) VALUES (?, ?, ?)")
            .bind(&bucket_id)
            .bind(&account_id)
            .bind("test-bucket")
            .execute(pool)
            .await
            .unwrap();
        (account_id, bucket_id)
    }

    #[tokio::test]
    async fn get_expiring_blobs_with_accounts_returns_pearl_account() {
        let pool = test_pool().await;
        let (account_id, bucket_id) = seed_account_with_pearl(&pool, "pearl-123").await;

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
        assert_eq!(blobs[0].pearl_account_id, "pearl-123");
        assert_eq!(blobs[0].min_sui_balance, 0);
        assert_eq!(blobs[0].min_wal_balance, 0);
    }

    #[tokio::test]
    async fn get_expiring_blobs_with_accounts_skips_no_pearl_account() {
        let pool = test_pool().await;
        let (account_id, bucket_id) = seed_account_and_bucket(&pool).await;

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        insert_blob(
            &pool,
            "blob-hash-pa2",
            &bucket_id,
            &account_id,
            "text/plain",
            100,
            &expires_at,
            Some("0xpa2"),
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
        assert_eq!(blobs.len(), 0);
    }

    #[tokio::test]
    async fn get_expiring_blobs_with_accounts_custom_thresholds() {
        let pool = test_pool().await;
        let account_id = uuid::Uuid::new_v4().to_string();
        let bucket_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO accounts (id, pearl_account_id, min_sui_balance, min_wal_balance) VALUES (?, ?, ?, ?)",
        )
        .bind(&account_id)
        .bind("pearl-thresh")
        .bind(1_000_000i64)
        .bind(500_000i64)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO buckets (id, account_id, name) VALUES (?, ?, ?)")
            .bind(&bucket_id)
            .bind(&account_id)
            .bind("test-bucket")
            .execute(&pool)
            .await
            .unwrap();

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        insert_blob(
            &pool,
            "blob-hash-thresh",
            &bucket_id,
            &account_id,
            "text/plain",
            100,
            &expires_at,
            Some("0xthresh1"),
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
        assert_eq!(blobs[0].pearl_account_id, "pearl-thresh");
        assert_eq!(blobs[0].min_sui_balance, 1_000_000);
        assert_eq!(blobs[0].min_wal_balance, 500_000);
    }

    #[tokio::test]
    async fn get_expiring_blobs_with_accounts_mixed() {
        let pool = test_pool().await;
        // Account with pearl
        let (acct_with, bucket_with) = seed_account_with_pearl(&pool, "pearl-mixed").await;
        // Account without pearl
        let (acct_without, bucket_without) = seed_account_and_bucket(&pool).await;

        let expires_at = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(3))
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        insert_blob(
            &pool,
            "blob-hash-mixed1",
            &bucket_with,
            &acct_with,
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
            &bucket_without,
            &acct_without,
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
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].sui_object_id, "0xmixed1");
        assert_eq!(blobs[0].pearl_account_id, "pearl-mixed");
        assert_eq!(blobs[0].min_sui_balance, 0);
        assert_eq!(blobs[0].min_wal_balance, 0);
    }
}
