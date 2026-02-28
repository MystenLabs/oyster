use std::path::PathBuf;

/// Oyster server configuration, loaded from environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    /// Socket address to bind the HTTP server to (e.g. `"0.0.0.0:3000"`).
    pub bind_addr: String,
    /// SQLite database URL.
    pub database_url: String,
    /// Filesystem path for the local blob store.
    pub blob_store_path: PathBuf,
    /// Whether debug-only endpoints are enabled.
    pub enable_debug_endpoints: bool,
    /// Optional Pearl gRPC endpoint URL.
    pub pearl_grpc_url: Option<String>,
    /// Shared secret for authenticating with Pearl.
    pub pearl_service_secret: String,
    /// Optional Walrus publisher HTTP URL.
    pub walrus_publisher_url: Option<String>,
    /// Optional Walrus aggregator HTTP URL.
    pub walrus_aggregator_url: Option<String>,
    /// Default number of Walrus storage epochs.
    pub walrus_default_epochs: u32,
    /// Optional Sui RPC endpoint URL.
    pub sui_rpc_url: Option<String>,
    /// Optional Walrus system object ID on Sui.
    pub walrus_system_object: Option<String>,
    /// Optional Walrus staking object ID on Sui.
    pub walrus_staking_object: Option<String>,
    /// Interval in seconds between blob extension checks.
    pub blob_extend_interval_secs: u64,
    /// Number of days to look ahead for expiring blobs.
    pub blob_extend_lookahead_days: u32,
    /// Number of epochs to extend blobs by.
    pub blob_extend_epochs: u32,
    /// Optional WAL coin type string for balance checks.
    pub wal_coin_type: Option<String>,
}

impl Config {
    /// Load configuration from environment variables, using sensible defaults.
    pub fn from_env() -> Self {
        Self {
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into()),
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite:oyster.db?mode=rwc".into()),
            blob_store_path: std::env::var("BLOB_STORE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("blob_store")),
            enable_debug_endpoints: std::env::var("ENABLE_DEBUG")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(true),
            pearl_grpc_url: std::env::var("PEARL_GRPC_URL").ok(),
            pearl_service_secret: std::env::var("PEARL_SERVICE_SECRET")
                .unwrap_or_else(|_| "dev-secret".into()),
            walrus_publisher_url: std::env::var("WALRUS_PUBLISHER_URL").ok(),
            walrus_aggregator_url: std::env::var("WALRUS_AGGREGATOR_URL").ok(),
            walrus_default_epochs: std::env::var("WALRUS_DEFAULT_EPOCHS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            sui_rpc_url: std::env::var("SUI_RPC_URL").ok(),
            walrus_system_object: std::env::var("WALRUS_SYSTEM_OBJECT").ok(),
            walrus_staking_object: std::env::var("WALRUS_STAKING_OBJECT").ok(),
            blob_extend_interval_secs: std::env::var("BLOB_EXTEND_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3600),
            blob_extend_lookahead_days: std::env::var("BLOB_EXTEND_LOOKAHEAD_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            blob_extend_epochs: std::env::var("BLOB_EXTEND_EPOCHS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            wal_coin_type: std::env::var("WAL_COIN_TYPE").ok(),
        }
    }
}
