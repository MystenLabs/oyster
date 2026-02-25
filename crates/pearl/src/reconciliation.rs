use std::str::FromStr;

use sui_sdk::SuiClientBuilder;
use sui_types::base_types::SuiAddress;

use crate::{config::Config, db};

pub async fn run_reconciliation_loop(pool: db::DbPool, config: Config) {
    let sui_rpc_url = match &config.sui_rpc_url {
        Some(url) => url.clone(),
        None => return,
    };

    let interval = std::time::Duration::from_secs(config.reconciliation_interval_secs);

    loop {
        if let Err(e) = reconcile_once(&pool, &config, &sui_rpc_url).await {
            tracing::warn!("reconciliation error: {e}");
        }
        tokio::time::sleep(interval).await;
    }
}

async fn reconcile_once(
    pool: &db::DbPool,
    config: &Config,
    sui_rpc_url: &str,
) -> Result<(), crate::error::Error> {
    // 1. Pick a random account to reconcile.
    let account_id = match db::accounts::get_random_account_id(pool).await? {
        Some(id) => id,
        None => return Ok(()),
    };

    let account = db::accounts::get_account(pool, &account_id).await?;
    let address = SuiAddress::from_str(&account.address)
        .map_err(|e| crate::error::Error::SuiRpc(e.to_string()))?;

    // 2. Query on-chain SUI balance.
    let sui_client = SuiClientBuilder::default()
        .build(sui_rpc_url)
        .await
        .map_err(|e| crate::error::Error::SuiRpc(e.to_string()))?;

    let sui_balance = sui_client
        .coin_read_api()
        .get_balance(address, None)
        .await
        .map_err(|e| crate::error::Error::SuiRpc(e.to_string()))?;
    let sui_amount = sui_balance.total_balance as i64;

    // 3. If WAL coin type configured, query WAL balance.
    let wal_amount = if let Some(ref wal_coin_type) = config.wal_coin_type {
        let wal_balance = sui_client
            .coin_read_api()
            .get_balance(address, Some(wal_coin_type.clone()))
            .await
            .map_err(|e| crate::error::Error::SuiRpc(e.to_string()))?;
        wal_balance.total_balance as i64
    } else {
        0
    };

    // 4. Update cached balance.
    db::accounts::set_cached_balance(pool, &account_id, sui_amount, wal_amount).await?;

    tracing::debug!(
        account_id,
        sui_amount,
        wal_amount,
        "reconciled on-chain balance"
    );

    // 5. Timeout stale pending transactions.
    let stale = db::pending_transactions::get_stale_pending_transactions(
        pool,
        config.pending_tx_timeout_minutes,
        100,
    )
    .await?;

    for ptx in &stale {
        if let Err(e) = db::pending_transactions::timeout_pending_transaction(pool, &ptx.id).await {
            tracing::warn!(pending_tx_id = %ptx.id, "failed to timeout stale tx: {e}");
        }
    }

    if !stale.is_empty() {
        tracing::info!(count = stale.len(), "timed out stale pending transactions");
    }

    Ok(())
}
