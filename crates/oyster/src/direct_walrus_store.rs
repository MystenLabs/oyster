use std::{future::Future, pin::Pin, sync::Arc};

use sui_sdk::rpc_types::ObjectChange;
use sui_types::base_types::{ObjectID, SuiAddress};
use walrus_core::{Epoch, encoding::EncodingFactory as _, messages::BlobPersistenceType};
use walrus_sdk::{config::ClientConfig, node_client::WalrusNodeClient, uploader::TailHandling};
use walrus_sui::{
    client::{
        BlobObjectMetadata,
        BlobPersistence,
        ReadClient,
        SuiReadClient,
        transaction_builder::WalrusPtbBuilder,
    },
    types::PooledBlob,
};

use crate::{
    AccountId,
    blob_store::{BlobId, BlobStore, BlobStoreError, StoreResult},
    db::{self, accounts::StoragePoolState},
    pearl_client::PearlConnection,
    sui_transaction,
};

/// Map a `reqwest::Error` to a `BlobStoreError`. Connect/timeout errors
/// (no HTTP status was ever returned) become `Unreachable`, which the error
/// layer surfaces as HTTP 502. Everything else becomes a generic `Http`.
fn map_reqwest_err(e: reqwest::Error) -> BlobStoreError {
    if e.is_connect() || e.is_timeout() {
        BlobStoreError::Unreachable(e.to_string())
    } else {
        BlobStoreError::Http(e.to_string())
    }
}

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

/// Blob store that writes directly to Walrus via on-chain Sui transactions.
pub struct DirectWalrusBlobStore {
    node_client: WalrusNodeClient<SuiReadClient>,
    read_client: Arc<SuiReadClient>,
    pearl: PearlConnection,
    rpc_url: String,
    aggregator_url: String,
    http_client: reqwest::Client,
    db: db::DbPool,
    pool_initial_encoded_capacity_bytes: u64,
    pool_initial_epochs_ahead: u32,
}

impl DirectWalrusBlobStore {
    /// Create a new direct Walrus blob store connected to the given Sui RPC and Walrus cluster.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        rpc_url: String,
        aggregator_url: String,
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
            aggregator_url,
            http_client: reqwest::Client::new(),
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
        let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender);
        ptb.create_storage_pool(
            self.pool_initial_encoded_capacity_bytes,
            self.pool_initial_epochs_ahead,
        )
        .await
        .map_err(|e| BlobStoreError::PoolCreationFailed(format!("create_storage_pool PTB: {e}")))?;
        let tx_data = ptb
            .build_transaction_data(None)
            .await
            .map_err(|e| BlobStoreError::PoolCreationFailed(format!("build tx: {e}")))?;
        let resp =
            sui_transaction::sign_and_submit(&self.pearl, account_id, &self.rpc_url, tx_data)
                .await
                .map_err(|e| {
                    let msg = e.to_string();
                    if is_insufficient_balance(&msg) {
                        BlobStoreError::InsufficientBalance(msg)
                    } else {
                        BlobStoreError::PoolCreationFailed(msg)
                    }
                })?;

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
            BlobStoreError::PoolCreationFailed(
                "no StoragePool object in create_storage_pool response".into(),
            )
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
                    BlobStoreError::Http(
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
        let sender_address = sui_transaction::resolve_sender_address(&self.pearl, account_id)
            .await
            .map_err(|e| BlobStoreError::Http(format!("resolve sender address: {e}")))?;

        // 1. Encode the blob data.
        let encoding_config = self.node_client.encoding_config();
        let encoding = encoding_config.get_for_type(walrus_core::EncodingType::RS2);
        let (sliver_pairs, metadata) = encoding
            .encode_with_metadata(data.to_vec())
            .map_err(|e| BlobStoreError::Http(format!("encoding error: {e}")))?;

        let blob_obj_metadata = BlobObjectMetadata::try_from(&metadata)
            .map_err(|e| BlobStoreError::Http(format!("blob metadata error: {e}")))?;
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
            return Ok(StoreResult {
                blob_id: BlobId(walrus_blob_id_str),
                pooled_blob_object_id: Some(existing),
                encoded_size: None,
            });
        }

        // 2. Determine the StoragePool for this account (lazy-create on first write).
        let current_epoch = self
            .read_client
            .current_epoch()
            .await
            .map_err(|e| BlobStoreError::Http(format!("current_epoch: {e}")))?;
        let pool_state = match db::accounts::get_storage_pool(&self.db, account_id).await? {
            Some(state) => state,
            None => {
                self.create_pool_for_account(account_id, sender_address, current_epoch)
                    .await?
            }
        };

        let pool_object_id: ObjectID = pool_state
            .object_id
            .parse()
            .map_err(|e| BlobStoreError::Http(format!("invalid pool ObjectID: {e}")))?;

        // 3. Register PTB: optional capacity bump + register_pooled_blobs.
        let remaining = pool_state.reserved_encoded_bytes - pool_state.used_encoded_bytes;
        let grow_by: i64 = if (encoded_size as i64) > remaining {
            (encoded_size as i64) - remaining
        } else {
            0
        };
        let remaining_epochs = (pool_state.end_epoch - current_epoch as i64).max(1) as u32;

        let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender_address);
        if grow_by > 0 {
            ptb.increase_storage_pool_capacity(pool_object_id, grow_by as u64, remaining_epochs)
                .await
                .map_err(|e| {
                    let msg = format!("increase_storage_pool_capacity error: {e}");
                    if is_insufficient_balance(&msg) {
                        BlobStoreError::InsufficientBalance(msg)
                    } else {
                        BlobStoreError::Http(msg)
                    }
                })?;
        }
        ptb.register_pooled_blobs(
            pool_object_id,
            vec![blob_obj_metadata],
            BlobPersistence::Deletable,
        )
        .await
        .map_err(|e| {
            let msg = format!("register_pooled_blobs error: {e}");
            if is_insufficient_balance(&msg) {
                BlobStoreError::InsufficientBalance(msg)
            } else {
                BlobStoreError::Http(msg)
            }
        })?;
        let tx_data = ptb
            .build_transaction_data(None)
            .await
            .map_err(|e| BlobStoreError::Http(format!("build_transaction_data error: {e}")))?;

        let register_resp =
            sui_transaction::sign_and_submit(&self.pearl, account_id, &self.rpc_url, tx_data)
                .await
                .map_err(|e| {
                    let msg = format!("register tx error: {e}");
                    if is_insufficient_balance(&msg) {
                        BlobStoreError::InsufficientBalance(msg)
                    } else {
                        BlobStoreError::Http(msg)
                    }
                })?;

        tracing::info!("register tx digest: {:?}", register_resp.digest);

        let pooled_blob_object_id = extract_object_id_from_event(&register_resp, |module, name| {
            module == "events" && name == "PooledBlobRegistered"
        })
        .ok_or_else(|| {
            BlobStoreError::Http(
                "no PooledBlobRegistered event in register_pooled_blobs response".into(),
            )
        })?;

        db::accounts::update_pool_after_register(
            &self.db,
            account_id,
            grow_by,
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
            .map_err(|e| BlobStoreError::Http(format!("sliver upload error: {e}")))?;

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
            .map_err(|e| BlobStoreError::Http(format!("certify_pooled_blobs error: {e}")))?;
        let tx_data = ptb
            .build_transaction_data(None)
            .await
            .map_err(|e| BlobStoreError::Http(format!("build_transaction_data error: {e}")))?;

        sui_transaction::sign_and_submit(&self.pearl, account_id, &self.rpc_url, tx_data)
            .await
            .map_err(|e| {
                let msg = format!("certify tx error: {e}");
                if is_insufficient_balance(&msg) {
                    BlobStoreError::InsufficientBalance(msg)
                } else {
                    BlobStoreError::Http(msg)
                }
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
        let url = format!("{}/v1/blobs/{}", self.aggregator_url, blob_id);
        Box::pin(async move {
            let resp = self
                .http_client
                .get(&url)
                .send()
                .await
                .map_err(map_reqwest_err)?;

            let status = resp.status();
            if !status.is_success() {
                if status == reqwest::StatusCode::NOT_FOUND {
                    return Err(BlobStoreError::NotFound(url));
                }
                let body = resp.text().await.unwrap_or_default();
                return Err(BlobStoreError::Upstream {
                    status: status.as_u16(),
                    message: body,
                });
            }

            resp.bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(map_reqwest_err)
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
                .map_err(|e| BlobStoreError::Http(format!("invalid pool_id: {e}")))?;
            let walrus_blob_id: walrus_core::BlobId = blob_id
                .as_str()
                .parse()
                .map_err(|e| BlobStoreError::Http(format!("invalid walrus blob_id: {e}")))?;

            let sender_address = sui_transaction::resolve_sender_address(&self.pearl, &account_id)
                .await
                .map_err(|e| BlobStoreError::Http(format!("resolve sender address: {e}")))?;

            let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender_address);
            ptb.delete_pooled_blob(pool_object_id, walrus_blob_id)
                .await
                .map_err(|e| BlobStoreError::Http(format!("delete_pooled_blob PTB: {e}")))?;
            let tx_data = ptb
                .build_transaction_data(None)
                .await
                .map_err(|e| BlobStoreError::Http(format!("build_transaction_data: {e}")))?;
            sui_transaction::sign_and_submit(&self.pearl, &account_id, &self.rpc_url, tx_data)
                .await
                .map_err(|e| {
                    let msg = format!("delete tx: {e}");
                    if is_insufficient_balance(&msg) {
                        BlobStoreError::InsufficientBalance(msg)
                    } else {
                        BlobStoreError::Http(msg)
                    }
                })?;

            if encoded_size > 0 {
                db::accounts::update_pool_after_delete(&self.db, &account_id, encoded_size as i64)
                    .await?;
            }
            Ok(())
        })
    }

    fn exists(&self, blob_id: &BlobId) -> BoxFuture<'_, Result<bool, BlobStoreError>> {
        let url = format!("{}/v1/blobs/{}", self.aggregator_url, blob_id);
        Box::pin(async move {
            let resp = self
                .http_client
                .head(&url)
                .send()
                .await
                .map_err(map_reqwest_err)?;

            match resp.status() {
                reqwest::StatusCode::OK => Ok(true),
                reqwest::StatusCode::NOT_FOUND => Ok(false),
                status => Err(BlobStoreError::Upstream {
                    status: status.as_u16(),
                    message: String::new(),
                }),
            }
        })
    }
}
