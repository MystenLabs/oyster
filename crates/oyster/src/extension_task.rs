use std::{collections::HashMap, sync::Arc, time::Instant};

use metrics::{counter, gauge, histogram};
use sui_types::base_types::{ObjectID, SuiAddress};
use walrus_sui::client::{SuiReadClient, transaction_builder::WalrusPtbBuilder};

use crate::{
    AccountId,
    db::{self, DbPool},
    metrics::{
        EXTENSION_BLOBS_EXPIRING,
        EXTENSION_BLOBS_EXTENDED_TOTAL,
        EXTENSION_CYCLE_BLOBS_PROCESSED,
        EXTENSION_CYCLE_DURATION_SECONDS,
        EXTENSION_CYCLES_TOTAL,
        EXTENSION_ERRORS_TOTAL,
    },
    pearl_client::PearlConnection,
    sui_transaction,
    webhook::{self, InsufficientFundsPayload, WebhookClient},
};

/// Configuration for the background blob extension task.
#[derive(Clone, Debug)]
pub struct ExtensionConfig {
    /// How often to check for expiring blobs.
    pub check_interval: std::time::Duration,
    /// How many days ahead to look for expiring blobs.
    pub lookahead_days: u32,
    /// Number of Walrus epochs to extend blobs by.
    pub extend_epochs: u32,
}

/// Run the background loop that periodically extends expiring blobs on Walrus.
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

        counter!(EXTENSION_CYCLES_TOTAL).increment(1);
        let cycle_start = Instant::now();

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

        gauge!(EXTENSION_BLOBS_EXPIRING).set(blobs.len() as f64);

        if blobs.is_empty() {
            tracing::debug!("no blobs need extension");
            gauge!(EXTENSION_CYCLE_BLOBS_PROCESSED).set(0.0);
            histogram!(EXTENSION_CYCLE_DURATION_SECONDS)
                .record(cycle_start.elapsed().as_secs_f64());
            continue;
        }

        tracing::info!("{} blob(s) approaching expiry, extending", blobs.len());

        let mut extended = 0u32;
        let mut errors = 0u32;
        let mut address_cache: HashMap<AccountId, SuiAddress> = HashMap::new();
        let mut webhook_clients: HashMap<String, WebhookClient> = HashMap::new();

        for blob in &blobs {
            // Resolve sender address (cached per account per cycle).
            let sender_address = match address_cache.get(&blob.account_id) {
                Some(&addr) => addr,
                None => {
                    match sui_transaction::resolve_sender_address(&pearl, &blob.account_id).await {
                        Ok(addr) => {
                            address_cache.insert(blob.account_id, addr);
                            addr
                        }
                        Err(e) => {
                            tracing::warn!(
                                account_id = %blob.account_id,
                                error = %e,
                                "failed to resolve sender address, skipping blob"
                            );
                            counter!(EXTENSION_ERRORS_TOTAL, "stage" => "resolve_address")
                                .increment(1);
                            errors += 1;
                            continue;
                        }
                    }
                }
            };

            if let Err(e) = extend_single_blob(
                &read_client,
                &pearl,
                &blob.account_id,
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
                counter!(EXTENSION_ERRORS_TOTAL, "stage" => "extend_blob").increment(1);
                errors += 1;

                if let Some(ref url) = blob.webhook_url
                    && webhook::is_insufficient_funds_error(e.as_ref())
                {
                    let wh = webhook_clients
                        .entry(url.clone())
                        .or_insert_with(|| WebhookClient::new(url.clone()));
                    wh.notify_insufficient_funds(&InsufficientFundsPayload {
                        account_id: blob.account_id,
                        address: sender_address.to_string(),
                        error: e.to_string(),
                    })
                    .await;
                }

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
                    counter!(EXTENSION_ERRORS_TOTAL, "stage" => "db_update").increment(1);
                }
            }

            counter!(EXTENSION_BLOBS_EXTENDED_TOTAL).increment(1);
            extended += 1;
        }

        gauge!(EXTENSION_CYCLE_BLOBS_PROCESSED).set((extended + errors) as f64);
        histogram!(EXTENSION_CYCLE_DURATION_SECONDS).record(cycle_start.elapsed().as_secs_f64());
        tracing::info!(extended, errors, "extension cycle complete");
    }
}

#[allow(clippy::too_many_arguments)]
async fn extend_single_blob(
    read_client: &Arc<SuiReadClient>,
    pearl: &PearlConnection,
    account_id: &AccountId,
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
    sui_transaction::sign_and_submit(pearl, account_id, rpc_url, tx_data).await?;

    Ok(())
}
