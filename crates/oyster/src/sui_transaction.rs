use std::sync::Arc;

use sui_sdk::{
    SuiClientBuilder,
    rpc_types::{SuiTransactionBlockResponse, SuiTransactionBlockResponseOptions},
};
use sui_types::{
    base_types::{ObjectID, SuiAddress},
    transaction::{Transaction, TransactionData},
};
use walrus_sui::client::{SuiReadClient, contract_config::ContractConfig};
use walrus_utils::backoff::ExponentialBackoffConfig;

use crate::pearl_client::PearlConnection;

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
    let read_client =
        SuiReadClient::new_for_rpc_urls(&[rpc_url], &contract_config, backoff).await?;
    Ok(Arc::new(read_client))
}

pub async fn resolve_sender_address(
    pearl: &PearlConnection,
    pearl_account_id: &str,
) -> Result<SuiAddress, Box<dyn std::error::Error + Send + Sync>> {
    let resp = pearl.get_account_wallets(pearl_account_id).await?;
    if resp.wallets.is_empty() {
        return Err(format!("pearl account {pearl_account_id} has no wallets").into());
    }
    let addr = resp.wallets[0].address.parse()?;
    Ok(addr)
}

pub async fn sign_and_submit(
    pearl: &PearlConnection,
    pearl_account_id: &str,
    rpc_url: &str,
    tx_data: TransactionData,
) -> Result<SuiTransactionBlockResponse, Box<dyn std::error::Error + Send + Sync>> {
    let tx_bytes = bcs::to_bytes(&tx_data)?;
    let sign_resp = pearl
        .sign_transaction(pearl_account_id, tx_bytes)
        .await
        .map_err(|e| format!("pearl sign error: {e}"))?;

    let signed_tx: Transaction = bcs::from_bytes(&sign_resp.signed_transaction)?;
    let sui_client = SuiClientBuilder::default().build(rpc_url).await?;
    let response = sui_client
        .quorum_driver_api()
        .execute_transaction_block(
            signed_tx,
            SuiTransactionBlockResponseOptions::new()
                .with_effects()
                .with_object_changes(),
            None,
        )
        .await?;
    Ok(response)
}
