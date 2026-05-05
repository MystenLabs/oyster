use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{
    AccountId,
    AppId,
    models::{Account, AccountSummary},
};

/// Format used to encode `DateTime<Utc>` values when binding to TEXT columns.
/// Matches the format used by `datetime('now')` (SQLite) and the Postgres
/// `to_char(... 'YYYY-MM-DD HH24:MI:SS')` defaults on the `accounts` table —
/// same width and ordering, so lexicographic comparison is monotonic.
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

fn ts(t: DateTime<Utc>) -> String {
    t.format(TIMESTAMP_FORMAT).to_string()
}

/// Minimal account-level pool state needed by the extension task to schedule
/// an `extend_storage_pool` PTB.
#[derive(Debug, Clone)]
pub struct ExpiringPool {
    /// Oyster account ID that owns the pool.
    pub account_id: AccountId,
    /// Owning app — used to look up the per-app `webhook_url`.
    pub app_id: AppId,
    /// On-chain Sui `ObjectID` of the `StoragePool`.
    pub storage_pool_object_id: String,
    /// Epoch at which the current reservation expires.
    pub pool_end_epoch: i64,
    /// Total encoded bytes reserved on the pool.
    pub pool_reserved_encoded_bytes: i64,
}

/// Insert a new account belonging to the given app.
pub async fn create_account(
    pool: &super::DbPool,
    app_id: &AppId,
    name: Option<&str>,
) -> Result<Account, sqlx::Error> {
    let id = AccountId::new();
    let name = name.map_or_else(|| id.to_string(), |n| n.to_string());
    let row = sqlx::query(&super::sql(
        "INSERT INTO accounts (id, app_id, name) VALUES (?, ?, ?) RETURNING id, app_id, name, created_at, updated_at",
    ))
    .bind(&id)
    .bind(app_id)
    .bind(&name)
    .fetch_one(pool)
    .await?;

    Ok(Account {
        id: row.get("id"),
        app_id: row.get("app_id"),
        name: row.get("name"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

/// Fetch an account by ID, returning `None` if it does not exist.
pub async fn get_account(
    pool: &super::DbPool,
    id: &AccountId,
) -> Result<Option<Account>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT id, app_id, name, created_at, updated_at FROM accounts WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| Account {
        id: r.get("id"),
        app_id: r.get("app_id"),
        name: r.get("name"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }))
}

/// Count the total number of accounts.
pub async fn count_accounts(pool: &super::DbPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&super::sql("SELECT COUNT(*) FROM accounts"))
        .fetch_one(pool)
        .await
}

/// Persistent per-account `StoragePool` accounting state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePoolState {
    /// On-chain Sui `ObjectID` of the pool.
    pub object_id: String,
    /// Epoch at which the current reservation expires.
    pub end_epoch: i64,
    /// Total encoded bytes reserved on the pool.
    pub reserved_encoded_bytes: i64,
    /// Encoded bytes currently consumed by registered blobs.
    pub used_encoded_bytes: i64,
}

/// Persist a freshly-created `StoragePool` for an account. Idempotent: only
/// populates the columns when the pool is currently `NULL` (first writer
/// wins); returns `true` on first-writer win, `false` on race loss.
pub async fn set_storage_pool(
    pool: &super::DbPool,
    account_id: &AccountId,
    object_id: &str,
    end_epoch: i64,
    reserved_encoded_bytes: i64,
    used_encoded_bytes: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "UPDATE accounts SET \
             storage_pool_object_id = ?, \
             pool_end_epoch = ?, \
             pool_reserved_encoded_bytes = ?, \
             pool_used_encoded_bytes = ? \
         WHERE id = ? AND storage_pool_object_id IS NULL",
    ))
    .bind(object_id)
    .bind(end_epoch)
    .bind(reserved_encoded_bytes)
    .bind(used_encoded_bytes)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Fetch the current `StoragePool` state for an account, or `None` if the
/// account hasn't lazy-created a pool yet (or doesn't exist).
pub async fn get_storage_pool(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<Option<StoragePoolState>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT storage_pool_object_id, pool_end_epoch, \
                pool_reserved_encoded_bytes, pool_used_encoded_bytes \
         FROM accounts WHERE id = ?",
    ))
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let object_id: Option<String> = row.get("storage_pool_object_id");
    let end_epoch: Option<i64> = row.get("pool_end_epoch");
    let reserved: Option<i64> = row.get("pool_reserved_encoded_bytes");
    let used: Option<i64> = row.get("pool_used_encoded_bytes");
    match (object_id, end_epoch, reserved, used) {
        (Some(object_id), Some(end_epoch), Some(reserved), Some(used)) => {
            Ok(Some(StoragePoolState {
                object_id,
                end_epoch,
                reserved_encoded_bytes: reserved,
                used_encoded_bytes: used,
            }))
        }
        _ => Ok(None),
    }
}

/// Bump reserved + used byte counters after a successful `register_pooled_blob`
/// transaction. `added_reserved_bytes` may be zero when the existing
/// reservation already covered the new blob.
pub async fn update_pool_after_register(
    pool: &super::DbPool,
    account_id: &AccountId,
    added_reserved_bytes: i64,
    added_used_bytes: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "UPDATE accounts SET \
             pool_reserved_encoded_bytes = pool_reserved_encoded_bytes + ?, \
             pool_used_encoded_bytes = pool_used_encoded_bytes + ? \
         WHERE id = ?",
    ))
    .bind(added_reserved_bytes)
    .bind(added_used_bytes)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Atomically claim a batch of pools that need extension and stamp them with
/// `extend_attempt_after = claim_until`. A row is claimable when:
///
/// * it has a `StoragePool` (i.e. `storage_pool_object_id IS NOT NULL`); and
/// * `pool_end_epoch < cutoff_epoch`; and
/// * `extend_attempt_after IS NULL OR extend_attempt_after <= now`.
///
/// The single `UPDATE … RETURNING` statement is atomic per row on both SQLite
/// (writer lock serializes claims) and Postgres (the implicit row lock taken
/// by `UPDATE` covers the read-after-claim race). Two concurrent callers
/// observe disjoint result sets — the second observes the freshly-stamped
/// `extend_attempt_after` from the first and skips those rows.
pub async fn claim_pools_for_extension(
    pool: &super::DbPool,
    cutoff_epoch: i64,
    limit: i64,
    claim_until: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Vec<ExpiringPool>, sqlx::Error> {
    let rows = sqlx::query(&super::sql(
        "UPDATE accounts SET extend_attempt_after = ? \
         WHERE id IN ( \
             SELECT id FROM accounts \
             WHERE storage_pool_object_id IS NOT NULL \
               AND pool_end_epoch IS NOT NULL \
               AND pool_end_epoch < ? \
               AND (extend_attempt_after IS NULL OR extend_attempt_after <= ?) \
             ORDER BY pool_end_epoch \
             LIMIT ? \
         ) \
         RETURNING id, app_id, storage_pool_object_id, pool_end_epoch, \
                   pool_reserved_encoded_bytes",
    ))
    .bind(ts(claim_until))
    .bind(cutoff_epoch)
    .bind(ts(now))
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| ExpiringPool {
            account_id: r.get("id"),
            app_id: r.get("app_id"),
            storage_pool_object_id: r.get("storage_pool_object_id"),
            pool_end_epoch: r.get("pool_end_epoch"),
            pool_reserved_encoded_bytes: r.get("pool_reserved_encoded_bytes"),
        })
        .collect())
}

/// Look up the per-app `webhook_url` for each app in `app_ids`. Apps without
/// a webhook configured map to `None`; missing apps are simply absent from
/// the map.
pub async fn fetch_webhook_urls_for_apps(
    pool: &super::DbPool,
    app_ids: &[AppId],
) -> Result<HashMap<AppId, Option<String>>, sqlx::Error> {
    if app_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = std::iter::repeat_n("?", app_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let raw = format!("SELECT id, webhook_url FROM apps WHERE id IN ({placeholders})");
    let q = super::sql(&raw);
    let mut query = sqlx::query(&q);
    for id in app_ids {
        query = query.bind(id);
    }
    let rows = query.fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<AppId, _>("id"),
                r.get::<Option<String>, _>("webhook_url"),
            )
        })
        .collect())
}

/// List one-row summaries of all accounts belonging to `app_id`, including
/// the count of active (non-revoked) API keys per account. The count is
/// computed in a single LEFT JOIN + GROUP BY to avoid N+1 round-trips.
pub async fn list_account_summaries_by_app(
    pool: &super::DbPool,
    app_id: &AppId,
) -> Result<Vec<AccountSummary>, sqlx::Error> {
    let rows = sqlx::query(&super::sql(
        "SELECT acc.id, acc.name, acc.created_at, \
                CAST(COALESCE(SUM(CASE WHEN ak.id IS NOT NULL AND ak.revoked_at IS NULL THEN 1 ELSE 0 END), 0) AS BIGINT) AS active_count \
         FROM accounts acc \
         LEFT JOIN api_keys ak ON ak.account_id = acc.id \
         WHERE acc.app_id = ? \
         GROUP BY acc.id, acc.name, acc.created_at \
         ORDER BY acc.created_at",
    ))
    .bind(app_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| AccountSummary {
            id: r.get("id"),
            name: r.get("name"),
            created_at: r.get("created_at"),
            active_api_key_count: r.get::<i64, _>("active_count"),
        })
        .collect())
}

/// Bump only `pool_end_epoch` after a successful `extend_storage_pool` PTB.
/// Leaves reserved/used byte counters untouched.
pub async fn bump_pool_end_epoch(
    pool: &super::DbPool,
    account_id: &AccountId,
    new_end_epoch: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "UPDATE accounts SET pool_end_epoch = ? WHERE id = ? AND COALESCE(pool_end_epoch, 0) < ?",
    ))
    .bind(new_end_epoch)
    .bind(account_id)
    .bind(new_end_epoch)
    .execute(pool)
    .await?;
    Ok(())
}

/// Decrement the used-byte counter after a successful `delete_pooled_blob`.
pub async fn update_pool_after_delete(
    pool: &super::DbPool,
    account_id: &AccountId,
    freed_used_bytes: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "UPDATE accounts SET \
             pool_used_encoded_bytes = pool_used_encoded_bytes - ? \
         WHERE id = ?",
    ))
    .bind(freed_used_bytes)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn test_pool() -> super::super::DbPool {
        db::create_pool("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn create_account_works() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        assert_eq!(account.name, account.id.to_string());
        assert_eq!(account.app_id, AppId::INTERNAL);
    }

    #[tokio::test]
    async fn create_account_with_name() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, Some("my-account"))
            .await
            .unwrap();
        assert_eq!(account.name, "my-account");
        assert_eq!(account.app_id, AppId::INTERNAL);
    }

    #[tokio::test]
    async fn get_account_returns_created() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let fetched = get_account(&pool, &account.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, account.id);
        assert_eq!(fetched.app_id, AppId::INTERNAL);
    }

    #[tokio::test]
    async fn get_account_returns_none_for_missing() {
        let pool = test_pool().await;
        let result = get_account(&pool, &AccountId::new()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_storage_pool_is_none_for_fresh_account() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let result = get_storage_pool(&pool, &account.id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_storage_pool_is_none_for_missing_account() {
        let pool = test_pool().await;
        let result = get_storage_pool(&pool, &AccountId::new()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn set_storage_pool_sets_full_state() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let updated = set_storage_pool(&pool, &account.id, "0xabc", 42, 1_000, 0)
            .await
            .unwrap();
        assert!(updated);
        let fetched = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(fetched.object_id, "0xabc");
        assert_eq!(fetched.end_epoch, 42);
        assert_eq!(fetched.reserved_encoded_bytes, 1_000);
        assert_eq!(fetched.used_encoded_bytes, 0);
    }

    #[tokio::test]
    async fn set_storage_pool_is_idempotent() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let first = set_storage_pool(&pool, &account.id, "0xabc", 42, 1_000, 0)
            .await
            .unwrap();
        assert!(first);
        let second = set_storage_pool(&pool, &account.id, "0xdef", 99, 5_000, 50)
            .await
            .unwrap();
        assert!(!second);
        let fetched = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(fetched.object_id, "0xabc");
        assert_eq!(fetched.end_epoch, 42);
        assert_eq!(fetched.reserved_encoded_bytes, 1_000);
        assert_eq!(fetched.used_encoded_bytes, 0);
    }

    #[tokio::test]
    async fn set_storage_pool_noop_for_missing_account() {
        let pool = test_pool().await;
        let updated = set_storage_pool(&pool, &AccountId::new(), "0xabc", 42, 1_000, 0)
            .await
            .unwrap();
        assert!(!updated);
    }

    #[tokio::test]
    async fn update_pool_after_register_bumps_both() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        set_storage_pool(&pool, &account.id, "0xabc", 42, 1_000, 0)
            .await
            .unwrap();
        update_pool_after_register(&pool, &account.id, 500, 300)
            .await
            .unwrap();
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.reserved_encoded_bytes, 1_500);
        assert_eq!(state.used_encoded_bytes, 300);
    }

    #[tokio::test]
    async fn update_pool_after_delete_decrements_used() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        set_storage_pool(&pool, &account.id, "0xabc", 42, 1_000, 400)
            .await
            .unwrap();
        update_pool_after_delete(&pool, &account.id, 250)
            .await
            .unwrap();
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.reserved_encoded_bytes, 1_000);
        assert_eq!(state.used_encoded_bytes, 150);
    }

    fn now_at(secs: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[tokio::test]
    async fn claim_pools_for_extension_filters_by_cutoff() {
        let pool = test_pool().await;
        let a = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let b = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        set_storage_pool(&pool, &a.id, "0xaaa", 10, 1_000, 0)
            .await
            .unwrap();
        set_storage_pool(&pool, &b.id, "0xbbb", 50, 1_000, 0)
            .await
            .unwrap();

        let now = now_at(0);
        let claim_until = now_at(60);
        let results = claim_pools_for_extension(&pool, 20, 100, claim_until, now)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].account_id, a.id);
        assert_eq!(results[0].app_id, AppId::INTERNAL);
        assert_eq!(results[0].storage_pool_object_id, "0xaaa");
        assert_eq!(results[0].pool_end_epoch, 10);
        assert_eq!(results[0].pool_reserved_encoded_bytes, 1_000);
    }

    #[tokio::test]
    async fn claim_pools_for_extension_skips_accounts_without_pool() {
        let pool = test_pool().await;
        let _unpooled = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let pooled = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        set_storage_pool(&pool, &pooled.id, "0xccc", 5, 1_000, 0)
            .await
            .unwrap();

        let results = claim_pools_for_extension(&pool, 100, 100, now_at(60), now_at(0))
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].account_id, pooled.id);
    }

    #[tokio::test]
    async fn claim_pools_for_extension_skips_unexpired_ttl() {
        let pool = test_pool().await;
        let a = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        set_storage_pool(&pool, &a.id, "0xaaa", 10, 1_000, 0)
            .await
            .unwrap();

        // First claim stamps extend_attempt_after = now+60.
        let first = claim_pools_for_extension(&pool, 100, 100, now_at(60), now_at(0))
            .await
            .unwrap();
        assert_eq!(first.len(), 1);

        // Second call at now=30 — TTL still in the future, row should be skipped.
        let second = claim_pools_for_extension(&pool, 100, 100, now_at(90), now_at(30))
            .await
            .unwrap();
        assert!(second.is_empty());

        // Third call at now=120 — TTL has expired, row is re-claimable and re-stamped.
        let third = claim_pools_for_extension(&pool, 100, 100, now_at(180), now_at(120))
            .await
            .unwrap();
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].account_id, a.id);
    }

    #[tokio::test]
    async fn claim_pools_for_extension_lifts_row_after_bump() {
        let pool = test_pool().await;
        let a = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        set_storage_pool(&pool, &a.id, "0xaaa", 10, 1_000, 0)
            .await
            .unwrap();

        let first = claim_pools_for_extension(&pool, 20, 100, now_at(60), now_at(0))
            .await
            .unwrap();
        assert_eq!(first.len(), 1);

        // Simulate successful extend: pool_end_epoch jumps past the cutoff.
        bump_pool_end_epoch(&pool, &a.id, 50).await.unwrap();

        // Even with a fully-expired claim TTL, the row no longer matches the cutoff.
        let second = claim_pools_for_extension(&pool, 20, 100, now_at(180), now_at(120))
            .await
            .unwrap();
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn claim_pools_for_extension_concurrent_callers_disjoint() {
        let pool = test_pool().await;
        // Insert several expiring pools.
        let mut ids = Vec::new();
        for i in 0..6 {
            let acc = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
            set_storage_pool(&pool, &acc.id, &format!("0x{i:03}"), 10, 1_000, 0)
                .await
                .unwrap();
            ids.push(acc.id);
        }

        let p1 = pool.clone();
        let p2 = pool.clone();
        let now = now_at(0);
        let until = now_at(60);
        let (r1, r2) = tokio::join!(
            tokio::spawn(async move {
                claim_pools_for_extension(&p1, 100, 10, until, now)
                    .await
                    .unwrap()
            }),
            tokio::spawn(async move {
                claim_pools_for_extension(&p2, 100, 10, until, now)
                    .await
                    .unwrap()
            }),
        );
        let r1 = r1.unwrap();
        let r2 = r2.unwrap();

        // Together cover every row, with no overlap.
        let mut seen: std::collections::HashSet<AccountId> =
            r1.iter().map(|p| p.account_id).collect();
        for p in &r2 {
            assert!(
                seen.insert(p.account_id),
                "overlapping claim for {}",
                p.account_id
            );
        }
        assert_eq!(seen.len(), ids.len());
    }

    #[tokio::test]
    async fn fetch_webhook_urls_for_apps_returns_map() {
        let pool = test_pool().await;
        let with_hook = crate::db::apps::create_app(
            &pool,
            "with-hook",
            "wh@example.com",
            Some("https://example.com/hook"),
        )
        .await
        .unwrap();
        let without_hook = crate::db::apps::create_app(&pool, "no-hook", "n@example.com", None)
            .await
            .unwrap();

        let map = fetch_webhook_urls_for_apps(&pool, &[with_hook.id, without_hook.id])
            .await
            .unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(&with_hook.id).unwrap().as_deref(),
            Some("https://example.com/hook")
        );
        assert!(map.get(&without_hook.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn fetch_webhook_urls_for_apps_handles_empty_input() {
        let pool = test_pool().await;
        let map = fetch_webhook_urls_for_apps(&pool, &[]).await.unwrap();
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn bump_pool_end_epoch_updates_only_end_epoch() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        set_storage_pool(&pool, &account.id, "0xabc", 42, 1_000, 400)
            .await
            .unwrap();

        bump_pool_end_epoch(&pool, &account.id, 99).await.unwrap();

        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.end_epoch, 99);
        assert_eq!(state.reserved_encoded_bytes, 1_000);
        assert_eq!(state.used_encoded_bytes, 400);
    }

    #[tokio::test]
    async fn bump_pool_end_epoch_does_not_regress() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        set_storage_pool(&pool, &account.id, "0xabc", 100, 1_000, 400)
            .await
            .unwrap();

        // Stale/smaller new_end_epoch must be a no-op.
        bump_pool_end_epoch(&pool, &account.id, 50).await.unwrap();
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.end_epoch, 100);

        // Equal new_end_epoch must also be a no-op (strict >).
        bump_pool_end_epoch(&pool, &account.id, 100).await.unwrap();
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.end_epoch, 100);

        // Strictly greater value still applies.
        bump_pool_end_epoch(&pool, &account.id, 101).await.unwrap();
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.end_epoch, 101);
    }

    async fn mint_key(pool: &super::super::DbPool, account_id: &AccountId, note: &str) -> String {
        let raw = crate::auth::generate_api_key();
        let hash = crate::auth::hash_api_key(&raw);
        let prefix = crate::auth::key_prefix(&raw);
        let k = crate::db::api_keys::create_api_key(pool, account_id, &hash, &prefix, &raw, note)
            .await
            .unwrap();
        k.id
    }

    #[tokio::test]
    async fn list_account_summaries_by_app_returns_zero_count_for_no_keys() {
        let pool = test_pool().await;
        let acc = create_account(&pool, &AppId::INTERNAL, Some("alpha"))
            .await
            .unwrap();

        let summaries = list_account_summaries_by_app(&pool, &AppId::INTERNAL)
            .await
            .unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, acc.id);
        assert_eq!(summaries[0].name, "alpha");
        assert_eq!(summaries[0].active_api_key_count, 0);
    }

    #[tokio::test]
    async fn list_account_summaries_by_app_counts_active_keys() {
        let pool = test_pool().await;
        let acc = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        mint_key(&pool, &acc.id, "api").await;
        mint_key(&pool, &acc.id, "api").await;

        let summaries = list_account_summaries_by_app(&pool, &AppId::INTERNAL)
            .await
            .unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].active_api_key_count, 2);
    }

    #[tokio::test]
    async fn list_account_summaries_by_app_excludes_revoked_keys() {
        let pool = test_pool().await;
        let acc = create_account(&pool, &AppId::INTERNAL, None).await.unwrap();
        let id1 = mint_key(&pool, &acc.id, "api").await;
        mint_key(&pool, &acc.id, "api").await;

        crate::db::api_keys::revoke_api_key(&pool, &id1, &acc.id)
            .await
            .unwrap();

        let summaries = list_account_summaries_by_app(&pool, &AppId::INTERNAL)
            .await
            .unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].active_api_key_count, 1);
    }

    #[tokio::test]
    async fn list_account_summaries_by_app_filters_by_app_id() {
        let pool = test_pool().await;
        let app_a = crate::db::apps::create_app(&pool, "app-a", "a@example.com", None)
            .await
            .unwrap();
        let app_b = crate::db::apps::create_app(&pool, "app-b", "b@example.com", None)
            .await
            .unwrap();
        let acc_a = create_account(&pool, &app_a.id, Some("for-a"))
            .await
            .unwrap();
        let _acc_b = create_account(&pool, &app_b.id, Some("for-b"))
            .await
            .unwrap();

        let summaries = list_account_summaries_by_app(&pool, &app_a.id)
            .await
            .unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, acc_a.id);
        assert_eq!(summaries[0].name, "for-a");
    }
}
