use std::path::PathBuf;

use walrus_sui::utils::BYTES_PER_UNIT_SIZE;

/// Secrets that can be provided via file instead of environment variable.
pub struct SecretOverrides {
    /// Pearl service secret, if loaded from a file.
    pub pearl_service_secret: Option<String>,
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
    /// Optional Sui RPC endpoint URL.
    pub sui_rpc_url: Option<String>,
    /// Optional Walrus system object ID on Sui.
    pub walrus_system_object: Option<String>,
    /// Optional Walrus staking object ID on Sui.
    pub walrus_staking_object: Option<String>,
    /// Initial epoch window for newly-created `StoragePool` objects.
    pub pool_initial_epochs_ahead: u32,
    /// Initial encoded capacity (bytes) reserved on a newly-created `StoragePool`.
    /// Walrus bills storage in `BYTES_PER_UNIT_SIZE` (1 MiB) units regardless of
    /// the stored value, so sub-MiB defaults pay the full MiB for a fractional
    /// reservation. Default is one full unit; subsequent uploads round growth up
    /// to the same quantum.
    pub pool_initial_encoded_capacity_bytes: u64,
    /// Number of Walrus epochs to extend `StoragePool` objects by on each cycle.
    pub pool_extend_epochs: u32,
    /// Number of Walrus epochs of runway: claim any pool whose `pool_end_epoch`
    /// is within `current_epoch + lookahead_epochs`. Operator picks a value
    /// appropriate to the deployed network's epoch duration.
    pub pool_extend_lookahead_epochs: u32,
    /// Sleep duration when the extension cycle finds no work (seconds).
    pub extension_idle_sleep_secs: u64,
    /// Sleep duration between batches when there is still work to drain (ms).
    pub extension_busy_sleep_ms: u64,
    /// Maximum number of pool rows to claim in a single cycle.
    pub extension_claim_batch_size: i64,
    /// Cooldown applied to a row by `claim_pools_for_extension`. Prevents the
    /// same row from being re-claimed (or re-notified) for this long, regardless
    /// of the attempt's outcome.
    pub extension_claim_cooldown_secs: u64,
    /// Socket address to bind the extension worker metrics HTTP server to.
    pub extension_metrics_bind_addr: String,
    /// Default `avg_blob_size` (unencoded bytes) assigned to newly-created
    /// accounts that omit the field. Drives the storage-cap inflation that
    /// makes `max_unencoded_bytes` a *lower* bound for blobs of this size.
    /// `0` disables inflation. Existing accounts are unaffected (they
    /// backfill to `0` via migration 020).
    pub default_avg_blob_size: u64,
    /// **Test-only.** When true, `validate_webhook_url` accepts `http://`
    /// in addition to `https://`. Production builds always leave this at
    /// `false`; integration tests flip it on so they can register a webhook
    /// URL pointing at a local axum test server. Never wired to an env var.
    pub allow_http_webhook_scheme: bool,
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
            sui_rpc_url: std::env::var("SUI_RPC_URL").ok(),
            walrus_system_object: std::env::var("WALRUS_SYSTEM_OBJECT").ok(),
            walrus_staking_object: std::env::var("WALRUS_STAKING_OBJECT").ok(),
            pool_initial_epochs_ahead: std::env::var("POOL_INITIAL_EPOCHS_AHEAD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            pool_initial_encoded_capacity_bytes: std::env::var(
                "POOL_INITIAL_ENCODED_CAPACITY_BYTES",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(BYTES_PER_UNIT_SIZE),
            pool_extend_epochs: std::env::var("POOL_EXTEND_EPOCHS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            pool_extend_lookahead_epochs: std::env::var("POOL_EXTEND_LOOKAHEAD_EPOCHS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(7),
            extension_idle_sleep_secs: std::env::var("EXTENSION_IDLE_SLEEP_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            extension_busy_sleep_ms: std::env::var("EXTENSION_BUSY_SLEEP_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(250),
            extension_claim_batch_size: std::env::var("EXTENSION_CLAIM_BATCH_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100),
            extension_claim_cooldown_secs: std::env::var("EXTENSION_CLAIM_COOLDOWN_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
            extension_metrics_bind_addr: std::env::var("OYSTER_EXTENSION_METRICS_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:50053".into()),
            default_avg_blob_size: std::env::var("OYSTER_DEFAULT_AVG_BLOB_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000_000),
            allow_http_webhook_scheme: false,
        }
    }

    /// Load configuration from environment variables, using sensible defaults.
    pub fn from_env() -> Self {
        Self::new(SecretOverrides {
            pearl_service_secret: None,
        })
    }
}
