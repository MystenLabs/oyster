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
/// Counter: total blobs successfully extended.
pub const EXTENSION_BLOBS_EXTENDED_TOTAL: &str = "oyster_extension_blobs_extended_total";
/// Counter: total extension errors, labelled by stage.
pub const EXTENSION_ERRORS_TOTAL: &str = "oyster_extension_errors_total";
/// Histogram: extension cycle wall-clock duration in seconds.
pub const EXTENSION_CYCLE_DURATION_SECONDS: &str = "oyster_extension_cycle_duration_seconds";
/// Gauge: number of blobs found expiring in the current cycle.
pub const EXTENSION_BLOBS_EXPIRING: &str = "oyster_extension_blobs_expiring";
/// Gauge: number of blobs processed (extended + errored) in the current cycle.
pub const EXTENSION_CYCLE_BLOBS_PROCESSED: &str = "oyster_extension_cycle_blobs_processed";

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
