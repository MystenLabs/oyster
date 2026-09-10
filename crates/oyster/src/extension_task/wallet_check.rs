//! Post-failure wallet verification for the extension worker.
//!
//! The app's only obligation is to keep its Pearl wallet funded; once it
//! has, Oyster owes it a successful extension. So after every failed
//! attempt the worker reads the wallet's WAL and SUI balances and asks:
//! could this extension have been paid for? If yes, the failure is
//! Oyster's problem and is surfaced as a funded failure (error log,
//! `account.extension_failed_funded` audit event, alertable metrics).
//! If no, it is the app's, and the usual `funding_required` webhook
//! path applies.

use std::sync::Arc;

use sui_types::base_types::SuiAddress;
use walrus_sui::{client::SuiReadClient, coin::CoinType};

use crate::{FundingAmount, db::accounts::ExtendWalletState};

/// Outcome of one post-failure wallet check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalletCheck {
    /// Whether the wallet could have paid for the extension.
    pub state: ExtendWalletState,
    /// WAL balance in FROST, when the read succeeded.
    pub wal_balance_frost: Option<u64>,
    /// SUI balance in MIST, when the read succeeded.
    pub sui_balance_mist: Option<u64>,
    /// What the check compared against: `wal_frost` is the exact cost of
    /// the attempted extension, `sui_mist` the configured gas floor.
    pub required: FundingAmount,
}

/// Pure classification: funded iff both balances are known and each
/// covers its requirement. A single failed read makes the verdict
/// `Unknown` rather than guessing in either direction — an `Unfunded`
/// guess would hide an operator-side failure, a `Funded` guess would
/// page for an empty wallet.
pub fn classify(
    wal_balance_frost: Option<u64>,
    sui_balance_mist: Option<u64>,
    required: FundingAmount,
) -> ExtendWalletState {
    match (wal_balance_frost, sui_balance_mist) {
        (Some(wal), Some(sui)) => {
            if wal >= required.wal_frost && sui >= required.sui_mist {
                ExtendWalletState::Funded
            } else {
                ExtendWalletState::Unfunded
            }
        }
        _ => ExtendWalletState::Unknown,
    }
}

/// Read `sender`'s WAL and SUI balances and classify them against
/// `required`. Never fails: a balance read error is logged and folded
/// into an `Unknown` verdict so the caller's failure bookkeeping is
/// unaffected.
pub async fn check_wallet(
    read_client: &Arc<SuiReadClient>,
    sender: SuiAddress,
    required: FundingAmount,
) -> WalletCheck {
    let sui_client = read_client.retriable_sui_client();
    let wal_type = read_client.wal_coin_type();
    let (wal, sui) = tokio::join!(
        sui_client.get_total_balance(sender, CoinType::Wal.as_str(wal_type)),
        sui_client.get_total_balance(sender, CoinType::Sui.as_str(wal_type)),
    );
    let wal_balance_frost = match wal {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!(%sender, error = %e, "wallet check: WAL balance read failed");
            None
        }
    };
    let sui_balance_mist = match sui {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!(%sender, error = %e, "wallet check: SUI balance read failed");
            None
        }
    };
    WalletCheck {
        state: classify(wal_balance_frost, sui_balance_mist, required),
        wal_balance_frost,
        sui_balance_mist,
        required,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQ: FundingAmount = FundingAmount {
        wal_frost: 1_000,
        sui_mist: 20_000_000,
    };

    #[test]
    fn funded_when_both_balances_cover_requirements() {
        assert_eq!(
            classify(Some(1_000), Some(20_000_000), REQ),
            ExtendWalletState::Funded
        );
        assert_eq!(
            classify(Some(u64::MAX), Some(u64::MAX), REQ),
            ExtendWalletState::Funded
        );
    }

    #[test]
    fn unfunded_when_either_balance_is_short() {
        assert_eq!(
            classify(Some(999), Some(20_000_000), REQ),
            ExtendWalletState::Unfunded
        );
        assert_eq!(
            classify(Some(1_000), Some(19_999_999), REQ),
            ExtendWalletState::Unfunded
        );
        assert_eq!(classify(Some(0), Some(0), REQ), ExtendWalletState::Unfunded);
    }

    #[test]
    fn unknown_when_any_read_failed() {
        // Even a wallet that is visibly short on the coin we *could*
        // read stays Unknown: the verdict must never be built on half
        // the evidence.
        assert_eq!(
            classify(None, Some(u64::MAX), REQ),
            ExtendWalletState::Unknown
        );
        assert_eq!(classify(Some(0), None, REQ), ExtendWalletState::Unknown);
        assert_eq!(classify(None, None, REQ), ExtendWalletState::Unknown);
    }

    #[test]
    fn zero_cost_extension_is_funded_by_an_empty_wal_balance() {
        let free = FundingAmount {
            wal_frost: 0,
            sui_mist: 0,
        };
        assert_eq!(classify(Some(0), Some(0), free), ExtendWalletState::Funded);
    }
}
