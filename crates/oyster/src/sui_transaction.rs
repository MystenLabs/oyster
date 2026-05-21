use std::{sync::Arc, time::Duration};

use prost_types::FieldMask;
use sui_rpc::{
    Client as SuiGrpcClient,
    proto::sui::rpc::v2::{
        self as v2,
        Bcs as ProtoBcs,
        ExecuteTransactionRequest,
        Transaction as ProtoTransaction,
        UserSignature as ProtoUserSignature,
    },
};
use sui_types::{
    base_types::{ObjectID, SuiAddress},
    digests::TransactionDigest,
    transaction::{Transaction, TransactionData},
};
use walrus_sui::client::{SuiReadClient, contract_config::ContractConfig};
use walrus_utils::backoff::ExponentialBackoffConfig;

use crate::{AccountId, pearl_client::PearlConnection};

/// Protocol-agnostic execute response carrying just the fields Oyster
/// needs from a submitted Sui transaction.
#[derive(Debug, Clone)]
pub struct SignedTxOutcome {
    /// Transaction digest, parsed.
    pub digest: TransactionDigest,
    /// Object changes (the gRPC `ChangedObject` list from
    /// `effects.changed_objects`). Used by `extract_created_by_type`.
    pub changed_objects: Vec<v2::ChangedObject>,
    /// Events emitted by the transaction, in order.
    pub events: Vec<v2::Event>,
}

/// Timeout for `execute_transaction_and_wait_for_checkpoint`. Subsequent
/// JSON-RPC reads in the same operation (e.g. `WalrusPtbBuilder`
/// looking up the just-created `StoragePool`) require the fullnode
/// indexes to have caught up; wait long enough that an in-process Sui
/// test cluster can land its next checkpoint.
const EXECUTE_CHECKPOINT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

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
    let read_client =
        SuiReadClient::new_for_rpc_urls(&[rpc_url], &contract_config, backoff).await?;
    Ok(Arc::new(read_client))
}

/// Build a Sui gRPC client. Mysten fullnodes and the in-process Sui
/// `test-cluster` serve both JSON-RPC and gRPC on the same endpoint, so
/// we reuse the same `rpc_url` we already pass to the read client.
pub fn build_sui_grpc_client(
    rpc_url: &str,
) -> Result<SuiGrpcClient, Box<dyn std::error::Error + Send + Sync>> {
    Ok(SuiGrpcClient::new(rpc_url)?)
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

/// Sign transaction data via Pearl and submit it to the Sui network via
/// gRPC, returning the minimal protocol-agnostic outcome Oyster needs.
pub async fn sign_and_submit(
    pearl: &PearlConnection,
    account_id: &AccountId,
    rpc_url: &str,
    tx_data: TransactionData,
) -> Result<SignedTxOutcome, Box<dyn std::error::Error + Send + Sync>> {
    // Sign via Pearl. Pearl returns a BCS-serialized sui-types
    // `Transaction` envelope (intent message + signatures).
    let tx_bytes = bcs::to_bytes(&tx_data)?;
    let signed_bytes = pearl
        .sign_transaction(account_id, tx_bytes)
        .await
        .map_err(|e| format!("pearl sign error: {e}"))?;
    let signed_tx: Transaction = bcs::from_bytes(&signed_bytes)?;

    // Convert sui-sdk Transaction → sui-rpc proto. The proto request
    // carries the BCS-serialized `TransactionData` in `transaction.bcs`
    // and the BCS-serialized signatures separately in `signatures`.
    let tx_data_bytes = bcs::to_bytes(signed_tx.transaction_data())?;
    let proto_tx =
        ProtoTransaction::default().with_bcs(ProtoBcs::default().with_value(tx_data_bytes));
    let proto_sigs: Vec<ProtoUserSignature> = signed_tx
        .tx_signatures()
        .iter()
        .map(|sig| {
            ProtoUserSignature::default()
                .with_bcs(ProtoBcs::default().with_value(sig.as_ref().to_vec()))
        })
        .collect();

    let mut client = build_sui_grpc_client(rpc_url)?;
    let request = ExecuteTransactionRequest::new(proto_tx)
        .with_signatures(proto_sigs)
        .with_read_mask(FieldMask {
            paths: vec![
                "digest".to_string(),
                "effects.status".to_string(),
                "effects.changed_objects".to_string(),
                "events".to_string(),
            ],
        });
    // Wait for the transaction to land in a checkpoint so that follow-up
    // JSON-RPC reads (Walrus's `SuiReadClient` is still on JSON-RPC) see
    // the just-created objects. Matches the read-your-writes guarantee
    // the legacy `quorum_driver_api` execute path used to provide.
    let response = client
        .execute_transaction_and_wait_for_checkpoint(request, EXECUTE_CHECKPOINT_WAIT_TIMEOUT)
        .await
        .map_err(|e| format!("execute_transaction: {e}"))?
        .into_inner();

    let executed = response
        .transaction
        .ok_or("ExecuteTransactionResponse missing transaction field")?;

    let digest_str = executed
        .digest
        .as_deref()
        .ok_or("ExecuteTransactionResponse missing digest")?;
    let digest: TransactionDigest = digest_str
        .parse()
        .map_err(|e| format!("invalid digest {digest_str}: {e}"))?;

    let effects = executed.effects.unwrap_or_default();
    let changed_objects = effects.changed_objects;
    let events = executed.events.map(|ev| ev.events).unwrap_or_default();

    Ok(SignedTxOutcome {
        digest,
        changed_objects,
        events,
    })
}
