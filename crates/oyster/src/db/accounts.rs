use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{
    AccountId, AppId,
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
    /// Consecutive failed extension attempts so far (drives retry backoff).
    pub extend_failure_count: i64,
    /// Pearl master-seed version the account's wallet key derives from.
    pub key_version: u32,
}

/// Insert a new account belonging to the given app. When
/// `max_unencoded_bytes` is `None`, the DB falls back to its
/// `DEFAULT 5_000_000_000` for the column. When `avg_blob_size` is
/// `None`, the DB falls back to `0` (the no-inflation sentinel) — the
/// route layer is responsible for resolving the global 10 MB default
/// (`OYSTER_DEFAULT_AVG_BLOB_SIZE`) into `Some(..)` for production
/// account creation, so DB-layer callers (and tests) passing `None`
/// get today's upper-bound behavior. `key_version` records which Pearl
/// master-seed version the account's wallet derives from; `None` falls
/// back to 1 — production account creation passes Pearl's active version.
pub async fn create_account(
    pool: &super::DbPool,
    app_id: &AppId,
    name: Option<&str>,
    max_unencoded_bytes: Option<i64>,
    avg_blob_size: Option<i64>,
    key_version: Option<u32>,
) -> Result<Account, sqlx::Error> {
    let id = AccountId::new();
    let name = name.map_or_else(|| id.to_string(), |n| n.to_string());
    let row = sqlx::query(&super::sql(
        "INSERT INTO accounts (id, app_id, name, max_unencoded_bytes, avg_blob_size, key_version) \
         VALUES (?, ?, ?, COALESCE(?, 5000000000), COALESCE(?, 0), COALESCE(?, 1)) \
         RETURNING id, app_id, name, max_unencoded_bytes, avg_blob_size, created_at, updated_at",
    ))
    .bind(&id)
    .bind(app_id)
    .bind(&name)
    .bind(max_unencoded_bytes)
    .bind(avg_blob_size)
    .bind(key_version.map(i64::from))
    .fetch_one(pool)
    .await?;

    Ok(Account {
        id: row.get("id"),
        app_id: row.get("app_id"),
        name: row.get("name"),
        max_unencoded_bytes: row.get("max_unencoded_bytes"),
        avg_blob_size: row.get("avg_blob_size"),
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
        "SELECT id, app_id, name, max_unencoded_bytes, avg_blob_size, created_at, updated_at \
         FROM accounts WHERE id = ?",
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| Account {
        id: r.get("id"),
        app_id: r.get("app_id"),
        name: r.get("name"),
        max_unencoded_bytes: r.get("max_unencoded_bytes"),
        avg_blob_size: r.get("avg_blob_size"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }))
}

/// Lightweight single-column read for the upload hot path. Returns
/// `None` if the account does not exist.
pub async fn get_max_unencoded_bytes(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<Option<i64>, sqlx::Error> {
    let value: Option<i64> = sqlx::query_scalar(&super::sql(
        "SELECT max_unencoded_bytes FROM accounts WHERE id = ?",
    ))
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(value)
}

/// Lightweight two-column read for the upload hot path: the storage cap
/// plus the `avg_blob_size` inflation knob. Returns `None` if the
/// account does not exist. Used by `DirectWalrusBlobStore` so the cap
/// check needs only one round-trip for both values.
pub async fn get_cap_and_avg_blob_size(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<Option<(i64, i64)>, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT max_unencoded_bytes, avg_blob_size FROM accounts WHERE id = ?",
    ))
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.get("max_unencoded_bytes"), r.get("avg_blob_size"))))
}

/// Pearl master-seed version the account's wallet key derives from.
/// Returns `None` if the account does not exist. Callers pass this on
/// every Pearl `get_address`/`sign_transaction` call so the account
/// keeps signing with its original key across seed rotations.
pub async fn get_key_version(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<Option<u32>, sqlx::Error> {
    let value: Option<i64> =
        sqlx::query_scalar(&super::sql("SELECT key_version FROM accounts WHERE id = ?"))
            .bind(account_id)
            .fetch_optional(pool)
            .await?;
    Ok(value.map(|v| v as u32))
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
                   pool_reserved_encoded_bytes, extend_failure_count, key_version",
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
            extend_failure_count: r.get("extend_failure_count"),
            key_version: r.get::<i64, _>("key_version") as u32,
        })
        .collect())
}

/// Fleet-wide pool health snapshot, sampled once per extension cycle to
/// feed the worker's gauges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolHealthStats {
    /// Accounts with a pool whose most recent extension attempt failed
    /// (`extend_failure_count > 0`) — pools currently in backoff.
    pub pools_in_backoff: i64,
    /// Highest consecutive-failure count across all pools (0 when none).
    pub max_failure_count: i64,
    /// Lowest `pool_end_epoch` across accounts that have a pool; `None`
    /// when no account has a pool yet.
    pub min_pool_end_epoch: Option<i64>,
}

/// One aggregate query over accounts that own a `StoragePool`. Uses
/// `COUNT(CASE …)` rather than `SUM` so the result is a plain integer on
/// both backends (Postgres `SUM(bigint)` is `numeric`).
pub async fn pool_health_stats(pool: &super::DbPool) -> Result<PoolHealthStats, sqlx::Error> {
    let row = sqlx::query(&super::sql(
        "SELECT \
             COUNT(CASE WHEN extend_failure_count > 0 THEN 1 END) AS pools_in_backoff, \
             MAX(extend_failure_count) AS max_failure_count, \
             MIN(pool_end_epoch) AS min_pool_end_epoch \
         FROM accounts \
         WHERE storage_pool_object_id IS NOT NULL",
    ))
    .fetch_one(pool)
    .await?;
    Ok(PoolHealthStats {
        pools_in_backoff: row.get("pools_in_backoff"),
        max_failure_count: row.get::<Option<i64>, _>("max_failure_count").unwrap_or(0),
        min_pool_end_epoch: row.get("min_pool_end_epoch"),
    })
}

/// Per-app webhook configuration: URL plus the base64-encoded Ed25519
/// keypair used to sign deliveries. All three fields are populated together
/// or all NULL together — see migration 017.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppWebhook {
    /// Receiver URL.
    pub url: String,
    /// Base64-encoded 32-byte public key.
    pub public_key_b64: String,
    /// Base64-encoded 32-byte signing-key seed.
    pub private_key_b64: String,
}

/// Look up the per-app webhook configuration for each app in `app_ids`. Apps
/// without a webhook configured map to `None`; missing apps are absent from
/// the map.
pub async fn fetch_webhook_for_apps(
    pool: &super::DbPool,
    app_ids: &[AppId],
) -> Result<HashMap<AppId, Option<AppWebhook>>, sqlx::Error> {
    if app_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = std::iter::repeat_n("?", app_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let raw = format!(
        "SELECT id, webhook_url, webhook_public_key, webhook_private_key \
         FROM apps WHERE id IN ({placeholders})"
    );
    let q = super::sql(&raw);
    let mut query = sqlx::query(&q);
    for id in app_ids {
        query = query.bind(id);
    }
    let rows = query.fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let id: AppId = r.get("id");
            let url: Option<String> = r.get("webhook_url");
            let pubk: Option<String> = r.get("webhook_public_key");
            let privk: Option<String> = r.get("webhook_private_key");
            let wh = match (url, pubk, privk) {
                (Some(url), Some(public_key_b64), Some(private_key_b64)) => Some(AppWebhook {
                    url,
                    public_key_b64,
                    private_key_b64,
                }),
                _ => None,
            };
            (id, wh)
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

/// Update the per-account `max_unencoded_bytes` cap. Returns `true`
/// when a row was matched (i.e. the account exists), `false` otherwise.
/// Single-column UPDATE — does not touch on-chain pool counters; the
/// admin route is responsible for keeping the on-chain reservation in
/// sync.
pub async fn set_max_unencoded_bytes(
    pool: &super::DbPool,
    account_id: &AccountId,
    new_cap: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "UPDATE accounts SET max_unencoded_bytes = ? WHERE id = ?",
    ))
    .bind(new_cap)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Update both the per-account `max_unencoded_bytes` cap and the
/// `avg_blob_size` inflation knob in one statement. Returns `true` when
/// a row was matched. Used by `update_max_storage` so the cap and its
/// matching inflation knob are persisted together after the on-chain
/// shrink (if any) succeeds — keeping the admission gate and the
/// orphan/shrink threshold derived from the same `(cap, avg)` pair.
pub async fn set_max_unencoded_and_avg_blob_size(
    pool: &super::DbPool,
    account_id: &AccountId,
    new_cap: i64,
    new_avg_blob_size: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "UPDATE accounts SET max_unencoded_bytes = ?, avg_blob_size = ? WHERE id = ?",
    ))
    .bind(new_cap)
    .bind(new_avg_blob_size)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Bump only `pool_end_epoch` after a successful `extend_storage_pool` PTB
/// (or a stale-DB repair from the on-chain value). Leaves reserved/used
/// byte counters untouched. Also clears `extend_failure_count`: a forward
/// move of the end epoch means the pool is healthy, so the retry backoff
/// restarts from its base on the next shortfall.
///
/// Guarded on `storage_pool_object_id` so an epoch observed on one pool
/// object is never stamped onto a replacement pool that swapped in while
/// the row was claimed (e.g. an expired-pool reset followed by a lazy
/// re-create). Returns whether a row was updated: `false` when the pool
/// changed underneath us or the DB value was already at or past
/// `new_end_epoch`.
pub async fn bump_pool_end_epoch(
    pool: &super::DbPool,
    account_id: &AccountId,
    storage_pool_object_id: &str,
    new_end_epoch: i64,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "UPDATE accounts SET pool_end_epoch = ?, extend_failure_count = 0 \
         WHERE id = ? AND storage_pool_object_id = ? \
           AND COALESCE(pool_end_epoch, 0) < ?",
    ))
    .bind(new_end_epoch)
    .bind(account_id)
    .bind(storage_pool_object_id)
    .bind(new_end_epoch)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Record a failed extension attempt: increment the consecutive-failure
/// counter and push `extend_attempt_after` out to `next_attempt_after`
/// (the exponential-backoff stamp computed by the extension task). The
/// stamp only ever moves forward relative to the claim-time cooldown, so
/// a concurrent claim cannot shorten the backoff.
pub async fn record_extension_failure(
    pool: &super::DbPool,
    account_id: &AccountId,
    next_attempt_after: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "UPDATE accounts SET \
             extend_failure_count = extend_failure_count + 1, \
             extend_attempt_after = ? \
         WHERE id = ?",
    ))
    .bind(ts(next_attempt_after))
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Overwrite `pool_reserved_encoded_bytes` and `pool_used_encoded_bytes`
/// to match an authoritative on-chain reading. Used to reconcile DB
/// drift after a `register_pooled_blobs` PTB aborts with
/// `EInsufficientCapacity` — the on-chain `StoragePoolInnerV1` is the
/// source of truth in that case.
pub async fn reconcile_pool_after_drift(
    pool: &super::DbPool,
    account_id: &AccountId,
    reserved_encoded_bytes: i64,
    used_encoded_bytes: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(&super::sql(
        "UPDATE accounts SET \
             pool_reserved_encoded_bytes = ?, \
             pool_used_encoded_bytes = ? \
         WHERE id = ?",
    ))
    .bind(reserved_encoded_bytes)
    .bind(used_encoded_bytes)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// User-requested extension retry: clear the backoff stamp and failure
/// count so the extension task claims the pool on its next cycle instead
/// of waiting out the exponential backoff. Intended for the
/// `POST /account/extend` endpoint after the user funds their wallet.
/// Deliberately touches no chain state — all extension work stays in the
/// single-writer background task, and the claim-time cooldown still
/// bounds how often repeated calls can trigger real attempts. Returns
/// `false` when the account has no `StoragePool` to extend.
pub async fn request_extension_retry(
    pool: &super::DbPool,
    account_id: &AccountId,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(&super::sql(
        "UPDATE accounts SET extend_attempt_after = NULL, extend_failure_count = 0 \
         WHERE id = ? AND storage_pool_object_id IS NOT NULL",
    ))
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Audit event type recorded when an expired `StoragePool` is reset.
pub const AUDIT_EVENT_POOL_EXPIRED: &str = "account.pool_expired";

/// Reset an account whose `StoragePool` has expired on-chain (Walrus
/// storage cannot be extended past its end epoch). In one transaction:
///
/// * null out the pool columns so the account lazy-creates a fresh pool
///   on its next funded upload, and so the extension task permanently
///   stops claiming the row (`storage_pool_object_id IS NOT NULL`);
/// * delete the account's blob rows (their Walrus storage lapsed with
///   the pool; `blob_tags` cascade via the FK); and
/// * record an `account.pool_expired` audit event carrying
///   `event_data` plus the deleted-blob count, so support can answer
///   "where did my data go" after the fact.
///
/// Guarded on `storage_pool_object_id` still matching
/// `expected_pool_object_id`: if a concurrent writer replaced the pool,
/// nothing is touched and `None` is returned. On success returns the
/// number of blob rows deleted.
pub async fn reset_expired_pool(
    pool: &super::DbPool,
    account_id: &AccountId,
    app_id: &AppId,
    expected_pool_object_id: &str,
    mut event_data: serde_json::Value,
) -> Result<Option<u64>, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let reset = sqlx::query(&super::sql(
        "UPDATE accounts SET \
             storage_pool_object_id = NULL, \
             pool_end_epoch = NULL, \
             pool_reserved_encoded_bytes = NULL, \
             pool_used_encoded_bytes = NULL, \
             extend_attempt_after = NULL, \
             extend_failure_count = 0 \
         WHERE id = ? AND storage_pool_object_id = ?",
    ))
    .bind(account_id)
    .bind(expected_pool_object_id)
    .execute(&mut *tx)
    .await?;
    if reset.rows_affected() != 1 {
        tx.rollback().await?;
        return Ok(None);
    }

    let deleted = sqlx::query(&super::sql("DELETE FROM blobs WHERE account_id = ?"))
        .bind(account_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    if let Some(obj) = event_data.as_object_mut() {
        obj.insert("deleted_blob_count".into(), deleted.into());
    }
    // Mirrors `audit_events::record_audit_event`, inlined so the event
    // commits atomically with the reset (microsecond timestamp for a
    // stable `(created_at, id)` sort, matching that helper).
    let created_at = Utc::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string();
    sqlx::query(&super::sql(
        "INSERT INTO audit_events (id, app_id, actor_admin_key_id, event_type, event_data, created_at) \
         VALUES (?, ?, NULL, ?, ?, ?)",
    ))
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(app_id)
    .bind(AUDIT_EVENT_POOL_EXPIRED)
    .bind(serde_json::to_string(&event_data).expect("serialize pool_expired event_data"))
    .bind(&created_at)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Some(deleted))
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
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(account.name, account.id.to_string());
        assert_eq!(account.app_id, AppId::INTERNAL);
    }

    #[tokio::test]
    async fn create_account_defaults_key_version_to_one() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        let version = get_key_version(&pool, &account.id).await.unwrap();
        assert_eq!(version, Some(1));
    }

    #[tokio::test]
    async fn create_account_stamps_explicit_key_version() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, Some(3))
            .await
            .unwrap();
        let version = get_key_version(&pool, &account.id).await.unwrap();
        assert_eq!(version, Some(3));
    }

    #[tokio::test]
    async fn get_key_version_none_for_missing_account() {
        let pool = test_pool().await;
        let version = get_key_version(&pool, &AccountId::new()).await.unwrap();
        assert_eq!(version, None);
    }

    #[tokio::test]
    async fn create_account_defaults_max_unencoded_bytes() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(account.max_unencoded_bytes, 5_000_000_000);
    }

    #[tokio::test]
    async fn create_account_honours_explicit_max_unencoded_bytes() {
        let pool = test_pool().await;
        let account = create_account(
            &pool,
            &AppId::INTERNAL,
            None,
            Some(1_000_000_000),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(account.max_unencoded_bytes, 1_000_000_000);
    }

    #[tokio::test]
    async fn get_max_unencoded_bytes_returns_value() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, Some(7_777_777), None, None)
            .await
            .unwrap();
        let value = get_max_unencoded_bytes(&pool, &account.id).await.unwrap();
        assert_eq!(value, Some(7_777_777));
    }

    #[tokio::test]
    async fn create_account_defaults_avg_blob_size_to_sentinel() {
        // DB-layer `None` → the 0 no-inflation sentinel (the route layer
        // resolves the global default before calling).
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(account.avg_blob_size, 0);
    }

    #[tokio::test]
    async fn create_account_honours_explicit_avg_blob_size() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, Some(10_000_000), None)
            .await
            .unwrap();
        assert_eq!(account.avg_blob_size, 10_000_000);
        let fetched = get_account(&pool, &account.id).await.unwrap().unwrap();
        assert_eq!(fetched.avg_blob_size, 10_000_000);
    }

    #[tokio::test]
    async fn get_cap_and_avg_blob_size_returns_both() {
        let pool = test_pool().await;
        let account = create_account(
            &pool,
            &AppId::INTERNAL,
            None,
            Some(3_000_000),
            Some(500_000),
            None,
        )
        .await
        .unwrap();
        let pair = get_cap_and_avg_blob_size(&pool, &account.id).await.unwrap();
        assert_eq!(pair, Some((3_000_000, 500_000)));
    }

    #[tokio::test]
    async fn get_cap_and_avg_blob_size_none_for_missing() {
        let pool = test_pool().await;
        let pair = get_cap_and_avg_blob_size(&pool, &AccountId::new())
            .await
            .unwrap();
        assert_eq!(pair, None);
    }

    #[tokio::test]
    async fn set_max_unencoded_and_avg_blob_size_updates_both() {
        let pool = test_pool().await;
        let account = create_account(
            &pool,
            &AppId::INTERNAL,
            None,
            Some(1_000_000),
            Some(100_000),
            None,
        )
        .await
        .unwrap();
        let updated = set_max_unencoded_and_avg_blob_size(&pool, &account.id, 9_000_000, 2_000_000)
            .await
            .unwrap();
        assert!(updated);
        let pair = get_cap_and_avg_blob_size(&pool, &account.id).await.unwrap();
        assert_eq!(pair, Some((9_000_000, 2_000_000)));
    }

    #[tokio::test]
    async fn set_max_unencoded_and_avg_blob_size_false_for_missing() {
        let pool = test_pool().await;
        let updated = set_max_unencoded_and_avg_blob_size(&pool, &AccountId::new(), 1, 1)
            .await
            .unwrap();
        assert!(!updated);
    }

    #[tokio::test]
    async fn get_max_unencoded_bytes_returns_none_for_missing() {
        let pool = test_pool().await;
        let value = get_max_unencoded_bytes(&pool, &AccountId::new())
            .await
            .unwrap();
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn create_account_with_name() {
        let pool = test_pool().await;
        let account = create_account(
            &pool,
            &AppId::INTERNAL,
            Some("my-account"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(account.name, "my-account");
        assert_eq!(account.app_id, AppId::INTERNAL);
    }

    #[tokio::test]
    async fn get_account_returns_created() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
    async fn reconcile_pool_after_drift_overwrites_counters() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        // Prime with drifted counters.
        set_storage_pool(&pool, &account.id, "0xabc", 42, 999_999, 888_888)
            .await
            .unwrap();
        reconcile_pool_after_drift(&pool, &account.id, 1_000, 400)
            .await
            .unwrap();
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        // Reserved + used are overwritten absolutely; object_id and end_epoch untouched.
        assert_eq!(state.object_id, "0xabc");
        assert_eq!(state.end_epoch, 42);
        assert_eq!(state.reserved_encoded_bytes, 1_000);
        assert_eq!(state.used_encoded_bytes, 400);
    }

    #[tokio::test]
    async fn update_pool_after_delete_decrements_used() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
        let a = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        let b = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
        let _unpooled = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        let pooled = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
        let a = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
        let a = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        set_storage_pool(&pool, &a.id, "0xaaa", 10, 1_000, 0)
            .await
            .unwrap();

        let first = claim_pools_for_extension(&pool, 20, 100, now_at(60), now_at(0))
            .await
            .unwrap();
        assert_eq!(first.len(), 1);

        // Simulate successful extend: pool_end_epoch jumps past the cutoff.
        bump_pool_end_epoch(&pool, &a.id, "0xaaa", 50)
            .await
            .unwrap();

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
            let acc = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
                .await
                .unwrap();
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
    async fn fetch_webhook_for_apps_returns_map() {
        let pool = test_pool().await;
        let with_hook = crate::db::apps::create_app(&pool, "with-hook", "wh@example.com")
            .await
            .unwrap();
        crate::db::apps::set_app_webhook(
            &pool,
            &with_hook.id,
            "https://example.com/hook",
            "pubkey-b64",
            "privkey-b64",
        )
        .await
        .unwrap();
        let without_hook = crate::db::apps::create_app(&pool, "no-hook", "n@example.com")
            .await
            .unwrap();

        let map = fetch_webhook_for_apps(&pool, &[with_hook.id, without_hook.id])
            .await
            .unwrap();
        assert_eq!(map.len(), 2);
        let wh = map.get(&with_hook.id).unwrap().as_ref().unwrap();
        assert_eq!(wh.url, "https://example.com/hook");
        assert_eq!(wh.public_key_b64, "pubkey-b64");
        assert_eq!(wh.private_key_b64, "privkey-b64");
        assert!(map.get(&without_hook.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn fetch_webhook_for_apps_handles_empty_input() {
        let pool = test_pool().await;
        let map = fetch_webhook_for_apps(&pool, &[]).await.unwrap();
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn set_max_unencoded_bytes_updates_existing_account() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, Some(1_000_000), None, None)
            .await
            .unwrap();
        let updated = set_max_unencoded_bytes(&pool, &account.id, 2_500_000)
            .await
            .unwrap();
        assert!(updated, "row must be updated");
        let fetched = get_max_unencoded_bytes(&pool, &account.id).await.unwrap();
        assert_eq!(fetched, Some(2_500_000));
    }

    #[tokio::test]
    async fn set_max_unencoded_bytes_returns_false_for_missing_account() {
        let pool = test_pool().await;
        let updated = set_max_unencoded_bytes(&pool, &AccountId::new(), 1_000)
            .await
            .unwrap();
        assert!(!updated);
    }

    #[tokio::test]
    async fn bump_pool_end_epoch_updates_only_end_epoch() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        set_storage_pool(&pool, &account.id, "0xabc", 42, 1_000, 400)
            .await
            .unwrap();

        bump_pool_end_epoch(&pool, &account.id, "0xabc", 99)
            .await
            .unwrap();

        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.end_epoch, 99);
        assert_eq!(state.reserved_encoded_bytes, 1_000);
        assert_eq!(state.used_encoded_bytes, 400);
    }

    #[tokio::test]
    async fn bump_pool_end_epoch_ignores_replaced_pool() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        set_storage_pool(&pool, &account.id, "0xnew", 42, 1_000, 0)
            .await
            .unwrap();

        // An epoch observed on the old pool object must not land on the
        // replacement that swapped in while the row was claimed.
        let updated = bump_pool_end_epoch(&pool, &account.id, "0xold", 99)
            .await
            .unwrap();
        assert!(!updated);
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.end_epoch, 42);

        let updated = bump_pool_end_epoch(&pool, &account.id, "0xnew", 99)
            .await
            .unwrap();
        assert!(updated);
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.end_epoch, 99);
    }

    #[tokio::test]
    async fn bump_pool_end_epoch_does_not_regress() {
        let pool = test_pool().await;
        let account = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        set_storage_pool(&pool, &account.id, "0xabc", 100, 1_000, 400)
            .await
            .unwrap();

        // Stale/smaller new_end_epoch must be a no-op.
        bump_pool_end_epoch(&pool, &account.id, "0xabc", 50)
            .await
            .unwrap();
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.end_epoch, 100);

        // Equal new_end_epoch must also be a no-op (strict >).
        bump_pool_end_epoch(&pool, &account.id, "0xabc", 100)
            .await
            .unwrap();
        let state = get_storage_pool(&pool, &account.id)
            .await
            .unwrap()
            .expect("pool state must be present");
        assert_eq!(state.end_epoch, 100);

        // Strictly greater value still applies.
        bump_pool_end_epoch(&pool, &account.id, "0xabc", 101)
            .await
            .unwrap();
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
        let acc = create_account(&pool, &AppId::INTERNAL, Some("alpha"), None, None, None)
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
        let acc = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
        let acc = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
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
        let app_a = crate::db::apps::create_app(&pool, "app-a", "a@example.com")
            .await
            .unwrap();
        let app_b = crate::db::apps::create_app(&pool, "app-b", "b@example.com")
            .await
            .unwrap();
        let acc_a = create_account(&pool, &app_a.id, Some("for-a"), None, None, None)
            .await
            .unwrap();
        let _acc_b = create_account(&pool, &app_b.id, Some("for-b"), None, None, None)
            .await
            .unwrap();

        let summaries = list_account_summaries_by_app(&pool, &app_a.id)
            .await
            .unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, acc_a.id);
        assert_eq!(summaries[0].name, "for-a");
    }

    #[tokio::test]
    async fn reset_expired_pool_clears_state_blobs_and_records_audit() {
        let pool = test_pool().await;
        let a = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        set_storage_pool(&pool, &a.id, "0xaaa", 10, 1_000, 0)
            .await
            .unwrap();
        db::buckets::create_bucket(&pool, &a.id, "expired-bkt")
            .await
            .unwrap();
        db::blobs::insert_blob(
            &pool,
            "k1",
            "blob1",
            "expired-bkt",
            &a.id,
            "text/plain",
            3,
            "md5",
            None,
            None,
        )
        .await
        .unwrap();

        let deleted = reset_expired_pool(
            &pool,
            &a.id,
            &AppId::INTERNAL,
            "0xaaa",
            serde_json::json!({"db_pool_end_epoch": 10}),
        )
        .await
        .unwrap();
        assert_eq!(deleted, Some(1));

        // Pool columns nulled → account lazy-recreates on next upload.
        assert!(get_storage_pool(&pool, &a.id).await.unwrap().is_none());
        assert_eq!(db::blobs::count_blobs(&pool).await.unwrap(), 0);

        // Row is no longer claimable by the extension task.
        let claims = claim_pools_for_extension(&pool, 100, 100, now_at(60), now_at(0))
            .await
            .unwrap();
        assert!(claims.is_empty());

        // Audit event committed with the deleted-blob count merged in.
        let events = db::audit_events::list_audit_events_by_app(&pool, &AppId::INTERNAL)
            .await
            .unwrap();
        let ev = events
            .iter()
            .find(|e| e.event_type == AUDIT_EVENT_POOL_EXPIRED)
            .expect("pool_expired audit event");
        let data: serde_json::Value = serde_json::from_str(&ev.event_data).unwrap();
        assert_eq!(data["deleted_blob_count"], 1);
        assert_eq!(data["db_pool_end_epoch"], 10);

        // A fresh pool can be set afterwards (first-writer-wins on NULL).
        assert!(
            set_storage_pool(&pool, &a.id, "0xbbb", 99, 1_000, 0)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn pool_health_stats_aggregates_backoff_and_min_end_epoch() {
        let pool = test_pool().await;

        // No pools at all: zeros and no min epoch.
        let empty = pool_health_stats(&pool).await.unwrap();
        assert_eq!(
            empty,
            PoolHealthStats {
                pools_in_backoff: 0,
                max_failure_count: 0,
                min_pool_end_epoch: None,
            }
        );

        let a = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        let b = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        // An account without a pool must not count, even if it somehow
        // carries a failure count.
        let _no_pool = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        set_storage_pool(&pool, &a.id, "0xaaa", 40, 1_000, 0)
            .await
            .unwrap();
        set_storage_pool(&pool, &b.id, "0xbbb", 25, 1_000, 0)
            .await
            .unwrap();

        let healthy = pool_health_stats(&pool).await.unwrap();
        assert_eq!(
            healthy,
            PoolHealthStats {
                pools_in_backoff: 0,
                max_failure_count: 0,
                min_pool_end_epoch: Some(25),
            }
        );

        record_extension_failure(&pool, &a.id, now_at(60))
            .await
            .unwrap();
        record_extension_failure(&pool, &a.id, now_at(120))
            .await
            .unwrap();
        record_extension_failure(&pool, &b.id, now_at(60))
            .await
            .unwrap();

        let degraded = pool_health_stats(&pool).await.unwrap();
        assert_eq!(
            degraded,
            PoolHealthStats {
                pools_in_backoff: 2,
                max_failure_count: 2,
                min_pool_end_epoch: Some(25),
            }
        );

        // A successful extension clears the failure count and moves the min.
        bump_pool_end_epoch(&pool, &b.id, "0xbbb", 60)
            .await
            .unwrap();
        let recovered = pool_health_stats(&pool).await.unwrap();
        assert_eq!(
            recovered,
            PoolHealthStats {
                pools_in_backoff: 1,
                max_failure_count: 2,
                min_pool_end_epoch: Some(40),
            }
        );
    }

    #[tokio::test]
    async fn record_extension_failure_backoff_round_trip() {
        let pool = test_pool().await;
        let a = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        set_storage_pool(&pool, &a.id, "0xaaa", 10, 1_000, 0)
            .await
            .unwrap();

        // Fresh row claims with a zero failure count.
        let first = claim_pools_for_extension(&pool, 100, 100, now_at(60), now_at(0))
            .await
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].extend_failure_count, 0);

        // Failure pushes the stamp past the claim-time cooldown and bumps
        // the counter.
        record_extension_failure(&pool, &a.id, now_at(240))
            .await
            .unwrap();
        let during_backoff = claim_pools_for_extension(&pool, 100, 100, now_at(160), now_at(100))
            .await
            .unwrap();
        assert!(during_backoff.is_empty(), "row must stay quiet in backoff");
        let after_backoff = claim_pools_for_extension(&pool, 100, 100, now_at(300), now_at(240))
            .await
            .unwrap();
        assert_eq!(after_backoff.len(), 1);
        assert_eq!(after_backoff[0].extend_failure_count, 1);

        // A successful extension resets the counter.
        bump_pool_end_epoch(&pool, &a.id, "0xaaa", 12)
            .await
            .unwrap();
        let after_success = claim_pools_for_extension(&pool, 100, 100, now_at(400), now_at(360))
            .await
            .unwrap();
        assert_eq!(after_success.len(), 1);
        assert_eq!(after_success[0].extend_failure_count, 0);
    }

    #[tokio::test]
    async fn reset_expired_pool_guard_mismatch_is_noop() {
        let pool = test_pool().await;
        let a = create_account(&pool, &AppId::INTERNAL, None, None, None, None)
            .await
            .unwrap();
        set_storage_pool(&pool, &a.id, "0xaaa", 10, 1_000, 0)
            .await
            .unwrap();
        db::buckets::create_bucket(&pool, &a.id, "kept-bkt")
            .await
            .unwrap();
        db::blobs::insert_blob(
            &pool,
            "k1",
            "blob1",
            "kept-bkt",
            &a.id,
            "text/plain",
            3,
            "md5",
            None,
            None,
        )
        .await
        .unwrap();

        // Pool was concurrently replaced — expected id no longer matches.
        let deleted = reset_expired_pool(
            &pool,
            &a.id,
            &AppId::INTERNAL,
            "0xstale",
            serde_json::json!({}),
        )
        .await
        .unwrap();
        assert_eq!(deleted, None);

        // Nothing was touched: pool, blobs, and audit log all intact.
        assert!(get_storage_pool(&pool, &a.id).await.unwrap().is_some());
        assert_eq!(db::blobs::count_blobs(&pool).await.unwrap(), 1);
        let events = db::audit_events::list_audit_events_by_app(&pool, &AppId::INTERNAL)
            .await
            .unwrap();
        assert!(
            !events
                .iter()
                .any(|e| e.event_type == AUDIT_EVENT_POOL_EXPIRED)
        );
    }
}
