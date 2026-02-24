use std::sync::Arc;

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
    pearl_account_id: String,
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

    // Resolve the Pearl account's Sui address once at startup.
    let sender_address: SuiAddress =
        match sui_transaction::resolve_sender_address(&pearl, &pearl_account_id).await {
            Ok(addr) => addr,
            Err(e) => {
                tracing::error!("failed to resolve sender address: {e}");
                return;
            }
        };

    tracing::info!("extension task sender address: {sender_address}");

    loop {
        tokio::time::sleep(config.check_interval).await;

        let cutoff = chrono::Utc::now()
            .checked_add_days(chrono::Days::new(config.lookahead_days as u64))
            .expect("valid date")
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let blobs = match db::blobs::get_expiring_blobs(&db, &cutoff, 100).await {
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

        for blob in &blobs {
            let sui_object_id = match &blob.sui_object_id {
                Some(id) => id,
                None => continue,
            };

            if let Err(e) = extend_single_blob(
                &read_client,
                &pearl,
                &pearl_account_id,
                &rpc_url,
                sender_address,
                sui_object_id,
                blob.size as u64,
                config.extend_epochs,
            )
            .await
            {
                tracing::warn!(
                    sui_object_id,
                    error = %e,
                    "failed to extend blob"
                );
                errors += 1;
                continue;
            }

            // Compute new expiry (approximate: current expires_at + extend_epochs * ~epoch_duration)
            // For now, just bump by extend_epochs days as an approximation.
            // The actual on-chain expiry is authoritative.
            if let Some(ref expires_at) = blob.expires_at
                && let Ok(parsed) =
                    chrono::NaiveDateTime::parse_from_str(expires_at, "%Y-%m-%d %H:%M:%S")
            {
                let new_expires = parsed + chrono::Duration::days(config.extend_epochs as i64);
                let new_expires_str = new_expires.format("%Y-%m-%d %H:%M:%S").to_string();
                if let Err(e) =
                    db::blobs::update_blob_expires_at(&db, sui_object_id, &new_expires_str).await
                {
                    tracing::warn!(sui_object_id, error = %e, "failed to update expires_at in DB");
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
