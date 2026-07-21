use std::{fmt, sync::Arc, time::Duration};

use prost_types::FieldMask;
use sui_rpc::{
    Client as SuiGrpcClient,
    client::ExecuteAndWaitError,
    proto::sui::rpc::v2::{
        self as v2, Bcs as ProtoBcs, ExecuteTransactionRequest, Transaction as ProtoTransaction,
        UserSignature as ProtoUserSignature, execution_error,
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

/// Structured information about a transaction that the RPC accepted
/// but that aborted on-chain.
#[derive(Debug, Clone)]
pub struct TxExecutionFailure {
    /// Digest of the failed transaction.
    pub digest: TransactionDigest,
    /// Human-readable `description` from the gRPC `ExecutionError`
    /// (e.g. "Move Runtime Abort. Location: …, Abort Code: 6").
    pub description: Option<String>,
    /// Top-level error kind from the proto (e.g. `MoveAbort`).
    pub kind: execution_error::ExecutionErrorKind,
    /// Structured `MoveAbort` payload when `kind == MoveAbort`.
    pub abort: Option<v2::MoveAbort>,
}

impl TxExecutionFailure {
    /// Returns `true` if this failure is a Move abort whose
    /// `location.module` and `location.function_name` match the given
    /// strings and whose `abort_code` equals `code`. Phase 2 uses this
    /// to dispatch on `storage_pool::add_blob` + code `6`.
    pub fn is_move_abort(&self, module: &str, function: &str, code: u64) -> bool {
        let Some(abort) = &self.abort else {
            return false;
        };
        if abort.abort_code != Some(code) {
            return false;
        }
        let Some(loc) = &abort.location else {
            return false;
        };
        loc.module.as_deref() == Some(module) && loc.function_name.as_deref() == Some(function)
    }
}

impl fmt::Display for TxExecutionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sui tx {} failed: {}",
            self.digest,
            self.kind.as_str_name(),
        )?;
        if let Some(abort) = &self.abort {
            let module = abort
                .location
                .as_ref()
                .and_then(|l| l.module.as_deref())
                .unwrap_or("?");
            let function = abort
                .location
                .as_ref()
                .and_then(|l| l.function_name.as_deref())
                .unwrap_or("?");
            let code = abort.abort_code.unwrap_or_default();
            write!(f, " at {module}::{function} code {code}")?;
        }
        if let Some(desc) = &self.description {
            write!(f, " ({desc})")?;
        }
        Ok(())
    }
}

/// Errors returned by [`sign_and_submit`].
#[derive(Debug, thiserror::Error)]
pub enum SignAndSubmitError {
    /// The gRPC call succeeded but the transaction aborted on-chain.
    /// Boxed because the gRPC `MoveAbort` payload is large relative to
    /// the other variant.
    #[error("{0}")]
    ExecutionFailure(Box<TxExecutionFailure>),
    /// Any other failure (Pearl error, BCS, transport, etc.).
    #[error("{0}")]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl From<Box<dyn std::error::Error + Send + Sync>> for SignAndSubmitError {
    fn from(e: Box<dyn std::error::Error + Send + Sync>) -> Self {
        SignAndSubmitError::Other(e)
    }
}

impl From<String> for SignAndSubmitError {
    fn from(e: String) -> Self {
        SignAndSubmitError::Other(e.into())
    }
}

impl From<&str> for SignAndSubmitError {
    fn from(e: &str) -> Self {
        SignAndSubmitError::Other(e.into())
    }
}

impl From<bcs::Error> for SignAndSubmitError {
    fn from(e: bcs::Error) -> Self {
        SignAndSubmitError::Other(Box::new(e))
    }
}

impl From<tonic::Status> for SignAndSubmitError {
    fn from(e: tonic::Status) -> Self {
        SignAndSubmitError::Other(Box::new(e))
    }
}

impl From<ExecuteAndWaitError> for SignAndSubmitError {
    fn from(e: ExecuteAndWaitError) -> Self {
        SignAndSubmitError::Other(format!("{e}").into())
    }
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
/// gRPC. Returns [`SignAndSubmitError::ExecutionFailure`] when the gRPC
/// call succeeded but the transaction aborted on-chain, so callers can
/// dispatch on the structured `MoveAbort`.
pub async fn sign_and_submit(
    pearl: &PearlConnection,
    account_id: &AccountId,
    rpc_url: &str,
    tx_data: TransactionData,
) -> Result<SignedTxOutcome, SignAndSubmitError> {
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
        .await?
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

    // gRPC returned OK but the transaction may still have aborted on-chain.
    // Surface the structured failure so callers can pattern-match on the
    // MoveAbort (Phase 2 dispatches on storage_pool::add_blob + code 6).
    if let Some(status) = &effects.status
        && status.success == Some(false)
    {
        let err = status.error.clone().unwrap_or_default();
        let kind = err.kind();
        let abort = match err.error_details {
            Some(execution_error::ErrorDetails::Abort(a)) => Some(a),
            _ => None,
        };
        let failure = TxExecutionFailure {
            digest,
            description: err.description,
            kind,
            abort,
        };
        tracing::warn!(
            tx_digest = %failure.digest,
            kind = %failure.kind.as_str_name(),
            abort = ?failure.abort,
            description = ?failure.description,
            "sui transaction aborted on-chain",
        );
        return Err(SignAndSubmitError::ExecutionFailure(Box::new(failure)));
    }

    let changed_objects = effects.changed_objects;
    let events = executed.events.map(|ev| ev.events).unwrap_or_default();

    Ok(SignedTxOutcome {
        digest,
        changed_objects,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_digest() -> TransactionDigest {
        TransactionDigest::new([7u8; 32])
    }

    fn move_abort_failure() -> TxExecutionFailure {
        let location = v2::MoveLocation::default()
            .with_module("storage_pool")
            .with_function_name("add_blob");
        let abort = v2::MoveAbort::default()
            .with_abort_code(6)
            .with_location(location);
        TxExecutionFailure {
            digest: synth_digest(),
            description: Some("Move Runtime Abort. Abort Code: 6".into()),
            kind: execution_error::ExecutionErrorKind::MoveAbort,
            abort: Some(abort),
        }
    }

    #[test]
    fn tx_execution_failure_display_without_abort() {
        let failure = TxExecutionFailure {
            digest: synth_digest(),
            description: None,
            kind: execution_error::ExecutionErrorKind::InsufficientGas,
            abort: None,
        };
        let s = failure.to_string();
        assert!(
            s.contains(&synth_digest().to_string()),
            "missing digest: {s}"
        );
        assert!(s.contains("INSUFFICIENT_GAS"), "missing kind name: {s}");
        assert!(
            !s.contains(" code "),
            "should not include code suffix without abort: {s}"
        );
    }

    #[test]
    fn tx_execution_failure_display_with_full_abort() {
        let s = move_abort_failure().to_string();
        assert!(s.contains("MOVE_ABORT"), "missing kind name: {s}");
        assert!(
            s.contains("storage_pool::add_blob"),
            "missing module::function: {s}"
        );
        assert!(s.contains("code 6"), "missing abort code: {s}");
    }

    #[test]
    fn is_move_abort_matches_exact() {
        let failure = move_abort_failure();
        assert!(failure.is_move_abort("storage_pool", "add_blob", 6));
        assert!(!failure.is_move_abort("storage_pool", "add_blob", 7));
        assert!(!failure.is_move_abort("storage_pool", "remove_blob", 6));
        assert!(!failure.is_move_abort("system", "add_blob", 6));
    }

    #[test]
    fn is_move_abort_false_when_no_abort() {
        let failure = TxExecutionFailure {
            digest: synth_digest(),
            description: None,
            kind: execution_error::ExecutionErrorKind::MoveAbort,
            abort: None,
        };
        assert!(!failure.is_move_abort("storage_pool", "add_blob", 6));
    }
}
