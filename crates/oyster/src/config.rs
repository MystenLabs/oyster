use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: String,
    pub database_url: String,
    pub blob_store_path: PathBuf,
    pub enable_debug_endpoints: bool,
    pub pearl_grpc_url: Option<String>,
    pub pearl_service_secret: String,
    pub walrus_publisher_url: Option<String>,
    pub walrus_aggregator_url: Option<String>,
    pub walrus_default_epochs: u32,
    pub sui_rpc_url: Option<String>,
    pub walrus_system_object: Option<String>,
    pub walrus_staking_object: Option<String>,
    pub blob_extend_interval_secs: u64,
    pub blob_extend_lookahead_days: u32,
    pub blob_extend_epochs: u32,
    pub wal_coin_type: Option<String>,
}

impl Config {
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
