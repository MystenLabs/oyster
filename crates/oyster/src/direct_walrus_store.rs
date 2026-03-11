use std::{future::Future, pin::Pin, sync::Arc};

use sui_sdk::rpc_types::ObjectChange;
use sui_types::base_types::ObjectID;
use walrus_core::{encoding::EncodingFactory as _, messages::BlobPersistenceType};
use walrus_sdk::{client::WalrusNodeClient, config::ClientConfig, uploader::TailHandling};
use walrus_sui::client::{
    BlobObjectMetadata,
    BlobPersistence,
    SuiReadClient,
    transaction_builder::WalrusPtbBuilder,
};

use crate::{
    blob_store::{BlobId, BlobStore, BlobStoreError, StoreResult},
    pearl_client::PearlConnection,
    sui_transaction,
};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Blob store that writes directly to Walrus via on-chain Sui transactions.
pub struct DirectWalrusBlobStore {
    node_client: WalrusNodeClient<SuiReadClient>,
    read_client: Arc<SuiReadClient>,
    pearl: PearlConnection,
    rpc_url: String,
    epochs: u32,
    aggregator_url: String,
    http_client: reqwest::Client,
}

impl DirectWalrusBlobStore {
    /// Create a new direct Walrus blob store connected to the given Sui RPC and Walrus cluster.
    pub async fn new(
        rpc_url: String,
        aggregator_url: String,
        system_object: ObjectID,
        staking_object: ObjectID,
        pearl: PearlConnection,
        epochs: u32,
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
            epochs,
            aggregator_url,
            http_client: reqwest::Client::new(),
        })
    }

    async fn store_impl(
        &self,
        data: &[u8],
        account_id: &str,
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

        // 2. Register on-chain (reserve_space + register_blob in one PTB).
        let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender_address);
        let storage_arg = ptb
            .reserve_space(blob_obj_metadata.encoded_size, self.epochs)
            .await
            .map_err(|e| BlobStoreError::Http(format!("reserve_space error: {e}")))?;

        let blob_arg = ptb
            .register_blob(
                storage_arg.into(),
                blob_obj_metadata,
                BlobPersistence::Deletable,
            )
            .await
            .map_err(|e| BlobStoreError::Http(format!("register_blob error: {e}")))?;
        let _ = blob_arg;

        let tx_data = ptb
            .build_transaction_data(None)
            .await
            .map_err(|e| BlobStoreError::Http(format!("build_transaction_data error: {e}")))?;

        let register_resp =
            sui_transaction::sign_and_submit(&self.pearl, account_id, &self.rpc_url, tx_data)
                .await
                .map_err(|e| BlobStoreError::Http(format!("register tx error: {e}")))?;

        tracing::info!("register tx digest: {:?}", register_resp.digest);

        // Extract the created blob object ID from object_changes.
        let blob_sui_object_id = register_resp
            .object_changes
            .as_ref()
            .and_then(|changes| {
                changes.iter().find_map(|c| {
                    if let ObjectChange::Created { object_id, .. } = c {
                        Some(*object_id)
                    } else {
                        None
                    }
                })
            })
            .ok_or_else(|| {
                BlobStoreError::Http("no created object in register_blob response".to_string())
            })?;

        // 3. Upload slivers to storage nodes and collect certificate.
        let persistence_type = BlobPersistenceType::Deletable {
            object_id: blob_sui_object_id.into(),
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

        // 4. Certify on-chain.
        let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender_address);
        ptb.certify_blob(blob_sui_object_id.into(), &certificate)
            .await
            .map_err(|e| BlobStoreError::Http(format!("certify_blob error: {e}")))?;

        let tx_data = ptb
            .build_transaction_data(None)
            .await
            .map_err(|e| BlobStoreError::Http(format!("build_transaction_data error: {e}")))?;

        sui_transaction::sign_and_submit(&self.pearl, account_id, &self.rpc_url, tx_data)
            .await
            .map_err(|e| BlobStoreError::Http(format!("certify tx error: {e}")))?;

        Ok(StoreResult {
            blob_id: BlobId(walrus_blob_id.to_string()),
            sui_object_id: Some(blob_sui_object_id.to_string()),
        })
    }
}

impl BlobStore for DirectWalrusBlobStore {
    fn store(
        &self,
        data: &[u8],
        account_id: &str,
    ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>> {
        let data = data.to_vec();
        let account_id = account_id.to_string();
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
                .map_err(|e| BlobStoreError::Http(e.to_string()))?;

            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Err(BlobStoreError::NotFound(url));
            }

            if !resp.status().is_success() {
                return Err(BlobStoreError::Http(format!(
                    "aggregator returned {}",
                    resp.status()
                )));
            }

            resp.bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| BlobStoreError::Http(e.to_string()))
        })
    }

    fn delete(
        &self,
        _blob_id: &BlobId,
        sui_object_id: Option<&str>,
        account_id: &str,
    ) -> BoxFuture<'_, Result<(), BlobStoreError>> {
        let sui_object_id = sui_object_id.map(String::from);
        let account_id = account_id.to_string();
        Box::pin(async move {
            let Some(sui_oid) = sui_object_id else {
                return Ok(());
            };

            let sender_address = sui_transaction::resolve_sender_address(&self.pearl, &account_id)
                .await
                .map_err(|e| BlobStoreError::Http(format!("resolve sender address: {e}")))?;
            let object_id: ObjectID = sui_oid
                .parse()
                .map_err(|e| BlobStoreError::Http(format!("invalid sui_object_id: {e}")))?;

            let mut ptb = WalrusPtbBuilder::new(self.read_client.clone(), sender_address);
            ptb.delete_blob(object_id.into())
                .await
                .map_err(|e| BlobStoreError::Http(format!("delete_blob PTB: {e}")))?;
            let tx_data = ptb
                .build_transaction_data(None)
                .await
                .map_err(|e| BlobStoreError::Http(format!("build_transaction_data: {e}")))?;
            sui_transaction::sign_and_submit(&self.pearl, &account_id, &self.rpc_url, tx_data)
                .await
                .map_err(|e| BlobStoreError::Http(format!("delete tx: {e}")))?;

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
                .map_err(|e| BlobStoreError::Http(e.to_string()))?;

            match resp.status() {
                reqwest::StatusCode::OK => Ok(true),
                reqwest::StatusCode::NOT_FOUND => Ok(false),
                status => Err(BlobStoreError::Http(format!(
                    "aggregator returned {status}"
                ))),
            }
        })
    }
}
