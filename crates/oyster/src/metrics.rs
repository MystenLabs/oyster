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

/// Install the Prometheus recorder and return a handle for rendering.
pub fn setup() -> PrometheusHandle {
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}
