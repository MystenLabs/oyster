use std::path::PathBuf;

/// Secrets that can be provided via file instead of environment variable.
pub struct SecretOverrides {
    /// Pearl service secret, if loaded from a file.
    pub pearl_service_secret: Option<String>,
    /// JWT signing secret, if loaded from a file.
    pub oyster_jwt_secret: Option<String>,
}

/// Oyster server configuration, loaded from environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    /// Socket address to bind the HTTP server to (e.g. `"0.0.0.0:3000"`).
    pub bind_addr: String,
    /// Database URL (SQLite or PostgreSQL).
    pub database_url: String,
    /// Filesystem path for the local blob store.
    pub blob_store_path: PathBuf,
    /// Optional Pearl gRPC endpoint URL.
    pub pearl_grpc_url: Option<String>,
    /// Shared secret for authenticating with Pearl.
    pub pearl_service_secret: String,
    /// Optional Walrus aggregator HTTP URL.
    pub walrus_aggregator_url: Option<String>,
    /// Optional Sui RPC endpoint URL.
    pub sui_rpc_url: Option<String>,
    /// Optional Walrus system object ID on Sui.
    pub walrus_system_object: Option<String>,
    /// Optional Walrus staking object ID on Sui.
    pub walrus_staking_object: Option<String>,
    /// Interval in seconds between blob extension checks.
    pub blob_extend_interval_secs: u64,
    /// Initial epoch window for newly-created `StoragePool` objects.
    pub pool_initial_epochs_ahead: u32,
    /// Initial encoded capacity (bytes) reserved on a newly-created
    /// `StoragePool`. Must be non-zero because the Move contract rejects
    /// zero-byte reservations; the first upload always bumps this via
    /// `increase_storage_pool_capacity`.
    pub pool_initial_encoded_capacity_bytes: u64,
    /// Number of epochs to extend `StoragePool` objects by.
    pub pool_extend_epochs: u32,
    /// Number of days to look ahead for expiring `StoragePool` objects.
    pub pool_extend_lookahead_days: u32,
    /// Socket address to bind the extension worker metrics HTTP server to.
    pub extension_metrics_bind_addr: String,
    /// Optional JWT signing secret for app authentication.
    pub jwt_secret: Option<String>,
}

impl Config {
    /// Load configuration from environment variables, with optional secret overrides.
    pub fn new(overrides: SecretOverrides) -> Self {
        Self {
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into()),
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite:oyster.db?mode=rwc".into()),
            blob_store_path: std::env::var("BLOB_STORE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("blob_store")),
            pearl_grpc_url: std::env::var("PEARL_GRPC_URL").ok(),
            pearl_service_secret: overrides
                .pearl_service_secret
                .or_else(|| std::env::var("PEARL_SERVICE_SECRET").ok())
                .expect(
                    "PEARL_SERVICE_SECRET is required (set env var or use --pearl-service-secret-file)",
                ),
            walrus_aggregator_url: std::env::var("WALRUS_AGGREGATOR_URL").ok(),
            sui_rpc_url: std::env::var("SUI_RPC_URL").ok(),
            walrus_system_object: std::env::var("WALRUS_SYSTEM_OBJECT").ok(),
            walrus_staking_object: std::env::var("WALRUS_STAKING_OBJECT").ok(),
            blob_extend_interval_secs: std::env::var("BLOB_EXTEND_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3600),
            pool_initial_epochs_ahead: std::env::var("POOL_INITIAL_EPOCHS_AHEAD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            pool_initial_encoded_capacity_bytes: std::env::var(
                "POOL_INITIAL_ENCODED_CAPACITY_BYTES",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1),
            pool_extend_epochs: std::env::var("POOL_EXTEND_EPOCHS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            pool_extend_lookahead_days: std::env::var("POOL_EXTEND_LOOKAHEAD_DAYS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            extension_metrics_bind_addr: std::env::var("OYSTER_EXTENSION_METRICS_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:50053".into()),
            jwt_secret: overrides
                .oyster_jwt_secret
                .or_else(|| std::env::var("OYSTER_JWT_SECRET").ok()),
        }
    }

    /// Load configuration from environment variables, using sensible defaults.
    pub fn from_env() -> Self {
        Self::new(SecretOverrides {
            pearl_service_secret: None,
            oyster_jwt_secret: None,
        })
    }
}
