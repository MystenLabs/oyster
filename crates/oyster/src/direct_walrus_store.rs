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

use sui_rpc::proto::sui::rpc::v2;
use sui_types::base_types::{ObjectID, SuiAddress};
use walrus_core::{Epoch, encoding::EncodingFactory as _, messages::BlobPersistenceType};
use walrus_sdk::{
    config::ClientConfig, error::ClientErrorKind, node_client::WalrusNodeClient,
    uploader::TailHandling,
};
use walrus_sui::{
    client::{
        BlobObjectMetadata, BlobPersistence, ReadClient, SuiReadClient,
        transaction_builder::WalrusPtbBuilder,
    },
    types::PooledBlob,
    utils::{BYTES_PER_UNIT_SIZE, storage_units_from_size},
};

use crate::{
    AccountId, FundingAmount,
    blob_store::{BlobId, BlobStore, BlobStoreError, StoreResult},
    db::{self, accounts::StoragePoolState},
    pearl_client::PearlConnection,
    storage_cap::{self, CapViolation},
    sui_object_reader,
    sui_transaction::{self, SignedTxOutcome},
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

/// Format a [`CapViolation`] as a [`BlobStoreError::CapExceeded`] with a
/// human-readable `message` describing the failing arm of the cap
/// inequality. The numeric fields surface in the structured JSON body.
fn cap_violation_to_error(violation: CapViolation) -> BlobStoreError {
    let CapViolation {
        max_unencoded_bytes,
        used_encoded_bytes,
        new_unencoded_bytes,
    } = violation;
    // i64 round-trip: per-account caps fit by construction (Postgres
    // BIGINT NOT NULL, route layer rejects ≤ 0), and saturating clamps
    // pathological inputs.
    let max_i64 = i64::try_from(max_unencoded_bytes).unwrap_or(i64::MAX);
    let message = format!(
        "account cap is {max_unencoded_bytes} unencoded bytes; \
         on-chain encoded usage {used_encoded_bytes} plus this upload \
         ({new_unencoded_bytes} unencoded bytes) would exceed it"
    );
    BlobStoreError::CapExceeded {
        message,
        max_unencoded_bytes: max_i64,
        used_encoded_bytes,
        new_unencoded_bytes,
    }
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
            operation: "unknown",
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
            operation: "unknown",
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

/// Find the first newly-created object in a gRPC transaction outcome
/// whose Move struct `module::name` matches the predicate, returning
/// its ObjectID.
fn extract_created_by_type<F>(outcome: &SignedTxOutcome, mut pred: F) -> Option<ObjectID>
where
    F: FnMut(&str, &str) -> bool,
{
    outcome.changed_objects.iter().find_map(|c| {
        if c.id_operation() != v2::changed_object::IdOperation::Created {
            return None;
        }
        let object_type = c.object_type.as_deref()?;
        let (module, name) = parse_object_type(object_type)?;
        if !pred(module, name) {
            return None;
        }
        c.object_id.as_deref()?.parse().ok()
    })
}

/// Find the first emitted event in `outcome` whose Move struct
/// `module::name` matches the predicate and return the `object_id` /
/// `objectId` field from its JSON payload, parsed as an `ObjectID`.
fn extract_object_id_from_event<F>(outcome: &SignedTxOutcome, mut pred: F) -> Option<ObjectID>
where
    F: FnMut(&str, &str) -> bool,
{
    for event in &outcome.events {
        let event_type = match event.event_type.as_deref() {
            Some(s) => s,
            None => continue,
        };
        let (module, name) = match parse_object_type(event_type) {
            Some(parts) => parts,
            None => continue,
        };
        if !pred(module, name) {
            continue;
        }
        if let Some(id) = pluck_object_id_from_event_json(event.json.as_deref()) {
            return Some(id);
        }
    }
    None
}

/// Parse `"0x…::module::Name"` into `("module", "Name")`. Returns
/// `None` on malformed input or fewer than three `::`-separated parts.
fn parse_object_type(s: &str) -> Option<(&str, &str)> {
    let mut parts = s.rsplitn(3, "::");
    let name = parts.next()?;
    let module = parts.next()?;
    parts.next()?;
    if name.is_empty() || module.is_empty() {
        return None;
    }
    Some((module, name))
}

/// Pluck the `object_id` (or `objectId`) string field from an event's
/// JSON payload (a `prost_types::Value`) and parse it as an `ObjectID`.
fn pluck_object_id_from_event_json(value: Option<&prost_types::Value>) -> Option<ObjectID> {
    let kind = value?.kind.as_ref()?;
    let prost_types::value::Kind::StructValue(s) = kind else {
        return None;
    };
    let raw = s
        .fields
        .get("object_id")
        .or_else(|| s.fields.get("objectId"))?
        .kind
        .as_ref()?;
    let prost_types::value::Kind::StringValue(s) = raw else {
        return None;
    };
    s.parse().ok()
}

/// Pluck a string field from an event's JSON payload by key, parsed as
/// an `ObjectID`.
fn pluck_string_from_event_json(value: Option<&prost_types::Value>, key: &str) -> Option<ObjectID> {
    let kind = value?.kind.as_ref()?;
    let prost_types::value::Kind::StructValue(s) = kind else {
        return None;
    };
    let raw = s.fields.get(key)?.kind.as_ref()?;
    let prost_types::value::Kind::StringValue(s) = raw else {
        return None;
    };
    s.parse().ok()
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

    /// Fetch the account's stored master-seed version. Every Pearl call
    /// in a flow must use it so the account signs with its own wallet key
    /// across seed rotations.
    async fn account_key_version(&self, account_id: &AccountId) -> Result<u32, BlobStoreError> {
        db::accounts::get_key_version(&self.db, account_id)
            .await?
            .ok_or_else(|| {
                BlobStoreError::Internal(format!("account {account_id} not found in database"))
            })
    }

    async fn create_pool_for_account(
        &self,
        account_id: &AccountId,
        key_version: u32,
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
        let resp = sui_transaction::sign_and_submit(
            &self.pearl,
            account_id,
            key_version,
            &self.rpc_url,
            tx_data,
        )
        .await
        .map_err(|e| classify_create_pool_error(e.to_string(), funding_estimate))?;

        let pool_object_id = extract_created_by_type(&resp, |module, name| {
            module == "storage_pool" && name == "StoragePool"
        })
        .or_else(|| {
            // Fall back to the StoragePoolCreated event, whose `storage_pool_id`
            // field names the newly-created pool.
            resp.events.iter().find_map(|event| {
                let event_type = event.event_type.as_deref()?;
                let (module, name) = parse_object_type(event_type)?;
                if module == "events" && name == "StoragePoolCreated" {
                    pluck_string_from_event_json(event.json.as_deref(), "storage_pool_id")
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
        data: Vec<u8>,
        account_id: &AccountId,
    ) -> Result<StoreResult, BlobStoreError> {
        // Pearl is a first-party internal service, so failures here are
        // genuinely server-internal rather than Sui/Walrus upstream errors.
        let key_version = self.account_key_version(account_id).await?;
        let sender_address =
            sui_transaction::resolve_sender_address(&self.pearl, account_id, key_version)
                .await
                .map_err(|e| BlobStoreError::Internal(format!("resolve sender address: {e}")))?;

        // 1. Encode the blob data. Encoding runs locally — failures here
        // are server-internal (RS2 encoder invariant), not upstream.
        let encoding_config = self.node_client.encoding_config();
        let n_shards = encoding_config.n_shards();
        let max_unencoded_for_network = walrus_core::encoding::max_blob_size_for_n_shards(
            n_shards,
            walrus_core::EncodingType::RS2,
        );
        let unencoded_len = data.len() as u64;
        // Reject over-ceiling payloads *before* encoding: the encoder
        // would materialize the ~4.5x sliver expansion (or fail after
        // significant allocation) for a blob we already know is too big.
        if unencoded_len > max_unencoded_for_network {
            return Err(BlobStoreError::PayloadTooLarge {
                unencoded_size: unencoded_len,
                n_shards: n_shards.get(),
                max_unencoded_for_network,
            });
        }
        let encoding = encoding_config.get_for_type(walrus_core::EncodingType::RS2);
        let (sliver_pairs, metadata) =
            encoding
                .encode_with_metadata(data)
                .map_err(|_e| BlobStoreError::PayloadTooLarge {
                    unencoded_size: unencoded_len,
                    n_shards: n_shards.get(),
                    max_unencoded_for_network,
                })?;

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

        // 2. Pre-pool storage-cap short-circuit: if this single blob's
        // unencoded size already exceeds the account's cap, reject 400
        // *before* lazy-creating a `StoragePool` on-chain. The full cap
        // check (which needs the pool's on-chain `used_encoded_bytes`)
        // runs after the pool is resolved.
        let (max_unencoded_bytes, avg_blob_size) =
            db::accounts::get_cap_and_avg_blob_size(&self.db, account_id)
                .await?
                .ok_or_else(|| {
                    BlobStoreError::Internal(format!("account {account_id} not found in DB"))
                })?;
        if i128::from(unencoded_size) > i128::from(max_unencoded_bytes) {
            return Err(cap_violation_to_error(CapViolation {
                max_unencoded_bytes: max_unencoded_bytes.max(0) as u64,
                used_encoded_bytes: 0,
                new_unencoded_bytes: unencoded_size,
            }));
        }

        // 3. Determine the StoragePool for this account (lazy-create on first write).
        let current_epoch = self
            .read_client
            .current_epoch()
            .await
            .map_err(|e| BlobStoreError::Upstream(format!("current_epoch: {e}")))?;
        let pool_state = match db::accounts::get_storage_pool(&self.db, account_id).await? {
            Some(state) => state,
            None => {
                self.create_pool_for_account(account_id, key_version, sender_address, current_epoch)
                    .await?
            }
        };

        let pool_object_id: ObjectID = pool_state.object_id.parse().map_err(|e| {
            // Stored pool ObjectID isn't parsable — DB invariant violation.
            BlobStoreError::Internal(format!("invalid pool ObjectID: {e}"))
        })?;

        // 4. Storage-cap check: one RPC read of the on-chain pool's
        // `used_encoded_bytes`, then evaluate the cap inequality. The
        // cap is stated in *unencoded* bytes but on-chain usage is
        // tracked in *encoded* bytes, so we compare against the
        // forward-encoded threshold `f(max_unencoded − new_unencoded)`.
        // See `storage_cap::enforce_storage_cap` for the derivation.
        let on_chain_pool =
            sui_object_reader::read_storage_pool_state(&self.rpc_url, pool_object_id)
                .await
                .map_err(|e| BlobStoreError::Upstream(format!("read_storage_pool_state: {e}")))?;
        let cap_u64 = u64::try_from(max_unencoded_bytes).map_err(|_| {
            // Migration 018 backfills with `5_000_000_000` and the
            // route layer rejects non-positive overrides, so a
            // negative value here is a DB-invariant violation.
            BlobStoreError::Internal(format!(
                "negative max_unencoded_bytes {max_unencoded_bytes} for account {account_id}"
            ))
        })?;
        // `avg_blob_size` (unencoded bytes) inflates the encoded
        // admission ceiling so the cap is a *lower* bound for blobs of
        // this size. `0` (the sentinel, and the value for accounts
        // created before migration 020) disables inflation. A negative
        // value would be a DB-invariant violation (the route layer
        // rejects negatives); clamp to the sentinel defensively.
        let avg_blob_size_u64 = u64::try_from(avg_blob_size).unwrap_or(0);
        if let Err(violation) = storage_cap::enforce_storage_cap(
            cap_u64,
            on_chain_pool.used_encoded_bytes,
            unencoded_size,
            avg_blob_size_u64,
            n_shards,
        ) {
            return Err(cap_violation_to_error(violation));
        }

        // 5. Register PTB: optional capacity bump + register_pooled_blobs.
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

        // Cross-replica races on the per-account `StoragePool` can leave
        // the DB's `pool_*` counters ahead of the on-chain truth, so two
        // replicas each compute `grow_by = 0` and the second register PTB
        // aborts inside `storage_pool::add_blob` (`EInsufficientCapacity`,
        // code 6). On that specific abort, refresh the on-chain inner
        // state, reconcile the DB to match, recompute `grow_by`, and
        // resubmit a fresh register PTB once. The retry result is
        // `(SignedTxOutcome, applied_grow_by_bytes)`; we use
        // `applied_grow_by` in the eventual `update_pool_after_register`
        // delta below so the counters land at the correct post-state
        // whether or not we retried.
        let (register_resp, applied_grow_by) = match sui_transaction::sign_and_submit(
            &self.pearl,
            account_id,
            key_version,
            &self.rpc_url,
            tx_data,
        )
        .await
        {
            Ok(resp) => (resp, grow_by),
            Err(sui_transaction::SignAndSubmitError::ExecutionFailure(f))
                if f.is_move_abort("storage_pool", "add_blob", 6) =>
            {
                tracing::warn!(
                    account_id = %account_id,
                    pool_object_id = %pool_object_id,
                    abort = %f,
                    db_reserved = pool_state.reserved_encoded_bytes,
                    db_used = pool_state.used_encoded_bytes,
                    "register PTB aborted with EInsufficientCapacity; \
                     refreshing pool state from chain and retrying once",
                );
                let on_chain =
                    sui_object_reader::read_storage_pool_state(&self.rpc_url, pool_object_id)
                        .await
                        .map_err(|e| {
                            BlobStoreError::Upstream(format!(
                                "on-chain pool refresh after EInsufficientCapacity failed: {e}"
                            ))
                        })?;
                db::accounts::reconcile_pool_after_drift(
                    &self.db,
                    account_id,
                    on_chain.reserved_encoded_bytes as i64,
                    on_chain.used_encoded_bytes as i64,
                )
                .await?;

                // Recompute grow_by from the on-chain truth.
                let on_chain_remaining =
                    (on_chain.reserved_encoded_bytes as i64) - (on_chain.used_encoded_bytes as i64);
                let retry_grow_by = grow_by_bytes(on_chain_remaining, encoded_size);

                // The abort means a concurrent upload consumed the
                // capacity this request's cap check was quoted
                // against, so that verdict is stale. Re-run the cap
                // gate against the refreshed usage before buying
                // more capacity — but only when the retry would buy
                // (`retry_grow_by > 0`): reserved pool capacity is
                // already paid for, and consuming it is allowed even
                // past the cap. Without this re-check the retry
                // would grow the pool straight through the cap in
                // exactly the raced case it exists to handle.
                if retry_grow_by > 0
                    && let Err(violation) = storage_cap::enforce_storage_cap(
                        cap_u64,
                        on_chain.used_encoded_bytes,
                        unencoded_size,
                        avg_blob_size_u64,
                        n_shards,
                    )
                {
                    return Err(cap_violation_to_error(violation));
                }

                let retry_grow_funding = if retry_grow_by > 0 {
                    estimate_storage_funding(&self.read_client, retry_grow_by, remaining_epochs)
                        .await
                } else {
                    None
                };

                // Rebuild the PTB. The original `blob_obj_metadata`
                // was moved into the first builder, so reconstruct
                // it from `metadata` (same source as the first
                // attempt, just one extra cheap clone).
                let blob_obj_metadata_retry =
                    BlobObjectMetadata::try_from(&metadata).map_err(|e| {
                        BlobStoreError::Internal(format!("retry blob metadata error: {e}"))
                    })?;
                let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender_address);
                if retry_grow_by > 0 {
                    ptb.increase_storage_pool_capacity(
                        pool_object_id,
                        retry_grow_by,
                        remaining_epochs,
                    )
                    .await
                    .map_err(|e| {
                        classify_upstream_error(
                            format!("retry increase_storage_pool_capacity: {e}"),
                            retry_grow_funding,
                        )
                    })?;
                }
                ptb.register_pooled_blobs(
                    pool_object_id,
                    vec![blob_obj_metadata_retry],
                    BlobPersistence::Deletable,
                )
                .await
                .map_err(|e| {
                    classify_upstream_error(
                        format!("retry register_pooled_blobs: {e}"),
                        write_funding,
                    )
                })?;
                let tx_data = ptb.build_transaction_data(None).await.map_err(|e| {
                    classify_upstream_error(
                        format!("retry build_transaction_data: {e}"),
                        write_funding,
                    )
                })?;
                let resp = sui_transaction::sign_and_submit(
                    &self.pearl,
                    account_id,
                    key_version,
                    &self.rpc_url,
                    tx_data,
                )
                .await
                .map_err(|e| {
                    classify_upstream_error(format!("retry register tx: {e}"), write_funding)
                })?;
                (resp, retry_grow_by)
            }
            Err(sui_transaction::SignAndSubmitError::ExecutionFailure(f))
                if f.is_move_abort("dynamic_field", "add", 0) =>
            {
                // `EFieldAlreadyExists` from `storage_pool::add_blob` —
                // a `PooledBlob` for `walrus_blob_id` is already
                // present in this pool's `blobs: ObjectTable<u256,
                // PooledBlob>`. Self-heal: look it up on-chain and
                // return Ok with the existing object ID. This
                // covers TOCTOU dedup races (two concurrent uploads
                // of the same content for one account both miss
                // the DB dedup) and orphans left behind when a
                // prior delete tx failed but its DB row was
                // dropped anyway.
                tracing::warn!(
                    account_id = %account_id,
                    pool_object_id = %pool_object_id,
                    walrus_blob_id = %walrus_blob_id_str,
                    abort = %f,
                    "register PTB aborted with EFieldAlreadyExists; \
                     self-healing via on-chain blob lookup",
                );
                let recovered = sui_object_reader::lookup_pooled_blob_object_id(
                    &self.rpc_url,
                    pool_object_id,
                    &walrus_blob_id,
                )
                .await
                .map_err(|e| {
                    BlobStoreError::Upstream(format!(
                        "on-chain blob lookup after EFieldAlreadyExists failed: {e}"
                    ))
                })?
                .ok_or_else(|| {
                    BlobStoreError::Internal(format!(
                        "dynamic_field::add abort but blob {walrus_blob_id_str} not \
                             found in pool {pool_object_id}"
                    ))
                })?;

                // Label the cause from what the DB knows. If a row
                // for this (account, blob_id) carrying a non-NULL
                // `pooled_blob_object_id` exists, the DB dedup
                // simply lost a TOCTOU race. Otherwise the on-chain
                // `PooledBlob` is an orphan from a prior
                // delete-tx-failed → DB-delete-only path.
                let cause = if db::blobs::find_pooled_blob_object_id_for_account(
                    &self.db,
                    account_id,
                    &walrus_blob_id_str,
                )
                .await?
                .is_some()
                {
                    "db_miss"
                } else {
                    "orphan_recovered"
                };
                metrics::counter!(
                    crate::metrics::REGISTER_DEDUP_SELF_HEAL_TOTAL,
                    "cause" => cause,
                )
                .increment(1);

                // Skip `update_pool_after_register` and the certify
                // step: the on-chain `used_encoded_bytes` already
                // accounts for this PooledBlob, and re-certifying
                // an already-registered blob is unnecessary (the
                // existing DB-dedup short-circuit above also skips
                // certify).
                return Ok(StoreResult {
                    blob_id: BlobId(walrus_blob_id_str),
                    pooled_blob_object_id: Some(recovered.to_string()),
                    encoded_size: Some(encoded_size),
                });
            }
            Err(e) => {
                return Err(classify_upstream_error(
                    format!("register tx error: {e}"),
                    write_funding,
                ));
            }
        };

        tracing::info!(tx_digest = %register_resp.digest, "register tx submitted");

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
            applied_grow_by as i64,
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

        sui_transaction::sign_and_submit(
            &self.pearl,
            account_id,
            key_version,
            &self.rpc_url,
            tx_data,
        )
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
        data: Vec<u8>,
        account_id: &AccountId,
    ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>> {
        let account_id = *account_id;
        Box::pin(async move { self.store_impl(data, &account_id).await })
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

            let key_version = self.account_key_version(&account_id).await?;
            let sender_address =
                sui_transaction::resolve_sender_address(&self.pearl, &account_id, key_version)
                    .await
                    .map_err(|e| {
                        BlobStoreError::Internal(format!("resolve sender address: {e}"))
                    })?;

            let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender_address);
            ptb.delete_pooled_blob(pool_object_id, walrus_blob_id)
                .await
                .map_err(|e| BlobStoreError::Upstream(format!("delete_pooled_blob PTB: {e}")))?;
            let tx_data = ptb
                .build_transaction_data(None)
                .await
                .map_err(|e| BlobStoreError::Upstream(format!("build_transaction_data: {e}")))?;
            sui_transaction::sign_and_submit(
                &self.pearl,
                &account_id,
                key_version,
                &self.rpc_url,
                tx_data,
            )
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
    fn parse_object_type_round_trip() {
        assert_eq!(
            parse_object_type("0x123::storage_pool::StoragePool"),
            Some(("storage_pool", "StoragePool"))
        );
        // Real-world type with a generic argument keeps the inner `::`
        // split correctly via `rsplitn(3, ...)`.
        assert_eq!(
            parse_object_type("0xabc::events::PooledBlobRegistered"),
            Some(("events", "PooledBlobRegistered"))
        );
        // No leading package address.
        assert_eq!(parse_object_type("storage_pool::StoragePool"), None);
        // Too few components.
        assert_eq!(parse_object_type("0x123::foo"), None);
        assert_eq!(parse_object_type("nope"), None);
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
