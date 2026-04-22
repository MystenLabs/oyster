use std::{collections::HashMap, time::Instant};

use metrics::{counter, gauge, histogram};
use sui_types::base_types::{ObjectID, SuiAddress};
use walrus_sui::client::{ReadClient as _, transaction_builder::WalrusPtbBuilder};

use crate::{
    AccountId,
    db::{self, DbPool, accounts::ExpiringPool},
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
    webhook::{self, InsufficientFundsPayload, WebhookClient},
};

/// Configuration for the background `StoragePool` extension task.
#[derive(Clone, Debug)]
pub struct ExtensionConfig {
    /// How often to check for expiring pools.
    pub check_interval: std::time::Duration,
    /// Select pools whose `pool_end_epoch` is less than
    /// `current_epoch + lookahead_epochs` (treated as ~days in config
    /// but named by epoch unit here to match the on-chain model).
    pub lookahead_epochs: u32,
    /// Number of Walrus epochs to extend pools by.
    pub extend_epochs: u32,
}

/// Run the background loop that periodically extends expiring
/// `StoragePool` objects on Walrus.
pub async fn run_extension_loop(
    db: DbPool,
    pearl: PearlConnection,
    rpc_url: String,
    system_object: ObjectID,
    staking_object: ObjectID,
    config: ExtensionConfig,
) {
    tracing::info!(
        "pool extension task started (interval={}s, lookahead={}e, epochs={})",
        config.check_interval.as_secs(),
        config.lookahead_epochs,
        config.extend_epochs,
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
        tokio::time::sleep(config.check_interval).await;
        let _ = run_extension_cycle_once(&db, &pearl, &rpc_url, &read_client, &config).await;
    }
}

/// Execute exactly one pool-extension cycle synchronously, without the
/// surrounding sleep loop. Tests use this to drive the extension task
/// deterministically. Returns `(extended, errors)`.
pub async fn run_extension_cycle_once(
    db: &DbPool,
    pearl: &PearlConnection,
    rpc_url: &str,
    read_client: &std::sync::Arc<walrus_sui::client::SuiReadClient>,
    config: &ExtensionConfig,
) -> (u32, u32) {
    counter!(EXTENSION_CYCLES_TOTAL).increment(1);
    let cycle_start = Instant::now();

    let current_epoch = match read_client.current_epoch().await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("failed to query current_epoch: {e}");
            counter!(EXTENSION_ERRORS_TOTAL, "stage" => "current_epoch").increment(1);
            return (0, 1);
        }
    };

    let cutoff_epoch = (current_epoch as i64) + (config.lookahead_epochs as i64);

    let pools =
        match db::accounts::list_accounts_with_pools_expiring_before(db, cutoff_epoch, 100).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("failed to query expiring pools: {e}");
                counter!(EXTENSION_ERRORS_TOTAL, "stage" => "db_query").increment(1);
                return (0, 1);
            }
        };

    gauge!(EXTENSION_POOLS_EXPIRING).set(pools.len() as f64);

    if pools.is_empty() {
        tracing::debug!("no pools need extension");
        gauge!(EXTENSION_CYCLE_POOLS_PROCESSED).set(0.0);
        histogram!(EXTENSION_CYCLE_DURATION_SECONDS).record(cycle_start.elapsed().as_secs_f64());
        return (0, 0);
    }

    tracing::info!("{} pool(s) approaching expiry, extending", pools.len());

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

                if let Some(ref url) = pool.webhook_url
                    && webhook::is_insufficient_funds_error(e.as_ref())
                {
                    let wh = webhook_clients
                        .entry(url.clone())
                        .or_insert_with(|| WebhookClient::new(url.clone()));
                    wh.notify_insufficient_funds(&InsufficientFundsPayload {
                        account_id: pool.account_id,
                        address: sender_address.to_string(),
                        error: e.to_string(),
                    })
                    .await;
                }
            }
        }
    }

    gauge!(EXTENSION_CYCLE_POOLS_PROCESSED).set((extended + errors) as f64);
    histogram!(EXTENSION_CYCLE_DURATION_SECONDS).record(cycle_start.elapsed().as_secs_f64());
    tracing::info!(extended, errors, "extension cycle complete");

    (extended, errors)
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
