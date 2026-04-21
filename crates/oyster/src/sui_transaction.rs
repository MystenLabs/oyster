use std::sync::Arc;

use sui_sdk::{
    SuiClientBuilder,
    rpc_types::{SuiTransactionBlockResponse, SuiTransactionBlockResponseOptions},
};
use sui_types::{
    base_types::{ObjectID, SuiAddress},
    transaction::{Transaction, TransactionData},
};
use walrus_sui::client::{
    SuiReadClient,
    contract_config::ContractConfig,
    dual_client::DEFAULT_CHECKPOINT_WAIT_TIMEOUT,
};
use walrus_utils::backoff::ExponentialBackoffConfig;

use crate::{AccountId, pearl_client::PearlConnection};

/// Build a `SuiReadClient` connected to the given RPC and Walrus contracts.
pub async fn build_sui_read_client(
    rpc_url: &str,
    system_object: ObjectID,
    staking_object: ObjectID,
) -> Result<Arc<SuiReadClient>, Box<dyn std::error::Error + Send + Sync>> {
    let backoff = ExponentialBackoffConfig::new(
        std::time::Duration::from_millis(100),
        std::time::Duration::from_secs(5),
        Some(3),
    );
    let contract_config = ContractConfig::new(system_object, staking_object);
    let read_client = SuiReadClient::new_for_rpc_urls(
        &[rpc_url],
        &contract_config,
        backoff,
        DEFAULT_CHECKPOINT_WAIT_TIMEOUT,
    )
    .await?;
    Ok(Arc::new(read_client))
}

/// Resolve a Pearl account ID to its Sui wallet address.
pub async fn resolve_sender_address(
    pearl: &PearlConnection,
    account_id: &AccountId,
) -> Result<SuiAddress, Box<dyn std::error::Error + Send + Sync>> {
    let address = pearl.get_address(account_id).await?;
    let addr = address.parse()?;
    Ok(addr)
}

/// Sign transaction data via Pearl and submit it to the Sui network.
pub async fn sign_and_submit(
    pearl: &PearlConnection,
    account_id: &AccountId,
    rpc_url: &str,
    tx_data: TransactionData,
) -> Result<SuiTransactionBlockResponse, Box<dyn std::error::Error + Send + Sync>> {
    let tx_bytes = bcs::to_bytes(&tx_data)?;
    let signed_bytes = pearl
        .sign_transaction(account_id, tx_bytes)
        .await
        .map_err(|e| format!("pearl sign error: {e}"))?;

    let signed_tx: Transaction = bcs::from_bytes(&signed_bytes)?;
    let sui_client = SuiClientBuilder::default().build(rpc_url).await?;
    let result = sui_client
        .quorum_driver_api()
        .execute_transaction_block(
            signed_tx,
            SuiTransactionBlockResponseOptions::new()
                .with_effects()
                .with_events()
                .with_object_changes(),
            None,
        )
        .await;

    Ok(result?)
}
