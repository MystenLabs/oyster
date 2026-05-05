//! Helpers for estimating the WAL + SUI cost of extending a `StoragePool`.
//!
//! This is the "send roughly this much" signal Oyster forwards to Harbor in
//! the `account.funding_required` webhook. It is intentionally not a precise
//! gas estimate — we don't dry-run the PTB.

use std::sync::Arc;

use walrus_sui::client::{ReadClient, SuiReadClient};

use crate::db::accounts::ExpiringPool;

/// Fixed SUI buffer attached to every funding-required notification, in MIST.
///
/// Roughly enough gas to cover ~100 `extend_storage_pool` PTBs at typical
/// reference gas prices. Harbor tops up the wallet with at least this much
/// SUI so subsequent extensions don't immediately re-trigger the webhook.
pub const SUI_GAS_PER_EXTENSION_BUFFER_MIST: u64 = 100_000_000;

/// Outputs of `compute_extension_cost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionCost {
    /// WAL needed for the extension, in FROST units.
    pub wal_frost: u64,
    /// SUI buffer to keep the wallet solvent, in MIST units.
    pub sui_mist: u64,
}

/// Errors returned while estimating extension cost.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionCostError {
    /// Failed to fetch the current `storage_price_per_unit_size` from Walrus.
    #[error("failed to read storage price: {0}")]
    StoragePrice(#[from] walrus_sui::client::SuiClientError),
}

/// Compute the WAL + SUI cost of extending `pool` by `extend_epochs` epochs.
///
/// `storage_price_per_unit_size` is cached aggressively at the
/// `SuiReadClient` layer, so per-cycle calls are cheap.
pub async fn compute_extension_cost(
    read_client: &Arc<SuiReadClient>,
    pool: &ExpiringPool,
    extend_epochs: u32,
) -> Result<ExtensionCost, ExtensionCostError> {
    let storage_price = read_client.storage_price_per_unit_size().await?;
    let wal_frost = walrus_sui::utils::price_for_encoded_length(
        pool.pool_reserved_encoded_bytes.max(0) as u64,
        storage_price,
        extend_epochs,
    );
    Ok(ExtensionCost {
        wal_frost,
        sui_mist: SUI_GAS_PER_EXTENSION_BUFFER_MIST,
    })
}

#[cfg(test)]
mod tests {
    use walrus_sui::utils::{BYTES_PER_UNIT_SIZE, price_for_encoded_length};

    #[test]
    fn price_for_encoded_length_matches_one_unit() {
        // One full unit, 1 frost per unit per epoch, 4 epochs.
        let price = price_for_encoded_length(BYTES_PER_UNIT_SIZE, 1, 4);
        assert_eq!(price, 4);
    }

    #[test]
    fn price_for_encoded_length_rounds_up_to_unit() {
        // Sub-unit length still pays a full unit.
        let price = price_for_encoded_length(1, 1_000, 3);
        assert_eq!(price, 3_000);
    }

    #[test]
    fn price_for_encoded_length_scales_with_units() {
        // Two units, 5 frost per unit per epoch, 7 epochs.
        let price = price_for_encoded_length(2 * BYTES_PER_UNIT_SIZE, 5, 7);
        assert_eq!(price, 2 * 5 * 7);
    }
}
