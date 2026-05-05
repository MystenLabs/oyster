use std::{collections::HashMap, time::Instant};

use chrono::Utc;
use metrics::{counter, gauge, histogram};
use sui_types::base_types::{ObjectID, SuiAddress};
use uuid::Uuid;
use walrus_sui::client::{ReadClient as _, transaction_builder::WalrusPtbBuilder};

use crate::{
    AccountId,
    AppId,
    db::{self, DbPool, accounts::ExpiringPool},
    extension_cost::{self, ExtensionCost},
    metrics::{
        EXTENSION_CYCLE_DURATION_SECONDS,
        EXTENSION_CYCLE_POOLS_PROCESSED,
        EXTENSION_CYCLES_TOTAL,
        EXTENSION_ERRORS_TOTAL,
        EXTENSION_POOLS_EXPIRING,
        EXTENSION_POOLS_EXTENDED_TOTAL,
    },
    pearl_client::PearlConnection,
    sui_transaction,
    webhook::{
        self,
        EVENT_TYPE_FUNDING_REQUIRED,
        FundingAmount,
        FundingRequiredPayload,
        WebhookClient,
    },
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
    let webhook_urls = match db::accounts::fetch_webhook_urls_for_apps(db, &app_ids).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("failed to fetch webhook URLs: {e}");
            HashMap::new()
        }
    };

    let mut extended = 0u32;
    let mut errors = 0u32;
    let mut address_cache: HashMap<AccountId, SuiAddress> = HashMap::new();
    let mut webhook_clients: HashMap<String, WebhookClient> = HashMap::new();

    for pool in &pools {
        let sender_address = match address_cache.get(&pool.account_id) {
            Some(&addr) => addr,
            None => match sui_transaction::resolve_sender_address(pearl, &pool.account_id).await {
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
                let new_end = pool.pool_end_epoch + config.extend_epochs as i64;
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

                if webhook::is_insufficient_funds_error(e.as_ref())
                    && let Some(Some(url)) = webhook_urls.get(&pool.app_id)
                {
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
                            ExtensionCost {
                                wal_frost: 0,
                                sui_mist: 0,
                            }
                        }
                    };

                    let payload = FundingRequiredPayload {
                        event_id: Uuid::new_v4(),
                        event_type: EVENT_TYPE_FUNDING_REQUIRED,
                        account_id: pool.account_id,
                        pearl_address: sender_address.to_string(),
                        amount: FundingAmount {
                            wal_frost: cost.wal_frost.to_string(),
                            sui_mist: cost.sui_mist.to_string(),
                        },
                        timestamp: Utc::now(),
                    };
                    let wh = webhook_clients
                        .entry(url.clone())
                        .or_insert_with(|| WebhookClient::new(url.clone()));
                    wh.notify_funding_required(&payload).await;
                }
            }
        }
    }

    let processed = extended + errors;
    gauge!(EXTENSION_CYCLE_POOLS_PROCESSED).set(processed as f64);
    histogram!(EXTENSION_CYCLE_DURATION_SECONDS).record(cycle_start.elapsed().as_secs_f64());
    tracing::info!(extended, errors, "extension cycle complete");

    processed
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
    sui_transaction::sign_and_submit(pearl, account_id, rpc_url, tx_data).await?;

    Ok(())
}
