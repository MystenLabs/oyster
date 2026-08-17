//! Prometheus metric constants and recorder setup.

use metrics_exporter_prometheus::PrometheusHandle;

/// Counter: total HTTP requests handled, labelled by method, path, and status.
pub const HTTP_REQUESTS_TOTAL: &str = "oyster_http_requests_total";
/// Histogram: HTTP request duration in seconds, labelled by method and path.
pub const HTTP_REQUEST_DURATION: &str = "oyster_http_request_duration_seconds";

/// Counter: total blob store operations, labelled by operation and result.
pub const BLOB_STORE_OPS_TOTAL: &str = "oyster_blob_store_operations_total";

/// Counter: total Pearl gRPC calls, labelled by method and result.
pub const PEARL_GRPC_CALLS_TOTAL: &str = "oyster_pearl_grpc_calls_total";
/// Histogram: Pearl gRPC call latency in seconds, labelled by method.
pub const PEARL_GRPC_LATENCY: &str = "oyster_pearl_grpc_latency_seconds";

/// Gauge: number of active accounts.
pub const ACTIVE_ACCOUNTS: &str = "oyster_active_accounts";
/// Gauge: number of active blobs.
pub const ACTIVE_BLOBS: &str = "oyster_active_blobs";

// Extension worker metrics

/// Counter: total extension cycles run.
pub const EXTENSION_CYCLES_TOTAL: &str = "oyster_extension_cycles_total";
/// Counter: total pools successfully extended.
pub const EXTENSION_POOLS_EXTENDED_TOTAL: &str = "oyster_extension_pools_extended_total";
/// Counter: total extension errors, labelled by stage.
pub const EXTENSION_ERRORS_TOTAL: &str = "oyster_extension_errors_total";
/// Histogram: extension cycle wall-clock duration in seconds.
pub const EXTENSION_CYCLE_DURATION_SECONDS: &str = "oyster_extension_cycle_duration_seconds";
/// Gauge: number of pools found expiring in the current cycle.
pub const EXTENSION_POOLS_EXPIRING: &str = "oyster_extension_pools_expiring";
/// Gauge: number of pools processed (extended + errored) in the current cycle.
pub const EXTENSION_CYCLE_POOLS_PROCESSED: &str = "oyster_extension_cycle_pools_processed";
/// Counter: pools whose stale DB `pool_end_epoch` was repaired from the
/// on-chain value (an extension landed outside Oyster).
pub const EXTENSION_POOLS_REPAIRED_TOTAL: &str = "oyster_extension_pools_repaired_total";
/// Counter: pools confirmed expired on-chain and reset for lazy re-create.
pub const EXTENSION_POOLS_EXPIRED_RESET_TOTAL: &str = "oyster_extension_pools_expired_reset_total";
/// Counter: retry attempts skipped because the WAL balance pre-check showed
/// the wallet still cannot cover the extension cost (1 read RPC instead of
/// the full PTB-build + sign + execute chain).
pub const EXTENSION_BALANCE_PRECHECK_SKIPS_TOTAL: &str =
    "oyster_extension_balance_precheck_skips_total";

// Webhook metrics

/// Counter: total webhook delivery attempts.
pub const WEBHOOK_ATTEMPTS_TOTAL: &str = "oyster_webhook_attempts_total";
/// Counter: total successful webhook deliveries.
pub const WEBHOOK_SUCCESSES_TOTAL: &str = "oyster_webhook_successes_total";
/// Counter: total failed webhook deliveries (after retries exhausted or 4xx).
pub const WEBHOOK_FAILURES_TOTAL: &str = "oyster_webhook_failures_total";
/// Counter: number of times the webhook circuit breaker opened.
pub const WEBHOOK_CIRCUIT_OPEN_TOTAL: &str = "oyster_webhook_circuit_open_total";
/// Counter: deliveries skipped because the per-app private key is missing.
/// Should be zero in normal operation; non-zero indicates the migration was
/// bypassed or a row was rolled back below 017's invariants.
pub const WEBHOOK_SKIPPED_UNSIGNED_TOTAL: &str = "oyster_webhook_skipped_unsigned_total";
/// Counter: terminal funding-required webhook deliveries, labelled by
/// `outcome` ∈ {`success`, `failure`}. Incremented exactly once per
/// `WebhookClient::notify_funding_required` call (not per HTTP retry).
pub const FUNDING_REQUIRED_WEBHOOKS_TOTAL: &str = "oyster_funding_required_webhooks_total";

// Funding-shortfall response metrics

/// Counter: 402 "insufficient funds" responses returned by Axum routes,
/// labelled by `operation` (e.g. `store_blob`).
pub const INSUFFICIENT_FUNDS_RESPONSES_TOTAL: &str = "oyster_insufficient_funds_responses_total";

/// Counter: 413 "payload too large" responses, labelled by `reason`
/// ∈ {`body_limit`, `encoder_ceiling`}. `body_limit` fires when the
/// upload exceeds the static `MAX_BLOB_SIZE` cap (JSON-route 413 or
/// S3 `PutObject` `EntityTooLarge`); `encoder_ceiling` fires when it
/// exceeds the per-`n_shards` Walrus encoder ceiling.
pub const PAYLOAD_TOO_LARGE_RESPONSES_TOTAL: &str = "oyster_payload_too_large_responses_total";

/// Counter: register-PTB self-heal hits, labelled by `cause` ∈
/// {`db_miss`, `orphan_recovered`}. Incremented when a register PTB
/// aborts with `dynamic_field::add` code 0 (`EFieldAlreadyExists`) and
/// Oyster recovers by reading the existing on-chain `PooledBlob`
/// instead of returning 502. `db_miss` indicates a TOCTOU race against
/// the DB-side dedup index; `orphan_recovered` indicates the on-chain
/// `PooledBlob` outlived its DB row (typically a prior delete tx that
/// failed but whose DB row was dropped anyway — see
/// [`DELETE_DB_ONLY_TOTAL`]).
pub const REGISTER_DEDUP_SELF_HEAL_TOTAL: &str = "oyster_register_dedup_self_heal_total";

/// Counter: compensating on-chain delete attempts after a post-store
/// DB failure in `store_blob` / S3 `put_object`. Labelled by
/// `outcome` ∈ {`ok`, `failed`}. A non-zero `failed` count means rows
/// are landing in `dead_letter_orphans` and need a reaper pass.
pub const POST_STORE_COMPENSATION_TOTAL: &str = "oyster_post_store_compensation_total";

/// Counter: `delete_blob` calls where the on-chain Sui delete tx
/// failed with a non-`InsufficientBalance` error but Oyster still
/// removed the DB row to preserve idempotent DELETE semantics.
/// Labelled by `reason` ∈ {`upstream_error`, `internal_error`,
/// `other`} — bucketed coarsely on purpose to avoid unbounded
/// label cardinality from on-chain error messages. A non-zero rate
/// here means on-chain `PooledBlob` orphans are accumulating and is
/// the upstream of register-tx `EFieldAlreadyExists` aborts (see
/// [`REGISTER_DEDUP_SELF_HEAL_TOTAL`]).
pub const DELETE_DB_ONLY_TOTAL: &str = "oyster_delete_db_only_total";

/// Install the Prometheus recorder and return a handle for rendering.
pub fn setup() -> PrometheusHandle {
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}

/// Serve Prometheus metrics over HTTP at the given bind address.
pub async fn serve_metrics(handle: PrometheusHandle, bind_addr: String) {
    let app = axum::Router::new().route(
        "/metrics",
        axum::routing::get(move || {
            let handle = handle.clone();
            async move { handle.render() }
        }),
    );

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind metrics server to {bind_addr}: {e}"));
    tracing::info!("metrics server listening on {bind_addr}");
    axum::serve(listener, app)
        .await
        .expect("metrics server error");
}
