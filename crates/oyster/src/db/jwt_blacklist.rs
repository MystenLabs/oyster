use sqlx::Row;

/// Add a JTI to the blacklist.
pub async fn blacklist_jti(
    pool: &super::DbPool,
    jti: &str,
    not_after: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "INSERT INTO jwt_blacklist (jti, not_after) VALUES (?, ?)",
    ))
    .bind(jti)
    .bind(not_after)
    .execute(pool)
    .await?;
    Ok(())
}

/// Check whether a JTI has been blacklisted.
pub async fn is_blacklisted(pool: &super::DbPool, jti: &str) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT COUNT(*) as cnt FROM jwt_blacklist WHERE jti = ?",
    ))
    .bind(jti)
    .fetch_one(pool)
    .await?;
    let count: i64 = row.get("cnt");
    Ok(count > 0)
}

/// Delete expired blacklist entries (where `not_after` is in the past).
pub async fn gc_expired(pool: &super::DbPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "DELETE FROM jwt_blacklist WHERE not_after < datetime('now')",
    ))
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> super::super::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn blacklist_and_check() {
        let pool = test_pool().await;
        assert!(!is_blacklisted(&pool, "jti-1").await.unwrap());
        blacklist_jti(&pool, "jti-1", "2099-01-01 00:00:00")
            .await
            .unwrap();
        assert!(is_blacklisted(&pool, "jti-1").await.unwrap());
        assert!(!is_blacklisted(&pool, "jti-2").await.unwrap());
    }

    #[tokio::test]
    async fn gc_removes_expired() {
        let pool = test_pool().await;
        // Insert an already-expired entry.
        blacklist_jti(&pool, "old", "2000-01-01 00:00:00")
            .await
            .unwrap();
        // Insert a future entry.
        blacklist_jti(&pool, "future", "2099-01-01 00:00:00")
            .await
            .unwrap();
        let removed = gc_expired(&pool).await.unwrap();
        assert_eq!(removed, 1);
        assert!(!is_blacklisted(&pool, "old").await.unwrap());
        assert!(is_blacklisted(&pool, "future").await.unwrap());
    }
}
