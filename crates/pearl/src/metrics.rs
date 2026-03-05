//! Prometheus metric constants and recorder setup.

use metrics_exporter_prometheus::PrometheusHandle;

/// Counter: total gRPC requests handled, labelled by method and status.
pub const GRPC_REQUESTS_TOTAL: &str = "pearl_grpc_requests_total";
/// Histogram: gRPC request duration in seconds, labelled by method.
pub const GRPC_REQUEST_DURATION: &str = "pearl_grpc_request_duration_seconds";

/// Counter: total accounts created.
pub const ACCOUNTS_CREATED_TOTAL: &str = "pearl_accounts_created_total";
/// Counter: total sign-transaction calls, labelled by result.
pub const SIGN_TRANSACTIONS_TOTAL: &str = "pearl_sign_transactions_total";

/// Gauge: current number of accounts.
pub const ACCOUNTS_TOTAL: &str = "pearl_accounts_total";

/// Install the Prometheus recorder and return a handle for rendering.
pub fn setup() -> PrometheusHandle {
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}
