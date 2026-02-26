use std::{collections::HashMap, sync::Arc};

use sui_types::base_types::{ObjectID, SuiAddress};
use walrus_sui::client::{SuiReadClient, transaction_builder::WalrusPtbBuilder};

use crate::{
    db::{self, DbPool},
    pearl_client::PearlConnection,
    sui_transaction,
};

#[derive(Clone, Debug)]
pub struct ExtensionConfig {
    pub check_interval: std::time::Duration,
    pub lookahead_days: u32,
    pub extend_epochs: u32,
}

pub async fn run_extension_loop(
    db: DbPool,
    pearl: PearlConnection,
    rpc_url: String,
    system_object: ObjectID,
    staking_object: ObjectID,
    config: ExtensionConfig,
) {
    tracing::info!(
        "blob extension task started (interval={}s, lookahead={}d, epochs={})",
        config.check_interval.as_secs(),
        config.lookahead_days,
        config.extend_epochs,
    );

    let read_client =
        match sui_transaction::build_sui_read_client(&rpc_url, system_object, staking_object).await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("failed to build SuiReadClient for extension task: {e}");
                return;
            }
        };

    loop {
        tokio::time::sleep(config.check_interval).await;

        let cutoff = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(config.lookahead_days as u64))
            .expect("valid date")
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let blobs = match db::blobs::get_expiring_blobs_with_accounts(&db, &cutoff, 100).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("failed to query expiring blobs: {e}");
                continue;
            }
        };

        if blobs.is_empty() {
            tracing::debug!("no blobs need extension");
            continue;
        }

        tracing::info!("{} blob(s) approaching expiry, extending", blobs.len());

        let mut extended = 0u32;
        let mut errors = 0u32;
        let mut address_cache: HashMap<String, SuiAddress> = HashMap::new();

        for blob in &blobs {
            // Resolve sender address (cached per account per cycle).
            let sender_address = match address_cache.get(&blob.pearl_account_id) {
                Some(&addr) => addr,
                None => {
                    match sui_transaction::resolve_sender_address(&pearl, &blob.pearl_account_id)
                        .await
                    {
                        Ok(addr) => {
                            address_cache.insert(blob.pearl_account_id.clone(), addr);
                            addr
                        }
                        Err(e) => {
                            tracing::warn!(
                                pearl_account_id = %blob.pearl_account_id,
                                error = %e,
                                "failed to resolve sender address, skipping blob"
                            );
                            errors += 1;
                            continue;
                        }
                    }
                }
            };

            if let Err(e) = extend_single_blob(
                &read_client,
                &pearl,
                &blob.pearl_account_id,
                &rpc_url,
                sender_address,
                &blob.sui_object_id,
                blob.size as u64,
                config.extend_epochs,
            )
            .await
            {
                tracing::warn!(
                    sui_object_id = %blob.sui_object_id,
                    error = %e,
                    "failed to extend blob"
                );
                errors += 1;
                continue;
            }

            // Compute new expiry (approximate: current expires_at + extend_epochs * ~epoch_duration).
            // The actual on-chain expiry is authoritative.
            if let Ok(parsed) =
                chrono::NaiveDateTime::parse_from_str(&blob.expires_at, "%Y-%m-%d %H:%M:%S")
            {
                let new_expires = parsed + chrono::Duration::days(config.extend_epochs as i64);
                let new_expires_str = new_expires.format("%Y-%m-%d %H:%M:%S").to_string();
                if let Err(e) =
                    db::blobs::update_blob_expires_at(&db, &blob.sui_object_id, &new_expires_str)
                        .await
                {
                    tracing::warn!(
                        sui_object_id = %blob.sui_object_id,
                        error = %e,
                        "failed to update expires_at in DB"
                    );
                }
            }

            extended += 1;
        }

        tracing::info!(extended, errors, "extension cycle complete");
    }
}

#[allow(clippy::too_many_arguments)]
async fn extend_single_blob(
    read_client: &Arc<SuiReadClient>,
    pearl: &PearlConnection,
    pearl_account_id: &str,
    rpc_url: &str,
    sender_address: SuiAddress,
    sui_object_id: &str,
    encoded_size: u64,
    extend_epochs: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let object_id: ObjectID = sui_object_id.parse()?;

    let mut ptb = WalrusPtbBuilder::new(read_client.clone(), sender_address);
    ptb.extend_blob(object_id.into(), extend_epochs, encoded_size)
        .await?;

    let tx_data = ptb.build_transaction_data(None).await?;
    sui_transaction::sign_and_submit(pearl, pearl_account_id, rpc_url, tx_data).await?;

    Ok(())
}
