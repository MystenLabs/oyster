//! Admin-only on-chain `StoragePool` operations.
//!
//! Phase 4 added the ability to shrink a per-account on-chain
//! `StoragePool` when an admin lowers the account's
//! `max_unencoded_bytes` cap below the currently-reserved encoded
//! capacity. Walrus's upstream [`WalrusPtbBuilder`] (pinned to
//! `testnet-v1.52.0`) exposes `create_storage_pool` and
//! `increase_storage_pool_capacity` publicly but not
//! `decrease_storage_pool_capacity_by_size`, so we build the PTB
//! directly here using `sui_types::ProgrammableTransactionBuilder`
//! and lean on the public
//! [`walrus_sui::client::transaction_builder::build_transaction_data_with_min_gas_balance`]
//! for gas estimation + gas-coin selection.
//!
//! The extracted `Storage` object is transferred back to the
//! sender's address (the Pearl-managed wallet). This is intentional:
//! the storage capacity has been paid for and is recoverable via
//! [`system::increase_storage_pool_capacity_with_storage`] in a
//! future tooling pass. The contract emits no event for the
//! extracted object.
//!
//! [`WalrusPtbBuilder`]: walrus_sui::client::transaction_builder::WalrusPtbBuilder
//! [`system::increase_storage_pool_capacity_with_storage`]: https://github.com/MystenLabs/walrus/blob/main/testnet-contracts/walrus/sources/system.move

use sui_types::{
    Identifier,
    base_types::ObjectID,
    digests::TransactionDigest,
    programmable_transaction_builder::ProgrammableTransactionBuilder,
    transaction::{Command, ObjectArg, SharedObjectMutability},
};
use walrus_sui::client::{
    SuiReadClient, transaction_builder::build_transaction_data_with_min_gas_balance,
};

use crate::{
    AccountId,
    pearl_client::PearlConnection,
    sui_transaction::{self, SignAndSubmitError},
};

/// Outcome of a successful `decrease_storage_pool_capacity_by_size`
/// PTB submission.
#[derive(Debug, Clone)]
pub struct DecreaseOutcome {
    /// Digest of the submitted shrink transaction.
    pub tx_digest: TransactionDigest,
}

/// Errors returned by [`decrease_storage_pool_capacity`].
#[derive(Debug, thiserror::Error)]
pub enum DecreaseError {
    /// The on-chain shrink aborted with the
    /// `storage_pool::decrease_capacity_by_size` `EInsufficientCapacity`
    /// (code 6) assertion — i.e. `used_encoded > storage_size −
    /// extract_size`. The caller maps this to HTTP 400.
    #[error("would orphan stored data ({description})")]
    WouldOrphan {
        /// Human-readable description from the gRPC `ExecutionError`.
        description: String,
        /// The Move abort code (always `6` for this variant).
        abort_code: u64,
    },
    /// Server-internal invariant violation (e.g. pool not owned by
    /// the resolved sender, defensive zero-extract guard). The caller
    /// maps these to HTTP 500-class responses.
    #[error("internal: {0}")]
    Internal(String),
    /// Upstream Sui/Walrus error not covered by the other variants.
    /// The caller maps this to HTTP 502.
    #[error("upstream: {0}")]
    Upstream(String),
}

/// Build, sign, and submit a PTB that calls
/// `system::decrease_storage_pool_capacity_by_size(pool, extract_size)`
/// and transfers the returned `Storage` object back to the
/// Pearl-managed sender's address.
///
/// Pre-conditions enforced by the caller (the `update_max_storage`
/// route handler):
/// * `extract_size > 0`.
/// * The DB's notion of `storage_pool_object_id` for `account_id` is
///   the on-chain pool we want to shrink.
///
/// Pre-conditions enforced inside this function:
/// * The fetched `StoragePool` object's owner is the Pearl-derived
///   sender address — guards against DB corruption or a wrong-account
///   request reaching the chain with a malformed `ObjectArg`.
pub async fn decrease_storage_pool_capacity(
    read_client: &SuiReadClient,
    pearl: &PearlConnection,
    rpc_url: &str,
    account_id: &AccountId,
    key_version: u32,
    pool_object_id: ObjectID,
    extract_size: u64,
) -> Result<DecreaseOutcome, DecreaseError> {
    // Defensive: the route handler is expected to skip the on-chain
    // shrink when extract_size == 0 (the contract aborts with
    // `EZeroExtractSize`, code 2), but treat it as an internal error
    // if it slips through.
    if extract_size == 0 {
        return Err(DecreaseError::Internal(
            "extract_size must be > 0 (would abort with EZeroExtractSize)".into(),
        ));
    }

    let sender = sui_transaction::resolve_sender_address(pearl, account_id, key_version)
        .await
        .map_err(|e| DecreaseError::Internal(format!("resolve sender address: {e}")))?;

    let sui_client = read_client.retriable_sui_client();

    // Fetch the system object's initial shared version. The read
    // client caches the system object's ID but not its initial version
    // (which is needed for `ObjectArg::SharedObject`), so do one RPC.
    let system_initial_version = sui_client
        .get_shared_object_initial_version(read_client.system_object_id())
        .await
        .map_err(|e| {
            DecreaseError::Upstream(format!("get_shared_object_initial_version system: {e}"))
        })?;

    // Fetch the StoragePool object ref. The pool is owned (transferred
    // to sender on creation; see `direct_walrus_store::create_pool_for_account`),
    // so we need (id, version, digest) plus an owner check.
    let pool_owner = sui_client
        .get_object_owner_address(pool_object_id)
        .await
        .map_err(|e| DecreaseError::Upstream(format!("get_object_owner_address pool: {e}")))?;
    // `None` means the pool has no address owner (shared or immutable),
    // which is just as malformed here as a mismatched owner.
    if pool_owner != Some(sender) {
        return Err(DecreaseError::Internal(format!(
            "pool {pool_object_id} owner {pool_owner:?} != sender {sender}; refusing to submit \
             a malformed ObjectArg",
        )));
    }
    let pool_ref = sui_client
        .get_object_ref(pool_object_id)
        .await
        .map_err(|e| DecreaseError::Upstream(format!("get_object_ref pool: {e}")))?;

    let walrus_package_id = read_client.walrus_package_id();

    let mut builder = ProgrammableTransactionBuilder::new();
    let system_arg = builder
        .obj(ObjectArg::SharedObject {
            id: read_client.system_object_id(),
            initial_shared_version: system_initial_version,
            mutability: SharedObjectMutability::Immutable,
        })
        .map_err(|e| DecreaseError::Internal(format!("system obj arg: {e}")))?;
    let pool_arg = builder
        .obj(ObjectArg::ImmOrOwnedObject(pool_ref))
        .map_err(|e| DecreaseError::Internal(format!("pool obj arg: {e}")))?;
    let size_arg = builder
        .pure(extract_size)
        .map_err(|e| DecreaseError::Internal(format!("size pure arg: {e}")))?;

    let storage_result = builder.programmable_move_call(
        walrus_package_id,
        Identifier::new("system")
            .map_err(|e| DecreaseError::Internal(format!("module ident: {e}")))?,
        Identifier::new("decrease_storage_pool_capacity_by_size")
            .map_err(|e| DecreaseError::Internal(format!("function ident: {e}")))?,
        vec![],
        vec![system_arg, pool_arg, size_arg],
    );

    // Transfer the extracted Storage object back to the sender so the
    // paid-for capacity isn't lost. See the module doc for context.
    let recipient_arg = builder
        .pure(sender)
        .map_err(|e| DecreaseError::Internal(format!("recipient pure arg: {e}")))?;
    builder.command(Command::TransferObjects(
        vec![storage_result],
        recipient_arg,
    ));

    let pt = builder.finish();

    let tx_data = build_transaction_data_with_min_gas_balance(
        pt,
        read_client,
        sender,
        None, // gas_budget
        0,    // minimum_gas_coin_balance
        0,    // tx_sui_cost (no SUI moved by this PTB)
        None, // gas_coins (let the helper pick)
    )
    .await
    .map_err(|e| DecreaseError::Upstream(format!("build_transaction_data: {e}")))?;

    match sui_transaction::sign_and_submit(pearl, account_id, key_version, rpc_url, tx_data).await {
        Ok(outcome) => Ok(DecreaseOutcome {
            tx_digest: outcome.digest,
        }),
        Err(SignAndSubmitError::ExecutionFailure(f)) => {
            // `decrease_capacity_by_size` aborts with code 6 when
            // `used_encoded > storage_size − extract_size` — surfaces
            // as "would orphan stored data" 400 in the route layer.
            if f.is_move_abort("storage_pool", "decrease_capacity_by_size", 6) {
                let description = f.description.clone().unwrap_or_else(|| f.to_string());
                return Err(DecreaseError::WouldOrphan {
                    description,
                    abort_code: 6,
                });
            }
            // `system::decrease_storage_pool_capacity_by_size` asserts
            // `EZeroExtractSize` (code 2) — we guard against that above,
            // so seeing it here is an invariant violation.
            if f.is_move_abort("system", "decrease_storage_pool_capacity_by_size", 2) {
                return Err(DecreaseError::Internal(format!(
                    "unexpected EZeroExtractSize from on-chain shrink: {f}",
                )));
            }
            Err(DecreaseError::Upstream(f.to_string()))
        }
        Err(SignAndSubmitError::Other(e)) => Err(DecreaseError::Upstream(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use sui_rpc::proto::sui::rpc::v2::{self as v2, execution_error};

    use super::*;
    use crate::sui_transaction::TxExecutionFailure;

    fn synth_digest() -> TransactionDigest {
        TransactionDigest::new([9u8; 32])
    }

    fn move_abort_failure(module: &str, function: &str, code: u64) -> TxExecutionFailure {
        let location = v2::MoveLocation::default()
            .with_module(module)
            .with_function_name(function);
        let abort = v2::MoveAbort::default()
            .with_abort_code(code)
            .with_location(location);
        TxExecutionFailure {
            digest: synth_digest(),
            description: Some(format!("Move Runtime Abort. Abort Code: {code}")),
            kind: execution_error::ExecutionErrorKind::MoveAbort,
            abort: Some(abort),
        }
    }

    /// Dispatching path mirroring the production match arms — pure,
    /// no RPC. Kept in-sync with [`decrease_storage_pool_capacity`].
    fn classify(failure: TxExecutionFailure) -> DecreaseError {
        if failure.is_move_abort("storage_pool", "decrease_capacity_by_size", 6) {
            let description = failure
                .description
                .clone()
                .unwrap_or_else(|| failure.to_string());
            return DecreaseError::WouldOrphan {
                description,
                abort_code: 6,
            };
        }
        if failure.is_move_abort("system", "decrease_storage_pool_capacity_by_size", 2) {
            return DecreaseError::Internal(format!(
                "unexpected EZeroExtractSize from on-chain shrink: {failure}",
            ));
        }
        DecreaseError::Upstream(failure.to_string())
    }

    #[test]
    fn classify_storage_pool_code_6_is_would_orphan() {
        let failure = move_abort_failure("storage_pool", "decrease_capacity_by_size", 6);
        match classify(failure) {
            DecreaseError::WouldOrphan {
                description,
                abort_code,
            } => {
                assert_eq!(abort_code, 6);
                assert!(
                    description.contains("Abort Code: 6"),
                    "description should include abort code: {description}",
                );
            }
            other => panic!("expected WouldOrphan, got {other:?}"),
        }
    }

    #[test]
    fn classify_system_code_2_is_internal() {
        let failure = move_abort_failure("system", "decrease_storage_pool_capacity_by_size", 2);
        match classify(failure) {
            DecreaseError::Internal(msg) => {
                assert!(
                    msg.contains("EZeroExtractSize"),
                    "expected internal EZeroExtractSize message: {msg}"
                );
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[test]
    fn classify_unrelated_abort_is_upstream() {
        // Different module/function — should fall through to Upstream.
        let failure = move_abort_failure("storage_pool", "add_blob", 6);
        match classify(failure) {
            DecreaseError::Upstream(_) => {}
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[test]
    fn classify_non_abort_failure_is_upstream() {
        let failure = TxExecutionFailure {
            digest: synth_digest(),
            description: Some("insufficient gas".into()),
            kind: execution_error::ExecutionErrorKind::InsufficientGas,
            abort: None,
        };
        match classify(failure) {
            DecreaseError::Upstream(msg) => {
                assert!(
                    msg.contains("INSUFFICIENT_GAS"),
                    "expected upstream message to include kind: {msg}"
                );
            }
            other => panic!("expected Upstream, got {other:?}"),
        }
    }
}
