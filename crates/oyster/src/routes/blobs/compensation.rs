//! Post-store DB-failure compensation: if `blob_store.store()` succeeds
//! but the follow-up DB write fails (e.g. `blobs_bucket_name_fkey` race
//! because the bucket was deleted concurrently), the on-chain
//! `PooledBlob` would be left orphaned — register PTB already paid SUI
//! + WAL — and would later trigger a `EFieldAlreadyExists` self-heal on
//! the next upload of the same content. This module issues a bounded
//! compensating delete; if that also fails, the orphan is recorded in
//! `dead_letter_orphans` for a future reaper.

use std::time::Duration;

use crate::{
    AccountId,
    AppState,
    blob_store::{BlobId, StoreResult},
    db,
    metrics as metric_names,
};

/// Retry schedule for the compensating delete. Three attempts with
/// short backoffs — the on-chain delete is independently retried by a
/// future reaper for anything that lands in `dead_letter_orphans`.
const RETRY_BACKOFFS: &[Duration] = &[Duration::from_millis(100), Duration::from_millis(250)];

/// Attempt to delete the on-chain `PooledBlob` produced by a
/// successful `blob_store.store()` call whose follow-up DB write
/// failed. Skips the delete (and dead-letter) when there's no on-chain
/// object to compensate (LocalBlobStore, or the dedup short-circuit on
/// `DirectWalrusBlobStore` which leaves `encoded_size = None`).
///
/// Errors are intentionally swallowed: the caller is about to return a
/// 4xx for the original DB failure and we don't want to mask that.
/// Failure to compensate is recorded via metrics + dead-letter row.
pub(crate) async fn compensate_after_failed_db_insert(
    state: &AppState,
    account_id: &AccountId,
    result: &StoreResult,
    db_error: &sqlx::Error,
) {
    // Skip when there's no on-chain object to delete:
    //  * `LocalBlobStore` leaves `encoded_size = None`.
    //  * The `DirectWalrusBlobStore` dedup short-circuit also leaves
    //    `encoded_size = None`, and in that case the on-chain
    //    `PooledBlob` is still referenced by the pre-existing row that
    //    drove the dedup — deleting it would be wrong.
    let Some(encoded_size) = result.encoded_size else {
        return;
    };

    // Look up the StoragePool id (delete() needs the pool id, not the
    // PooledBlob id). If this lookup itself fails, dead-letter
    // straight away — we cannot safely issue the on-chain delete.
    let pool_id = match db::accounts::get_storage_pool(&state.db, account_id).await {
        Ok(Some(s)) => Some(s.object_id),
        Ok(None) => None,
        Err(e) => {
            metrics::counter!(
                metric_names::POST_STORE_COMPENSATION_TOTAL,
                "outcome" => "failed",
            )
            .increment(1);
            dead_letter_or_log(
                &state.db,
                result.blob_id.as_str(),
                account_id,
                None,
                encoded_size as i64,
                db_error,
                &e,
            )
            .await;
            return;
        }
    };

    for attempt in 0..=RETRY_BACKOFFS.len() {
        match state
            .blob_store
            .delete(
                &BlobId(result.blob_id.as_str().to_string()),
                pool_id.as_deref(),
                encoded_size,
                account_id,
            )
            .await
        {
            Ok(()) => {
                metrics::counter!(
                    metric_names::POST_STORE_COMPENSATION_TOTAL,
                    "outcome" => "ok",
                )
                .increment(1);
                tracing::warn!(
                    blob_id = %result.blob_id,
                    %account_id,
                    db_error = %db_error,
                    "compensated on-chain blob after post-store DB failure",
                );
                return;
            }
            Err(e) => {
                if let Some(backoff) = RETRY_BACKOFFS.get(attempt) {
                    tracing::warn!(
                        error = %e,
                        attempt = attempt + 1,
                        "compensating delete failed; retrying",
                    );
                    tokio::time::sleep(*backoff).await;
                    continue;
                }
                metrics::counter!(
                    metric_names::POST_STORE_COMPENSATION_TOTAL,
                    "outcome" => "failed",
                )
                .increment(1);
                dead_letter_or_log(
                    &state.db,
                    result.blob_id.as_str(),
                    account_id,
                    pool_id.as_deref(),
                    encoded_size as i64,
                    db_error,
                    &e,
                )
                .await;
                return;
            }
        }
    }
}

/// Persist the orphan to `dead_letter_orphans`. On insert failure (DB
/// fully wedged), log at `error!` so the orphan is at least surfaced
/// in logs for a human/operator to pick up — the alternative is silent
/// loss.
async fn dead_letter_or_log<E: std::fmt::Display>(
    db_pool: &db::DbPool,
    blob_id: &str,
    account_id: &AccountId,
    pool_id: Option<&str>,
    encoded_size: i64,
    original_db_error: &sqlx::Error,
    compensation_error: &E,
) {
    let original = original_db_error.to_string();
    let comp = compensation_error.to_string();
    match db::dead_letter_orphans::insert_orphan(
        db_pool,
        blob_id,
        account_id,
        pool_id,
        encoded_size,
        &original,
        Some(&comp),
    )
    .await
    {
        Ok(()) => {
            tracing::error!(
                blob_id = %blob_id,
                %account_id,
                pool_id = ?pool_id,
                encoded_size,
                original_db_error = %original,
                compensation_error = %comp,
                "recorded on-chain orphan in dead_letter_orphans after failed compensation",
            );
        }
        Err(insert_err) => {
            tracing::error!(
                blob_id = %blob_id,
                %account_id,
                pool_id = ?pool_id,
                encoded_size,
                original_db_error = %original,
                compensation_error = %comp,
                insert_error = %insert_err,
                "FAILED to record on-chain orphan; on-chain PooledBlob is leaked",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::{
        AccountId,
        AppId,
        AppState,
        blob_store::{BlobId, BlobStore, BlobStoreError, StoreResult},
        config::Config,
    };

    type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

    /// Minimal test double that records `delete()` calls and never
    /// touches a real blob store.
    struct RecordingBlobStore {
        delete_calls: Mutex<Vec<String>>,
    }
    impl RecordingBlobStore {
        fn new() -> Self {
            Self {
                delete_calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.delete_calls.lock().unwrap().clone()
        }
    }
    impl BlobStore for RecordingBlobStore {
        fn store(
            &self,
            _data: &[u8],
            _account_id: &AccountId,
        ) -> BoxFuture<'_, Result<StoreResult, BlobStoreError>> {
            unreachable!("RecordingBlobStore::store should not be called")
        }
        fn read(&self, _blob_id: &BlobId) -> BoxFuture<'_, Result<Vec<u8>, BlobStoreError>> {
            unreachable!()
        }
        fn delete(
            &self,
            blob_id: &BlobId,
            _pool_id: Option<&str>,
            _encoded_size: u64,
            _account_id: &AccountId,
        ) -> BoxFuture<'_, Result<(), BlobStoreError>> {
            self.delete_calls.lock().unwrap().push(blob_id.0.clone());
            Box::pin(async { Ok(()) })
        }
        fn exists(&self, _blob_id: &BlobId) -> BoxFuture<'_, Result<bool, BlobStoreError>> {
            unreachable!()
        }
    }

    fn dummy_config() -> Config {
        Config {
            bind_addr: "unused".into(),
            database_url: "sqlite::memory:".into(),
            blob_store_path: std::path::PathBuf::from("/tmp"),
            pearl_grpc_url: None,
            pearl_service_secret: "test-secret".into(),
            sui_rpc_url: None,
            walrus_system_object: None,
            walrus_staking_object: None,
            pool_initial_epochs_ahead: 5,
            pool_initial_encoded_capacity_bytes: walrus_sui::utils::BYTES_PER_UNIT_SIZE,
            pool_extend_epochs: 5,
            pool_extend_lookahead_epochs: 7,
            extension_idle_sleep_secs: 30,
            extension_busy_sleep_ms: 250,
            extension_claim_batch_size: 100,
            extension_claim_cooldown_secs: 60,
            extension_metrics_bind_addr: "unused".into(),
            allow_http_webhook_scheme: true,
        }
    }

    /// `encoded_size = None` (LocalBlobStore / dedup short-circuit) must
    /// skip the on-chain delete entirely.
    #[tokio::test]
    async fn skips_when_encoded_size_is_none() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        let account = db::accounts::create_account(&pool, &AppId::INTERNAL, None, None)
            .await
            .unwrap();
        let blob_store = Arc::new(RecordingBlobStore::new());
        let state = AppState {
            db: pool.clone(),
            blob_store: blob_store.clone() as Arc<dyn BlobStore>,
            pearl: None,
            read_client: None,
            config: dummy_config(),
            metrics_handle: None,
        };

        let result = StoreResult {
            blob_id: BlobId("deadbeef".into()),
            pooled_blob_object_id: Some("0xpool".into()),
            encoded_size: None,
        };
        let db_err = sqlx::Error::RowNotFound;

        compensate_after_failed_db_insert(&state, &account.id, &result, &db_err).await;

        assert!(
            blob_store.calls().is_empty(),
            "no compensating delete should fire when encoded_size is None",
        );
    }
}
