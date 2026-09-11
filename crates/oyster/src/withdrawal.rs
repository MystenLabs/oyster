//! Admin fund withdrawals: moving SUI/WAL out of an account's Pearl
//! wallet to a pre-registered destination.
//!
//! This module is the on-chain half. The policy half — who may ask, who
//! may approve, and where funds may go — lives in
//! `routes::withdrawals` and is summarized in migration 027. In short,
//! fund egress requires two independent admin keys and a destination
//! that was registered ahead of a cooldown, because a single leaked
//! admin key must not be enough to drain wallets.
//!
//! Two shapes of transfer:
//! - fixed amounts: `transfer_sui` splits the SUI amount off the gas
//!   coin; WAL is paid from coins selected to cover the amount. Gas is
//!   selected by the Walrus helper with the SUI amount included in its
//!   minimum, so the wallet keeps working afterwards.
//! - drain: every SUI coin is used as gas payment and the merged gas
//!   coin is transferred whole (the fee comes off the top); every WAL
//!   coin is transferred as an object. The wallet is left empty, which
//!   is the "close the account, refund the user" case.

use sui_types::{
    base_types::SuiAddress,
    digests::TransactionDigest,
    programmable_transaction_builder::ProgrammableTransactionBuilder,
    transaction::{Command, ObjectArg, TransactionData, TransactionKind},
};
use walrus_sui::{
    client::{SuiReadClient, transaction_builder::build_transaction_data_with_min_gas_balance},
    coin::CoinType,
};

use crate::{
    AccountId,
    pearl_client::PearlConnection,
    sui_transaction::{self, SignAndSubmitError},
};

/// Sui's cap on gas-payment objects per transaction. A wallet with more
/// SUI coin objects than this is drained in several `drain` runs.
const MAX_GAS_PAYMENT_OBJECTS: usize = 256;

/// What to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithdrawalAmounts {
    /// SUI in MIST, or `None`.
    pub sui_mist: Option<u64>,
    /// WAL in FROST, or `None`.
    pub wal_frost: Option<u64>,
    /// Move every SUI and WAL coin instead (amounts ignored).
    pub drain: bool,
}

impl WithdrawalAmounts {
    /// Reject empty, zero, and contradictory requests. Returns a
    /// caller-facing message on failure.
    pub fn validate(&self) -> Result<(), String> {
        if self.drain {
            if self.sui_mist.is_some() || self.wal_frost.is_some() {
                return Err("`all` cannot be combined with `sui_mist`/`wal_frost`".into());
            }
            return Ok(());
        }
        match (self.sui_mist, self.wal_frost) {
            (None, None) => Err("specify `sui_mist`, `wal_frost`, or `all: true`".into()),
            (Some(0), _) | (_, Some(0)) => Err("amounts must be greater than zero".into()),
            _ => Ok(()),
        }
    }
}

/// Why a withdrawal could not be executed.
#[derive(Debug, thiserror::Error)]
pub enum WithdrawalExecError {
    /// The wallet does not hold enough SUI (amount + gas) or WAL.
    #[error("insufficient balance: {0}")]
    InsufficientBalance(String),
    /// Refusing a transfer that would be a no-op or malformed.
    #[error("invalid withdrawal: {0}")]
    Invalid(String),
    /// Pearl, Sui RPC, or on-chain execution failure.
    #[error("upstream: {0}")]
    Upstream(String),
}

fn map_build_err(e: walrus_sui::client::SuiClientError) -> WithdrawalExecError {
    use walrus_sui::client::SuiClientError;
    match e {
        SuiClientError::NoCompatibleWalCoins => {
            WithdrawalExecError::InsufficientBalance("not enough WAL in the wallet".into())
        }
        SuiClientError::NoCompatibleGasCoins(_) => WithdrawalExecError::InsufficientBalance(
            "not enough SUI in the wallet to cover the amount plus gas".into(),
        ),
        other => WithdrawalExecError::Upstream(other.to_string()),
    }
}

/// Build, sign (via Pearl, under the account's `key_version`) and submit
/// the transfer. Returns the transaction digest once it has landed in a
/// checkpoint.
pub async fn execute_withdrawal(
    read_client: &SuiReadClient,
    pearl: &PearlConnection,
    rpc_url: &str,
    account_id: &AccountId,
    key_version: u32,
    destination: SuiAddress,
    amounts: &WithdrawalAmounts,
) -> Result<TransactionDigest, WithdrawalExecError> {
    amounts.validate().map_err(WithdrawalExecError::Invalid)?;
    let sender = sui_transaction::resolve_sender_address(pearl, account_id, key_version)
        .await
        .map_err(|e| WithdrawalExecError::Upstream(format!("resolve sender address: {e}")))?;
    if sender == destination {
        return Err(WithdrawalExecError::Invalid(
            "destination is the account's own wallet".into(),
        ));
    }

    let tx_data = if amounts.drain {
        build_drain(read_client, sender, destination).await?
    } else {
        build_fixed(read_client, sender, destination, amounts).await?
    };

    match sui_transaction::sign_and_submit(pearl, account_id, key_version, rpc_url, tx_data).await {
        Ok(outcome) => Ok(outcome.digest),
        Err(SignAndSubmitError::ExecutionFailure(f)) => {
            Err(WithdrawalExecError::Upstream(f.to_string()))
        }
        Err(SignAndSubmitError::Other(e)) => Err(WithdrawalExecError::Upstream(e.to_string())),
    }
}

async fn build_fixed(
    read_client: &SuiReadClient,
    sender: SuiAddress,
    destination: SuiAddress,
    amounts: &WithdrawalAmounts,
) -> Result<TransactionData, WithdrawalExecError> {
    let mut pt = ProgrammableTransactionBuilder::new();
    if let Some(sui) = amounts.sui_mist {
        pt.transfer_sui(destination, Some(sui));
    }
    if let Some(wal) = amounts.wal_frost {
        let coins = read_client
            .get_coins_with_total_balance(sender, CoinType::Wal, wal, vec![])
            .await
            .map_err(map_build_err)?;
        let refs = coins.iter().map(|c| c.object_ref()).collect();
        pt.pay(refs, vec![destination], vec![wal])
            .map_err(|e| WithdrawalExecError::Upstream(format!("build WAL payment: {e}")))?;
    }
    build_transaction_data_with_min_gas_balance(
        pt.finish(),
        read_client,
        sender,
        None,
        0,
        amounts.sui_mist.unwrap_or(0),
        None,
    )
    .await
    .map_err(map_build_err)
}

async fn build_drain(
    read_client: &SuiReadClient,
    sender: SuiAddress,
    destination: SuiAddress,
) -> Result<TransactionData, WithdrawalExecError> {
    let sui_client = read_client.retriable_sui_client();
    let sui_total = sui_client
        .get_total_balance(sender, "0x2::sui::SUI")
        .await
        .map_err(|e| WithdrawalExecError::Upstream(format!("SUI balance: {e}")))?;
    if sui_total == 0 {
        return Err(WithdrawalExecError::InsufficientBalance(
            "wallet holds no SUI to pay for the transfer".into(),
        ));
    }
    let wal_total = sui_client
        .get_total_balance(sender, read_client.wal_coin_type())
        .await
        .map_err(|e| WithdrawalExecError::Upstream(format!("WAL balance: {e}")))?;

    // Every SUI coin becomes gas payment (Sui merges them into the gas
    // coin), and the gas coin is transferred whole after the fee.
    let mut sui_coins = read_client
        .get_coins_with_total_balance(sender, CoinType::Sui, sui_total, vec![])
        .await
        .map_err(map_build_err)?;
    sui_coins.truncate(MAX_GAS_PAYMENT_OBJECTS);
    let gas_payment: Vec<_> = sui_coins.iter().map(|c| c.object_ref()).collect();

    let mut pt = ProgrammableTransactionBuilder::new();
    pt.transfer_sui(destination, None);
    if wal_total > 0 {
        let wal_coins = read_client
            .get_coins_with_total_balance(sender, CoinType::Wal, wal_total, vec![])
            .await
            .map_err(map_build_err)?;
        let mut args = Vec::with_capacity(wal_coins.len());
        for c in &wal_coins {
            args.push(
                pt.obj(ObjectArg::ImmOrOwnedObject(c.object_ref()))
                    .map_err(|e| WithdrawalExecError::Upstream(format!("WAL obj arg: {e}")))?,
            );
        }
        let recipient = pt
            .pure(destination)
            .map_err(|e| WithdrawalExecError::Upstream(format!("recipient arg: {e}")))?;
        pt.command(Command::TransferObjects(args, recipient));
    }
    let pt = pt.finish();

    let budget = sui_client
        .gas_budget_and_price(
            None,
            sender,
            TransactionKind::ProgrammableTransaction(pt.clone()),
        )
        .await
        .map_err(|e| WithdrawalExecError::Upstream(format!("gas estimate: {e}")))?;
    let gas_in_payment: u64 = sui_coins.iter().map(|c| c.balance).sum();
    if gas_in_payment < budget.gas_budget {
        return Err(WithdrawalExecError::InsufficientBalance(format!(
            "wallet SUI ({gas_in_payment} MIST) is below the gas budget ({} MIST)",
            budget.gas_budget
        )));
    }
    Ok(TransactionData::new_programmable(
        sender,
        gas_payment,
        pt,
        budget.gas_budget,
        budget.gas_price,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_amounts() {
        let ok = |s, w, d| WithdrawalAmounts {
            sui_mist: s,
            wal_frost: w,
            drain: d,
        };
        assert!(ok(Some(1), None, false).validate().is_ok());
        assert!(ok(None, Some(1), false).validate().is_ok());
        assert!(ok(Some(1), Some(1), false).validate().is_ok());
        assert!(ok(None, None, true).validate().is_ok());
        assert!(ok(None, None, false).validate().is_err());
        assert!(ok(Some(0), None, false).validate().is_err());
        assert!(ok(Some(1), Some(0), false).validate().is_err());
        assert!(ok(Some(1), None, true).validate().is_err());
    }
}
