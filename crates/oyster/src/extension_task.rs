use std::{collections::HashMap, time::Instant};

use chrono::Utc;
use ed25519_dalek::SigningKey;
use metrics::{counter, gauge, histogram};
use sui_types::base_types::{ObjectID, SuiAddress};
use uuid::Uuid;
use walrus_sui::{
    client::{ReadClient as _, SuiClientError, transaction_builder::WalrusPtbBuilder},
    coin::CoinType,
};

use crate::{
    AccountId, AppId, FundingAmount,
    db::{self, DbPool, accounts::ExpiringPool},
    extension_cost,
    metrics::{
        EXTENSION_BALANCE_PRECHECK_SKIPS_TOTAL, EXTENSION_CYCLE_DURATION_SECONDS,
        EXTENSION_CYCLE_POOLS_PROCESSED, EXTENSION_CYCLES_TOTAL, EXTENSION_ERRORS_TOTAL,
        EXTENSION_POOLS_ALREADY_EXTENDED_TOTAL, EXTENSION_POOLS_EXPIRED_RESET_TOTAL,
        EXTENSION_POOLS_EXPIRING, EXTENSION_POOLS_EXTENDED_TOTAL, EXTENSION_POOLS_REPAIRED_TOTAL,
        WEBHOOK_SKIPPED_UNSIGNED_TOTAL,
    },
    pearl_client::PearlConnection,
    sui_object_reader::{self, OnChainStoragePoolState},
    sui_transaction,
    webhook::{self, EVENT_TYPE_FUNDING_REQUIRED, FundingRequiredPayload, WebhookClient},
    webhook_keys,
};

/// Configuration for the background `StoragePool` extension task.
#[derive(Clone, Debug)]
pub struct ExtensionConfig {
    /// Select pools whose `pool_end_epoch` is less than
    /// `current_epoch + lookahead_epochs`.
    pub lookahead_epochs: u32,
    /// Number of Walrus epochs to extend pools by.
    pub extend_epochs: u32,
    /// Sleep duration when a cycle finds no work.
    pub idle_sleep: std::time::Duration,
    /// Sleep duration between busy cycles.
    pub busy_sleep: std::time::Duration,
    /// Maximum rows to claim per cycle.
    pub claim_batch_size: i64,
    /// Cooldown applied by `claim_pools_for_extension` — both the
    /// don't-double-claim and the don't-spam-Harbor backoff.
    pub claim_cooldown: std::time::Duration,
    /// Ceiling for the exponential per-pool retry backoff
    /// (`claim_cooldown * 2^failures`, capped here). Also bounds how long
    /// a user waits after funding their wallet before the next attempt.
    pub failure_backoff_cap: std::time::Duration,
}

/// Exponential backoff after `failures` consecutive failed attempts:
/// `base * 2^failures`, saturating, capped at `cap`. `failures` counts
/// the failure being recorded (so the first failure waits `2 * base`).
fn failure_backoff(
    base: std::time::Duration,
    cap: std::time::Duration,
    failures: i64,
) -> std::time::Duration {
    let shift = failures.clamp(0, 30) as u32;
    let secs = base
        .as_secs()
        .saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX));
    std::time::Duration::from_secs(secs).min(cap)
}

/// Run the background loop that continuously extends expiring `StoragePool`
/// objects on Walrus. Modeled after Walrus's `garbage_collector.rs`: when a
/// cycle finds nothing to do, sleep `idle_sleep`; while there is still work
/// to drain, only pause for `busy_sleep` between cycles.
pub async fn run_extension_loop(
    db: DbPool,
    pearl: PearlConnection,
    rpc_url: String,
    system_object: ObjectID,
    staking_object: ObjectID,
    config: ExtensionConfig,
) {
    tracing::info!(
        "pool extension task started (lookahead={}e, extend={}e, idle={}s, busy={}ms, batch={})",
        config.lookahead_epochs,
        config.extend_epochs,
        config.idle_sleep.as_secs(),
        config.busy_sleep.as_millis(),
        config.claim_batch_size,
    );

    let read_client =
        match sui_transaction::build_sui_read_client(&rpc_url, system_object, staking_object).await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("failed to build SuiReadClient for extension task: {e}");
                return;
            }
        };

    loop {
        let processed =
            run_extension_cycle_once(&db, &pearl, &rpc_url, &read_client, &config).await;
        if processed == 0 {
            tokio::time::sleep(config.idle_sleep).await;
        } else {
            tokio::time::sleep(config.busy_sleep).await;
        }
    }
}

/// Execute exactly one pool-extension cycle synchronously. Returns the number
/// of pool rows that were claimed and processed (regardless of outcome). A
/// return of `0` means there was nothing to extend; callers can use this to
/// pick between idle and busy sleeps.
pub async fn run_extension_cycle_once(
    db: &DbPool,
    pearl: &PearlConnection,
    rpc_url: &str,
    read_client: &std::sync::Arc<walrus_sui::client::SuiReadClient>,
    config: &ExtensionConfig,
) -> u32 {
    counter!(EXTENSION_CYCLES_TOTAL).increment(1);
    let cycle_start = Instant::now();

    let current_epoch = match read_client.current_epoch().await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("failed to query current_epoch: {e}");
            counter!(EXTENSION_ERRORS_TOTAL, "stage" => "current_epoch").increment(1);
            return 0;
        }
    };

    let cutoff_epoch = (current_epoch as i64) + (config.lookahead_epochs as i64);
    let now = Utc::now();
    let claim_until = now
        + chrono::Duration::from_std(config.claim_cooldown)
            .unwrap_or_else(|_| chrono::Duration::seconds(60));

    let pools = match db::accounts::claim_pools_for_extension(
        db,
        cutoff_epoch,
        config.claim_batch_size,
        claim_until,
        now,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("failed to claim expiring pools: {e}");
            counter!(EXTENSION_ERRORS_TOTAL, "stage" => "db_query").increment(1);
            return 0;
        }
    };

    gauge!(EXTENSION_POOLS_EXPIRING).set(pools.len() as f64);

    if pools.is_empty() {
        tracing::debug!("no pools need extension");
        gauge!(EXTENSION_CYCLE_POOLS_PROCESSED).set(0.0);
        histogram!(EXTENSION_CYCLE_DURATION_SECONDS).record(cycle_start.elapsed().as_secs_f64());
        return 0;
    }

    tracing::info!("{} pool(s) approaching expiry, extending", pools.len());

    // Resolve webhook URLs once per cycle, keyed by the distinct app_ids in this batch.
    let app_ids: Vec<AppId> = pools
        .iter()
        .map(|p| p.app_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let webhook_for_apps = match db::accounts::fetch_webhook_for_apps(db, &app_ids).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("failed to fetch webhook configs: {e}");
            HashMap::new()
        }
    };

    let mut extended = 0u32;
    let mut errors = 0u32;
    let mut expired_handled = 0u32;
    let mut already_extended = 0u32;
    let mut skipped_unfunded = 0u32;
    let mut address_cache: HashMap<AccountId, SuiAddress> = HashMap::new();
    let mut webhook_clients: HashMap<AppId, WebhookClient> = HashMap::new();

    for pool in &pools {
        // The DB `pool_end_epoch` is only a hint for *claiming* rows; the
        // on-chain value decides what to do. `extend_storage_pool` is
        // additive (each call pushes `end_epoch` out by `extend_epochs`),
        // so acting on a stale-low DB value would extend the pool twice
        // whenever an earlier attempt landed on-chain but its DB update
        // was lost (write failure, checkpoint-wait timeout after
        // execution, crash between submit and update). One read here
        // makes every retry idempotent.
        let Some(on_chain) = read_pool_state(rpc_url, pool).await else {
            errors += 1;
            continue;
        };
        let on_chain_end = on_chain.end_epoch as i64;

        // A pool whose end epoch is already past cannot be extended on
        // Walrus (storage end epochs are exclusive) — attempting the PTB
        // would just burn RPCs and gas every cycle, forever.
        if on_chain_end <= current_epoch as i64 {
            handle_expired_pool(db, pool, &on_chain, current_epoch).await;
            expired_handled += 1;
            continue;
        }

        if on_chain_end > pool.pool_end_epoch {
            // Stale DB — a previous extension landed (ours, with its DB
            // update lost, or one made outside Oyster). Repair first so
            // a later failure below leaves the row accurate.
            db_repair_end_epoch(db, pool, on_chain.end_epoch).await;
        }

        if on_chain_end >= cutoff_epoch {
            // Already past the lookahead window on-chain: nothing to do.
            // The repair above lifts the row out of the claim query.
            counter!(EXTENSION_POOLS_ALREADY_EXTENDED_TOTAL).increment(1);
            tracing::info!(
                account_id = %pool.account_id,
                storage_pool_object_id = %pool.storage_pool_object_id,
                db_pool_end_epoch = pool.pool_end_epoch,
                on_chain_end_epoch = on_chain.end_epoch,
                cutoff_epoch,
                "pool already extended on-chain, skipping"
            );
            already_extended += 1;
            continue;
        }

        let sender_address = match address_cache.get(&pool.account_id) {
            Some(&addr) => addr,
            None => match sui_transaction::resolve_sender_address(
                pearl,
                &pool.account_id,
                pool.key_version,
            )
            .await
            {
                Ok(addr) => {
                    address_cache.insert(pool.account_id, addr);
                    addr
                }
                Err(e) => {
                    tracing::warn!(
                        account_id = %pool.account_id,
                        error = %e,
                        "failed to resolve sender address, skipping pool"
                    );
                    counter!(EXTENSION_ERRORS_TOTAL, "stage" => "resolve_address").increment(1);
                    errors += 1;
                    continue;
                }
            },
        };

        // Retry attempts (failure count > 0) get a cheap pre-check: one
        // coin-selection read against the exact WAL cost instead of the
        // full PTB-build + sign + execute chain. If the wallet still
        // cannot cover the cost, skip the attempt, keep the backoff
        // growing, and re-notify the app. Fails open on any
        // indeterminate result so a funded wallet is never starved.
        if pool.extend_failure_count > 0
            && let Some(cost) =
                wal_shortfall(read_client, pool, sender_address, config.extend_epochs).await
        {
            counter!(EXTENSION_BALANCE_PRECHECK_SKIPS_TOTAL).increment(1);
            tracing::info!(
                account_id = %pool.account_id,
                wal_frost_needed = cost.wal_frost,
                extend_failure_count = pool.extend_failure_count,
                "wallet still cannot cover extension cost, skipping attempt"
            );
            record_failure_backoff(db, pool, config).await;
            notify_funding_required(
                &webhook_for_apps,
                &mut webhook_clients,
                pool,
                sender_address,
                cost,
            )
            .await;
            skipped_unfunded += 1;
            continue;
        }

        match extend_single_pool(
            read_client,
            pearl,
            &pool.account_id,
            rpc_url,
            sender_address,
            pool,
            config.extend_epochs,
        )
        .await
        {
            Ok(()) => {
                let new_end = on_chain_end + config.extend_epochs as i64;
                if let Err(e) =
                    db::accounts::bump_pool_end_epoch(db, &pool.account_id, new_end).await
                {
                    tracing::warn!(
                        account_id = %pool.account_id,
                        error = %e,
                        "failed to bump pool_end_epoch in DB"
                    );
                    counter!(EXTENSION_ERRORS_TOTAL, "stage" => "db_update").increment(1);
                }
                counter!(EXTENSION_POOLS_EXTENDED_TOTAL).increment(1);
                extended += 1;
            }
            Err(e) => {
                tracing::warn!(
                    account_id = %pool.account_id,
                    storage_pool_object_id = %pool.storage_pool_object_id,
                    error = %e,
                    "failed to extend pool"
                );
                counter!(EXTENSION_ERRORS_TOTAL, "stage" => "extend_storage_pool").increment(1);
                errors += 1;

                record_failure_backoff(db, pool, config).await;

                if webhook::is_insufficient_funds_error(e.as_ref()) {
                    let cost = match extension_cost::compute_extension_cost(
                        read_client,
                        pool,
                        config.extend_epochs,
                    )
                    .await
                    {
                        Ok(c) => c,
                        Err(err) => {
                            tracing::warn!(
                                account_id = %pool.account_id,
                                error = %err,
                                "failed to compute extension cost; falling back to zeros"
                            );
                            FundingAmount {
                                wal_frost: 0,
                                sui_mist: 0,
                            }
                        }
                    };
                    notify_funding_required(
                        &webhook_for_apps,
                        &mut webhook_clients,
                        pool,
                        sender_address,
                        cost,
                    )
                    .await;
                }
            }
        }
    }

    let processed = extended + errors + expired_handled + already_extended + skipped_unfunded;
    gauge!(EXTENSION_CYCLE_POOLS_PROCESSED).set(processed as f64);
    histogram!(EXTENSION_CYCLE_DURATION_SECONDS).record(cycle_start.elapsed().as_secs_f64());
    tracing::info!(
        extended,
        errors,
        expired_handled,
        already_extended,
        skipped_unfunded,
        "extension cycle complete"
    );

    processed
}

/// Exponential backoff bookkeeping after a failed (or pre-check-skipped)
/// extension attempt: push the row's next attempt out by
/// `claim_cooldown * 2^failures` (capped) so a persistently failing pool —
/// typically an unfunded wallet — stops burning the full PTB/sign/execute
/// RPC chain every cooldown. The exponent is this failure's ordinal
/// (prior count + 1); success resets the count via `bump_pool_end_epoch`.
async fn record_failure_backoff(db: &DbPool, pool: &ExpiringPool, config: &ExtensionConfig) {
    let backoff = failure_backoff(
        config.claim_cooldown,
        config.failure_backoff_cap,
        pool.extend_failure_count + 1,
    );
    let next_attempt_after = Utc::now()
        + chrono::Duration::from_std(backoff).unwrap_or_else(|_| chrono::Duration::seconds(3600));
    if let Err(db_err) =
        db::accounts::record_extension_failure(db, &pool.account_id, next_attempt_after).await
    {
        tracing::warn!(
            account_id = %pool.account_id,
            error = %db_err,
            "failed to record extension failure backoff"
        );
        counter!(EXTENSION_ERRORS_TOTAL, "stage" => "db_update").increment(1);
    }
}

/// Send the `account.funding_required` webhook for `pool` if its app has
/// one configured. Fire-and-forget: delivery failures are handled inside
/// `WebhookClient`.
async fn notify_funding_required(
    webhook_for_apps: &HashMap<AppId, Option<db::accounts::AppWebhook>>,
    webhook_clients: &mut HashMap<AppId, WebhookClient>,
    pool: &ExpiringPool,
    sender_address: SuiAddress,
    cost: FundingAmount,
) {
    let Some(Some(wh_cfg)) = webhook_for_apps.get(&pool.app_id) else {
        return;
    };
    let payload = FundingRequiredPayload {
        event_id: Uuid::new_v4(),
        event_type: EVENT_TYPE_FUNDING_REQUIRED,
        account_id: pool.account_id,
        pearl_address: sender_address.to_string(),
        amount: cost,
        timestamp: Utc::now(),
    };
    if let Some(wh) = get_or_build_webhook_client(webhook_clients, &pool.app_id, wh_cfg) {
        wh.notify_funding_required(&payload).await;
    }
}

/// WAL-balance pre-check for a retry attempt. Returns `Some(cost)` when
/// the sender's wallet demonstrably cannot cover the WAL cost of the next
/// extension (the dominant shortfall — SUI gas is not checked because a
/// tight-but-sufficient gas balance must not cause a false skip), `None`
/// when the wallet can cover it or the check is indeterminate (cost or
/// coin lookup failed — fail open so a funded wallet is never starved).
async fn wal_shortfall(
    read_client: &std::sync::Arc<walrus_sui::client::SuiReadClient>,
    pool: &ExpiringPool,
    sender_address: SuiAddress,
    extend_epochs: u32,
) -> Option<FundingAmount> {
    let cost = match extension_cost::compute_extension_cost(read_client, pool, extend_epochs).await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                account_id = %pool.account_id,
                error = %e,
                "balance pre-check could not compute extension cost; attempting anyway"
            );
            return None;
        }
    };
    match read_client
        .get_coins_with_total_balance(sender_address, CoinType::Wal, cost.wal_frost, vec![])
        .await
    {
        Ok(_) => None,
        Err(SuiClientError::NoCompatibleWalCoins) => Some(cost),
        Err(e) => {
            tracing::warn!(
                account_id = %pool.account_id,
                error = %e,
                "balance pre-check coin lookup failed; attempting anyway"
            );
            None
        }
    }
}

/// Read the authoritative on-chain `StoragePoolInnerV1` for a claimed
/// pool. Returns `None` (after logging and counting the error) when the
/// stored ObjectID is unparsable or the read fails; the row is left
/// claimed so its cooldown stamp keeps it quiet until a later cycle
/// re-examines it. Never extend blind — a failed read is exactly the
/// situation in which a duplicate extension could slip through.
async fn read_pool_state(rpc_url: &str, pool: &ExpiringPool) -> Option<OnChainStoragePoolState> {
    let pool_object_id: ObjectID = match pool.storage_pool_object_id.parse() {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(
                account_id = %pool.account_id,
                storage_pool_object_id = %pool.storage_pool_object_id,
                error = %e,
                "stored pool ObjectID unparsable, cannot reconcile pool"
            );
            counter!(EXTENSION_ERRORS_TOTAL, "stage" => "chain_reconcile").increment(1);
            return None;
        }
    };

    match sui_object_reader::read_storage_pool_state(rpc_url, pool_object_id).await {
        Ok(state) => Some(state),
        Err(e) => {
            tracing::warn!(
                account_id = %pool.account_id,
                storage_pool_object_id = %pool.storage_pool_object_id,
                error = %e,
                "failed to read on-chain pool state, will retry later"
            );
            counter!(EXTENSION_ERRORS_TOTAL, "stage" => "chain_reconcile").increment(1);
            None
        }
    }
}

/// Handle a claimed pool whose on-chain end epoch is already past. The
/// pool can never be extended again; reset the account for lazy
/// re-create ([`db::accounts::reset_expired_pool`]).
async fn handle_expired_pool(
    db: &DbPool,
    pool: &ExpiringPool,
    on_chain: &OnChainStoragePoolState,
    current_epoch: u32,
) {
    debug_assert!(on_chain.end_epoch as i64 <= current_epoch as i64);

    let event_data = serde_json::json!({
        "account_id": pool.account_id.to_string(),
        "storage_pool_object_id": pool.storage_pool_object_id,
        "db_pool_end_epoch": pool.pool_end_epoch,
        "on_chain_end_epoch": on_chain.end_epoch,
        "current_epoch": current_epoch,
    });
    match db::accounts::reset_expired_pool(
        db,
        &pool.account_id,
        &pool.app_id,
        &pool.storage_pool_object_id,
        event_data,
    )
    .await
    {
        Ok(Some(deleted_blobs)) => {
            counter!(EXTENSION_POOLS_EXPIRED_RESET_TOTAL).increment(1);
            tracing::warn!(
                account_id = %pool.account_id,
                storage_pool_object_id = %pool.storage_pool_object_id,
                db_pool_end_epoch = pool.pool_end_epoch,
                on_chain_end_epoch = on_chain.end_epoch,
                current_epoch,
                deleted_blobs,
                "storage pool expired on-chain; account reset for lazy re-create"
            );
        }
        Ok(None) => {
            // Concurrent writer changed the pool between claim and reset —
            // whatever replaced it will be picked up by a later cycle.
            tracing::info!(
                account_id = %pool.account_id,
                "expired-pool reset skipped: pool changed concurrently"
            );
        }
        Err(e) => {
            tracing::error!(
                account_id = %pool.account_id,
                error = %e,
                "failed to reset expired pool in DB"
            );
            counter!(EXTENSION_ERRORS_TOTAL, "stage" => "expired_reset").increment(1);
        }
    }
}

/// Repair a stale-low DB `pool_end_epoch` from the authoritative
/// on-chain value. `bump_pool_end_epoch` only ever moves the value
/// forward, so a concurrent extension cannot be regressed.
async fn db_repair_end_epoch(db: &DbPool, pool: &ExpiringPool, on_chain_end_epoch: u64) {
    match db::accounts::bump_pool_end_epoch(db, &pool.account_id, on_chain_end_epoch as i64).await {
        Ok(()) => {
            counter!(EXTENSION_POOLS_REPAIRED_TOTAL).increment(1);
            tracing::info!(
                account_id = %pool.account_id,
                db_pool_end_epoch = pool.pool_end_epoch,
                on_chain_end_epoch,
                "repaired stale pool_end_epoch from on-chain value"
            );
        }
        Err(e) => {
            tracing::warn!(
                account_id = %pool.account_id,
                error = %e,
                "failed to repair pool_end_epoch from on-chain value"
            );
            counter!(EXTENSION_ERRORS_TOTAL, "stage" => "expiry_repair").increment(1);
        }
    }
}

/// Build or retrieve a `WebhookClient` for `app_id` using the per-app
/// keypair. Returns `None` (and emits a warning + metric) when the stored
/// keys fail to decode — defensive: post-migration this should not happen.
fn get_or_build_webhook_client<'a>(
    cache: &'a mut HashMap<AppId, WebhookClient>,
    app_id: &AppId,
    cfg: &db::accounts::AppWebhook,
) -> Option<&'a WebhookClient> {
    if !cache.contains_key(app_id) {
        let private_bytes = match webhook_keys::decode_key(&cfg.private_key_b64) {
            Ok(b) => b,
            Err(e) => {
                counter!(WEBHOOK_SKIPPED_UNSIGNED_TOTAL).increment(1);
                tracing::warn!(%app_id, error = %e, "skipping webhook delivery: invalid private key");
                return None;
            }
        };
        let public_bytes = match webhook_keys::decode_key(&cfg.public_key_b64) {
            Ok(b) => b,
            Err(e) => {
                counter!(WEBHOOK_SKIPPED_UNSIGNED_TOTAL).increment(1);
                tracing::warn!(%app_id, error = %e, "skipping webhook delivery: invalid public key");
                return None;
            }
        };
        let signing_key = SigningKey::from_bytes(&private_bytes);
        cache.insert(
            *app_id,
            WebhookClient::new(cfg.url.clone(), signing_key, public_bytes),
        );
    }
    cache.get(app_id)
}

async fn extend_single_pool(
    read_client: &std::sync::Arc<walrus_sui::client::SuiReadClient>,
    pearl: &PearlConnection,
    account_id: &AccountId,
    rpc_url: &str,
    sender_address: SuiAddress,
    pool: &ExpiringPool,
    extend_epochs: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let pool_object_id: ObjectID = pool.storage_pool_object_id.parse()?;

    let mut ptb = WalrusPtbBuilder::new(read_client.clone(), sender_address);
    ptb.extend_storage_pool(
        pool_object_id,
        extend_epochs,
        pool.pool_reserved_encoded_bytes.max(0) as u64,
    )
    .await?;

    let tx_data = ptb.build_transaction_data(None).await?;
    sui_transaction::sign_and_submit(pearl, account_id, pool.key_version, rpc_url, tx_data).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::failure_backoff;

    #[test]
    fn failure_backoff_doubles_per_failure() {
        let base = Duration::from_secs(60);
        let cap = Duration::from_secs(3600);
        assert_eq!(failure_backoff(base, cap, 1), Duration::from_secs(120));
        assert_eq!(failure_backoff(base, cap, 2), Duration::from_secs(240));
        assert_eq!(failure_backoff(base, cap, 5), Duration::from_secs(1920));
    }

    #[test]
    fn failure_backoff_caps() {
        let base = Duration::from_secs(60);
        let cap = Duration::from_secs(3600);
        assert_eq!(failure_backoff(base, cap, 6), cap);
        assert_eq!(failure_backoff(base, cap, 60), cap);
        assert_eq!(failure_backoff(base, cap, i64::MAX), cap);
    }

    #[test]
    fn failure_backoff_handles_degenerate_inputs() {
        let cap = Duration::from_secs(3600);
        // Zero / negative failure counts fall back to the base cooldown.
        assert_eq!(
            failure_backoff(Duration::from_secs(60), cap, 0),
            Duration::from_secs(60)
        );
        assert_eq!(
            failure_backoff(Duration::from_secs(60), cap, -3),
            Duration::from_secs(60)
        );
        // Zero base never schedules a negative/overflowed duration.
        assert_eq!(failure_backoff(Duration::ZERO, cap, 10), Duration::ZERO);
    }
}
