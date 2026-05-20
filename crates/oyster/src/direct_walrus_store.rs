//! Direct Walrus blob store: writes to Walrus via on-chain Sui PTBs against a
//! per-account `StoragePool`.
//!
//! Walrus bills storage in `walrus_sui::utils::BYTES_PER_UNIT_SIZE` (1 MiB)
//! units — the `storage_units_from_size!` macro in the Move contract rounds
//! every reservation's payment up to the nearest unit, while the stored
//! `reserved_encoded_capacity_bytes` is kept at literal byte granularity. We
//! round our own reservation requests up to the unit so the stored and billed
//! views agree and subsequent small uploads don't trigger redundant
//! `increase_storage_pool_capacity` PTBs.

use std::{future::Future, pin::Pin, sync::Arc};

use sui_sdk::rpc_types::ObjectChange;
use sui_types::base_types::{ObjectID, SuiAddress};
use walrus_core::{Epoch, encoding::EncodingFactory as _, messages::BlobPersistenceType};
use walrus_sdk::{
    config::ClientConfig,
    error::ClientErrorKind,
    node_client::WalrusNodeClient,
    uploader::TailHandling,
};
use walrus_sui::{
    client::{
        BlobObjectMetadata,
        BlobPersistence,
        ReadClient,
        SuiReadClient,
        transaction_builder::WalrusPtbBuilder,
    },
    types::PooledBlob,
    utils::{BYTES_PER_UNIT_SIZE, storage_units_from_size},
};

use crate::{
    AccountId,
    blob_store::{BlobId, BlobStore, BlobStoreError, FundingAmount, StoreResult},
    db::{self, accounts::StoragePoolState},
    pearl_client::PearlConnection,
    sui_transaction,
};

/// Fixed SUI buffer attached to every synchronous 402 response, in MIST.
///
/// Roughly ~10 PTBs of headroom at typical reference gas prices. Sized
/// independently from `extension_cost::SUI_GAS_PER_EXTENSION_BUFFER_MIST`,
/// which is the async extension worker's per-cycle hint. This constant is
/// only used by the user-facing synchronous upload path.
pub const SUI_GAS_PER_OP_BUFFER_MIST: u64 = 10_000_000;

/// Check whether an error message indicates insufficient on-chain balance.
fn is_insufficient_balance(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("insufficientgas")
        || lower.contains("insufficient gas")
        || lower.contains("insufficientcoinbalance")
        || lower.contains("insufficient coin balance")
        || lower.contains("gasbalancetoolow")
        || lower.contains("gas balance too low")
        || lower.contains("unable to select gas")
        || lower.contains("not enough balance")
        || lower.contains("not enough coins")
        || lower.contains("cannot pay gas")
        || lower.contains("insufficient balance")
        || lower.contains("could not find") && lower.contains("coins")
}

/// Classify a `create_storage_pool` failure: balance shortfalls become 402
/// (with the carried `funding_required` estimate); everything else stays a
/// 502 [`BlobStoreError::PoolCreationFailed`].
fn classify_create_pool_error(
    raw: String,
    funding_estimate: Option<FundingAmount>,
) -> BlobStoreError {
    if is_insufficient_balance(&raw) {
        BlobStoreError::InsufficientBalance {
            message: raw,
            funding_required: funding_estimate,
        }
    } else {
        BlobStoreError::PoolCreationFailed(raw)
    }
}

/// Classify a generic "PTB build / submit" failure: balance shortfalls
/// become 402 (with the carried `funding_required` estimate); everything
/// else is bucketed as an upstream Sui/Walrus error → 502.
fn classify_upstream_error(raw: String, funding_estimate: Option<FundingAmount>) -> BlobStoreError {
    if is_insufficient_balance(&raw) {
        BlobStoreError::InsufficientBalance {
            message: raw,
            funding_required: funding_estimate,
        }
    } else {
        BlobStoreError::Upstream(raw)
    }
}

/// Best-effort estimate of the WAL+SUI needed to reserve `encoded_size`
/// bytes of `StoragePool` capacity for `epochs` epochs. Falls back to
/// `None` if the storage price lookup itself fails — the caller surfaces
/// `funding_required: None` in the response body and the client falls back
/// to `/account/wallet`.
async fn estimate_storage_funding(
    read_client: &SuiReadClient,
    encoded_size: u64,
    epochs: u32,
) -> Option<FundingAmount> {
    match read_client.storage_price_per_unit_size().await {
        Ok(price) => {
            let wal_frost =
                walrus_sui::utils::price_for_encoded_length(encoded_size, price, epochs);
            Some(FundingAmount {
                wal_frost,
                sui_mist: SUI_GAS_PER_OP_BUFFER_MIST,
            })
        }
        Err(e) => {
            tracing::warn!(error = %e, "storage_price_per_unit_size lookup failed; \
                returning funding_required: None");
            None
        }
    }
}

/// Best-effort estimate of the WAL+SUI needed to register `encoded_size`
/// bytes' worth of blob writes. See [`estimate_storage_funding`] for the
/// failure-mode behavior.
async fn estimate_write_funding(
    read_client: &SuiReadClient,
    encoded_size: u64,
) -> Option<FundingAmount> {
    match read_client.write_price_per_unit_size().await {
        Ok(price) => {
            let wal_frost = walrus_sui::utils::write_price_for_encoded_length(encoded_size, price);
            Some(FundingAmount {
                wal_frost,
                sui_mist: SUI_GAS_PER_OP_BUFFER_MIST,
            })
        }
        Err(e) => {
            tracing::warn!(error = %e, "write_price_per_unit_size lookup failed; \
                returning funding_required: None");
            None
        }
    }
}

/// Find the first `ObjectChange::Created` in a transaction response whose
/// Move struct `module::name` matches the predicate, returning its ObjectID.
fn extract_created_by_type<F>(
    resp: &sui_sdk::rpc_types::SuiTransactionBlockResponse,
    mut pred: F,
) -> Option<ObjectID>
where
    F: FnMut(&str, &str) -> bool,
{
    resp.object_changes.as_ref()?.iter().find_map(|c| {
        if let ObjectChange::Created {
            object_type,
            object_id,
            ..
        } = c
            && pred(object_type.module.as_str(), object_type.name.as_str())
        {
            Some(*object_id)
        } else {
            None
        }
    })
}

/// Find the first emitted event in `resp` whose Move struct `module::name`
/// matches the predicate and return the `objectId` / `object_id` field
/// from its `parsed_json` payload, parsed as an `ObjectID`.
fn extract_object_id_from_event<F>(
    resp: &sui_sdk::rpc_types::SuiTransactionBlockResponse,
    mut pred: F,
) -> Option<ObjectID>
where
    F: FnMut(&str, &str) -> bool,
{
    let events = resp.events.as_ref()?;
    for event in &events.data {
        if pred(event.type_.module.as_str(), event.type_.name.as_str()) {
            let obj = event.parsed_json.as_object()?;
            let raw = obj
                .get("object_id")
                .or_else(|| obj.get("objectId"))
                .and_then(|v| v.as_str())?;
            return raw.parse().ok();
        }
    }
    None
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Parse an Oyster [`BlobId`] string into a `walrus_core::BlobId`,
/// returning `BlobStoreError::InvalidBlobId` (which the error layer maps
/// to 400) when the string can't be decoded.
fn parse_walrus_blob_id(blob_id: &BlobId) -> Result<walrus_core::BlobId, BlobStoreError> {
    blob_id
        .as_str()
        .parse()
        .map_err(|e| BlobStoreError::InvalidBlobId(format!("{blob_id}: {e}")))
}

/// Bytes to request from `increase_storage_pool_capacity`, rounded up to the
/// Walrus accounting unit. Returns 0 when the encoded blob already fits.
///
/// Walrus bills storage in whole `BYTES_PER_UNIT_SIZE` units
/// (see `walrus_sui::utils::BYTES_PER_UNIT_SIZE`); rounding up costs the same
/// as rounding down and leaves usable headroom for subsequent small uploads.
fn grow_by_bytes(remaining: i64, encoded_size: u64) -> u64 {
    let needed: i64 = (encoded_size as i64) - remaining;
    if needed > 0 {
        storage_units_from_size(needed as u64) * BYTES_PER_UNIT_SIZE
    } else {
        0
    }
}

/// Blob store that writes directly to Walrus via on-chain Sui transactions.
pub struct DirectWalrusBlobStore {
    node_client: WalrusNodeClient<SuiReadClient>,
    read_client: Arc<SuiReadClient>,
    pearl: PearlConnection,
    rpc_url: String,
    db: db::DbPool,
    pool_initial_encoded_capacity_bytes: u64,
    pool_initial_epochs_ahead: u32,
}

impl DirectWalrusBlobStore {
    /// Create a new direct Walrus blob store connected to the given Sui RPC and Walrus cluster.
    pub async fn new(
        rpc_url: String,
        system_object: ObjectID,
        staking_object: ObjectID,
        pearl: PearlConnection,
        db: db::DbPool,
        pool_initial_encoded_capacity_bytes: u64,
        pool_initial_epochs_ahead: u32,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let read_client =
            sui_transaction::build_sui_read_client(&rpc_url, system_object, staking_object).await?;

        let contract_config =
            walrus_sui::client::contract_config::ContractConfig::new(system_object, staking_object);
        let client_config = ClientConfig::new_from_contract_config(contract_config);
        let node_client =
            WalrusNodeClient::new_read_client_with_refresher(client_config, (*read_client).clone())
                .await?;

        Ok(Self {
            node_client,
            read_client,
            pearl,
            rpc_url,
            db,
            pool_initial_encoded_capacity_bytes,
            pool_initial_epochs_ahead,
        })
    }

    async fn create_pool_for_account(
        &self,
        account_id: &AccountId,
        sender: SuiAddress,
        current_epoch: Epoch,
    ) -> Result<StoragePoolState, BlobStoreError> {
        // Pre-compute the per-account pool reservation cost so a balance
        // shortfall at any of the build/submit sites below carries a
        // `funding_required` block on the 402 response.
        let funding_estimate = estimate_storage_funding(
            &self.read_client,
            self.pool_initial_encoded_capacity_bytes,
            self.pool_initial_epochs_ahead,
        )
        .await;

        let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender);
        ptb.create_storage_pool(
            self.pool_initial_encoded_capacity_bytes,
            self.pool_initial_epochs_ahead,
        )
        .await
        .map_err(|e| {
            classify_create_pool_error(format!("create_storage_pool PTB: {e}"), funding_estimate)
        })?;
        let tx_data = ptb
            .build_transaction_data(None)
            .await
            .map_err(|e| classify_create_pool_error(format!("build tx: {e}"), funding_estimate))?;
        let resp =
            sui_transaction::sign_and_submit(&self.pearl, account_id, &self.rpc_url, tx_data)
                .await
                .map_err(|e| classify_create_pool_error(e.to_string(), funding_estimate))?;

        let pool_object_id = extract_created_by_type(&resp, |module, name| {
            module == "storage_pool" && name == "StoragePool"
        })
        .or_else(|| {
            // Fall back to the StoragePoolCreated event, whose `storage_pool_id`
            // field names the newly-created pool.
            let events = resp.events.as_ref()?;
            events.data.iter().find_map(|event| {
                if event.type_.module.as_str() == "events"
                    && event.type_.name.as_str() == "StoragePoolCreated"
                {
                    event
                        .parsed_json
                        .as_object()
                        .and_then(|o| o.get("storage_pool_id"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| {
            // Server-internal invariant violation: the on-chain call succeeded
            // but the response didn't carry the object we just created.
            BlobStoreError::Internal("no StoragePool object in create_storage_pool response".into())
        })?;

        let end_epoch = (current_epoch as i64) + (self.pool_initial_epochs_ahead as i64);
        let reserved = self.pool_initial_encoded_capacity_bytes as i64;

        let won = db::accounts::set_storage_pool(
            &self.db,
            account_id,
            &pool_object_id.to_string(),
            end_epoch,
            reserved,
            0,
        )
        .await?;
        if won {
            Ok(StoragePoolState {
                object_id: pool_object_id.to_string(),
                end_epoch,
                reserved_encoded_bytes: reserved,
                used_encoded_bytes: 0,
            })
        } else {
            tracing::warn!(
                account_id = %account_id,
                orphan_pool = %pool_object_id,
                "lost lazy-create race; orphaning just-created StoragePool and using the existing one",
            );
            db::accounts::get_storage_pool(&self.db, account_id)
                .await?
                .ok_or_else(|| {
                    BlobStoreError::Internal(
                        "race: set_storage_pool returned false but get_storage_pool returned None"
                            .into(),
                    )
                })
        }
    }

    async fn store_impl(
        &self,
        data: &[u8],
        account_id: &AccountId,
    ) -> Result<StoreResult, BlobStoreError> {
        // Pearl is a first-party internal service, so failures here are
        // genuinely server-internal rather than Sui/Walrus upstream errors.
        let sender_address = sui_transaction::resolve_sender_address(&self.pearl, account_id)
            .await
            .map_err(|e| BlobStoreError::Internal(format!("resolve sender address: {e}")))?;

        // 1. Encode the blob data. Encoding runs locally — failures here
        // are server-internal (RS2 encoder invariant), not upstream.
        let encoding_config = self.node_client.encoding_config();
        let encoding = encoding_config.get_for_type(walrus_core::EncodingType::RS2);
        let (sliver_pairs, metadata) = encoding
            .encode_with_metadata(data.to_vec())
            .map_err(|e| BlobStoreError::Internal(format!("encoding error: {e}")))?;

        let blob_obj_metadata = BlobObjectMetadata::try_from(&metadata)
            .map_err(|e| BlobStoreError::Internal(format!("blob metadata error: {e}")))?;
        let walrus_blob_id = *metadata.blob_id();
        let encoded_size = blob_obj_metadata.encoded_size;
        let unencoded_size = blob_obj_metadata.unencoded_size;
        let encoding_type = blob_obj_metadata.encoding_type;
        let walrus_blob_id_str = walrus_blob_id.to_string();

        // Account-level content dedup: if the same blob_id is already registered
        // for this account, reuse its PooledBlob object ID and skip all on-chain
        // work. The StoragePool rejects duplicate blob_ids anyway.
        if let Some(existing) = db::blobs::find_pooled_blob_object_id_for_account(
            &self.db,
            account_id,
            &walrus_blob_id_str,
        )
        .await?
        {
            // Record encoded_size on the dedup'd row so the final delete (when
            // the last reference to this blob_id is removed) can decrement
            // pool_used_encoded_bytes — Walrus is content-addressed, so the
            // encoded_size matches the prior row.
            return Ok(StoreResult {
                blob_id: BlobId(walrus_blob_id_str),
                pooled_blob_object_id: Some(existing),
                encoded_size: Some(encoded_size),
            });
        }

        // 2. Determine the StoragePool for this account (lazy-create on first write).
        let current_epoch = self
            .read_client
            .current_epoch()
            .await
            .map_err(|e| BlobStoreError::Upstream(format!("current_epoch: {e}")))?;
        let pool_state = match db::accounts::get_storage_pool(&self.db, account_id).await? {
            Some(state) => state,
            None => {
                self.create_pool_for_account(account_id, sender_address, current_epoch)
                    .await?
            }
        };

        let pool_object_id: ObjectID = pool_state.object_id.parse().map_err(|e| {
            // Stored pool ObjectID isn't parsable — DB invariant violation.
            BlobStoreError::Internal(format!("invalid pool ObjectID: {e}"))
        })?;

        // 3. Register PTB: optional capacity bump + register_pooled_blobs.
        let remaining = pool_state.reserved_encoded_bytes - pool_state.used_encoded_bytes;
        let grow_by: u64 = grow_by_bytes(remaining, encoded_size);
        let remaining_epochs = (pool_state.end_epoch - current_epoch as i64).max(1) as u32;

        // Best-effort funding estimates for the two distinct cost components
        // we're about to submit: capacity growth (storage price × bytes ×
        // remaining epochs) and the per-blob write fee.
        let grow_funding = if grow_by > 0 {
            estimate_storage_funding(&self.read_client, grow_by, remaining_epochs).await
        } else {
            None
        };
        let write_funding = estimate_write_funding(&self.read_client, encoded_size).await;

        let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender_address);
        if grow_by > 0 {
            ptb.increase_storage_pool_capacity(pool_object_id, grow_by, remaining_epochs)
                .await
                .map_err(|e| {
                    classify_upstream_error(
                        format!("increase_storage_pool_capacity error: {e}"),
                        grow_funding,
                    )
                })?;
        }
        ptb.register_pooled_blobs(
            pool_object_id,
            vec![blob_obj_metadata],
            BlobPersistence::Deletable,
        )
        .await
        .map_err(|e| {
            classify_upstream_error(format!("register_pooled_blobs error: {e}"), write_funding)
        })?;
        let tx_data = ptb.build_transaction_data(None).await.map_err(|e| {
            classify_upstream_error(format!("build_transaction_data error: {e}"), write_funding)
        })?;

        let register_resp =
            sui_transaction::sign_and_submit(&self.pearl, account_id, &self.rpc_url, tx_data)
                .await
                .map_err(|e| {
                    classify_upstream_error(format!("register tx error: {e}"), write_funding)
                })?;

        tracing::info!("register tx digest: {:?}", register_resp.digest);

        let pooled_blob_object_id = extract_object_id_from_event(&register_resp, |module, name| {
            module == "events" && name == "PooledBlobRegistered"
        })
        .ok_or_else(|| {
            BlobStoreError::Internal(
                "no PooledBlobRegistered event in register_pooled_blobs response".into(),
            )
        })?;

        db::accounts::update_pool_after_register(
            &self.db,
            account_id,
            grow_by as i64,
            encoded_size as i64,
        )
        .await?;

        // 4. Upload slivers to storage nodes and collect certificate.
        let persistence_type = BlobPersistenceType::Deletable {
            object_id: pooled_blob_object_id.into(),
        };
        let certificate = self
            .node_client
            .send_blob_data_and_get_certificate(
                &metadata,
                Arc::new(sliver_pairs),
                &persistence_type,
                None,
                TailHandling::Blocking,
                None,
                None,
                None,
                None,
            )
            .await
            .map_err(|e| BlobStoreError::Upstream(format!("sliver upload error: {e}")))?;

        // 5. Certify the PooledBlob on-chain.
        let pooled_blob = PooledBlob {
            id: pooled_blob_object_id,
            registered_epoch: current_epoch,
            blob_id: walrus_blob_id,
            unencoded_size,
            encoding_type,
            certified_epoch: None,
            storage_pool_id: pool_object_id,
            deletable: true,
        };

        let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender_address);
        ptb.certify_pooled_blobs(pool_object_id, &[(&pooled_blob, certificate)])
            .await
            .map_err(|e| BlobStoreError::Upstream(format!("certify_pooled_blobs error: {e}")))?;
        let tx_data = ptb
            .build_transaction_data(None)
            .await
            .map_err(|e| BlobStoreError::Upstream(format!("build_transaction_data error: {e}")))?;

        sui_transaction::sign_and_submit(&self.pearl, account_id, &self.rpc_url, tx_data)
            .await
            .map_err(|e| {
                // Certify doesn't take new funds (it consumes the registered
                // blob slot), so we don't attach a funding estimate here.
                classify_upstream_error(format!("certify tx error: {e}"), None)
            })?;

        Ok(StoreResult {
            blob_id: BlobId(walrus_blob_id_str),
            pooled_blob_object_id: Some(pooled_blob_object_id.to_string()),
            encoded_size: Some(encoded_size),
        })
    }
}

impl BlobStore for DirectWalrusBlobStore {
    fn store(
        &self,
        data: &[u8],
        account_id: &AccountId,
    ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>> {
        let data = data.to_vec();
        let account_id = *account_id;
        Box::pin(async move { self.store_impl(&data, &account_id).await })
    }

    fn read(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<Vec<u8>, BlobStoreError>> {
        let blob_id = blob_id.clone();
        Box::pin(async move {
            let walrus_blob_id = parse_walrus_blob_id(&blob_id)?;

            match self
                .node_client
                .read_blob_retry_committees::<walrus_core::encoding::Primary>(
                    &walrus_blob_id,
                    walrus_core::encoding::ConsistencyCheckType::Default,
                )
                .await
            {
                Ok(bytes) => Ok(bytes),
                Err(e) => match e.kind() {
                    ClientErrorKind::BlobIdDoesNotExist => {
                        Err(BlobStoreError::NotFound(blob_id.to_string()))
                    }
                    _ => Err(BlobStoreError::Upstream(format!("walrus read error: {e}"))),
                },
            }
        })
    }

    fn delete(
        &self,
        blob_id: &BlobId,
        pool_id: Option<&str>,
        encoded_size: u64,
        account_id: &AccountId,
    ) -> BoxFuture<'_, Result<(), BlobStoreError>> {
        let blob_id = blob_id.clone();
        let pool_id = pool_id.map(String::from);
        let account_id = *account_id;
        Box::pin(async move {
            let Some(pool_id_str) = pool_id else {
                tracing::warn!(
                    account_id = %account_id,
                    blob_id = %blob_id,
                    "delete called with no pool_id; skipping on-chain delete",
                );
                return Ok(());
            };
            let pool_object_id: ObjectID = pool_id_str
                .parse()
                .map_err(|e| BlobStoreError::Internal(format!("invalid pool_id: {e}")))?;
            let walrus_blob_id: walrus_core::BlobId = blob_id
                .as_str()
                .parse()
                .map_err(|e| BlobStoreError::Internal(format!("invalid walrus blob_id: {e}")))?;

            let sender_address = sui_transaction::resolve_sender_address(&self.pearl, &account_id)
                .await
                .map_err(|e| BlobStoreError::Internal(format!("resolve sender address: {e}")))?;

            let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender_address);
            ptb.delete_pooled_blob(pool_object_id, walrus_blob_id)
                .await
                .map_err(|e| BlobStoreError::Upstream(format!("delete_pooled_blob PTB: {e}")))?;
            let tx_data = ptb
                .build_transaction_data(None)
                .await
                .map_err(|e| BlobStoreError::Upstream(format!("build_transaction_data: {e}")))?;
            sui_transaction::sign_and_submit(&self.pearl, &account_id, &self.rpc_url, tx_data)
                .await
                .map_err(|e| {
                    // The delete path only needs SUI gas; the FE can already
                    // infer the SUI buffer from `/account/wallet`, so we
                    // intentionally don't attach a WAL funding estimate here.
                    classify_upstream_error(format!("delete tx: {e}"), None)
                })?;

            if encoded_size > 0 {
                db::accounts::update_pool_after_delete(&self.db, &account_id, encoded_size as i64)
                    .await?;
            }
            Ok(())
        })
    }

    fn exists(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<bool, BlobStoreError>> {
        let blob_id = blob_id.clone();
        Box::pin(async move {
            let walrus_blob_id = parse_walrus_blob_id(&blob_id)?;

            match self
                .node_client
                .get_blob_status_with_retries(&walrus_blob_id, self.read_client.as_ref())
                .await
            {
                // `initial_certified_epoch().is_some()` mirrors the aggregator's
                // HEAD semantics: 200 only when the blob is retrievable —
                // certified (Permanent or Deletable) and not Nonexistent/Invalid.
                Ok(status) => Ok(status.initial_certified_epoch().is_some()),
                Err(e) if matches!(e.kind(), ClientErrorKind::BlobIdDoesNotExist) => Ok(false),
                Err(e) => Err(BlobStoreError::Upstream(format!(
                    "walrus blob status error: {e}"
                ))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = BYTES_PER_UNIT_SIZE;

    #[test]
    fn grow_by_bytes_already_fits_returns_zero() {
        assert_eq!(grow_by_bytes(1_000, 500), 0);
        assert_eq!(grow_by_bytes(500, 500), 0); // exact fit
        assert_eq!(grow_by_bytes(MIB as i64, MIB - 1), 0);
    }

    #[test]
    fn grow_by_bytes_rounds_up_to_unit() {
        // 500 KiB remaining, 600 KiB encoded → needs 100 KiB, rounds to 1 MiB.
        assert_eq!(grow_by_bytes(500 * 1024, 600 * 1024), MIB);
        // 0 remaining, exactly 1 MiB encoded → 1 MiB.
        assert_eq!(grow_by_bytes(0, MIB), MIB);
        // 0 remaining, 1 MiB + 1 byte → 2 MiB.
        assert_eq!(grow_by_bytes(0, MIB + 1), 2 * MIB);
        // Negative `remaining` (shouldn't happen in practice, but be defensive).
        assert_eq!(grow_by_bytes(-10, 5), MIB);
    }

    #[test]
    fn parse_walrus_blob_id_rejects_malformed_with_invalid_blob_id() {
        // The route handler hands the raw path param straight to the store,
        // so a malformed id must surface as `InvalidBlobId` (→ HTTP 400) and
        // never as `Http` (→ HTTP 500). See `read_blob_by_blob_id` and the
        // `blob_store_invalid_blob_id_maps_to_400` integration test.
        let err = parse_walrus_blob_id(&BlobId("FAKE_BLOB_ID_DOES_NOT_EXIST".into()))
            .expect_err("malformed blob_id should fail to parse");
        assert!(
            matches!(err, BlobStoreError::InvalidBlobId(_)),
            "expected InvalidBlobId, got {err:?}",
        );
    }

    #[test]
    fn parse_walrus_blob_id_accepts_well_formed_id() {
        use base64::Engine as _;
        // 32-byte URL-safe-base64 (no padding) — well-formed by construction.
        let s = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 32]);
        parse_walrus_blob_id(&BlobId(s)).expect("well-formed id should parse");
    }

    /// Reproduces the staging incident: `create_storage_pool` PTB-build
    /// failed with "could not find WAL coins with sufficient balance" but
    /// was bucketed as `PoolCreationFailed` → 502. After Phase A it must
    /// classify as `InsufficientBalance` → 402.
    #[test]
    fn classify_create_pool_error_recognizes_walrus_balance_message() {
        let err = classify_create_pool_error(
            "create_storage_pool PTB: could not find WAL coins with sufficient balance".into(),
            None,
        );
        assert!(
            matches!(err, BlobStoreError::InsufficientBalance { .. }),
            "expected InsufficientBalance, got {err:?}",
        );
    }

    #[test]
    fn classify_create_pool_error_passes_through_funding_estimate() {
        let funding = FundingAmount {
            wal_frost: 12_345,
            sui_mist: SUI_GAS_PER_OP_BUFFER_MIST,
        };
        let err =
            classify_create_pool_error("...could not find WAL coins...".into(), Some(funding));
        match err {
            BlobStoreError::InsufficientBalance {
                funding_required: Some(amount),
                ..
            } => {
                assert_eq!(amount, funding);
            }
            other => panic!("expected InsufficientBalance with funding, got {other:?}"),
        }
    }

    #[test]
    fn classify_create_pool_error_other_failures_stay_pool_creation_failed() {
        let err = classify_create_pool_error("internal walrus contract assertion".into(), None);
        assert!(
            matches!(err, BlobStoreError::PoolCreationFailed(_)),
            "expected PoolCreationFailed, got {err:?}",
        );
    }

    #[test]
    fn classify_upstream_error_recognizes_balance_shortfall() {
        let err = classify_upstream_error(
            "register tx error: InsufficientCoinBalance".into(),
            Some(FundingAmount {
                wal_frost: 1,
                sui_mist: 2,
            }),
        );
        assert!(
            matches!(err, BlobStoreError::InsufficientBalance { .. }),
            "expected InsufficientBalance, got {err:?}",
        );
    }

    #[test]
    fn classify_upstream_error_buckets_other_failures_as_upstream() {
        let err = classify_upstream_error("walrus storage node 500".into(), None);
        assert!(
            matches!(err, BlobStoreError::Upstream(_)),
            "expected Upstream, got {err:?}",
        );
    }
}
