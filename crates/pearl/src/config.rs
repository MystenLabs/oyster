/// Pearl service configuration, read from environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub service_secret: String,
    pub sui_rpc_url: Option<String>,
    pub wal_coin_type: Option<String>,
    pub reconciliation_interval_secs: u64,
    pub pending_tx_timeout_minutes: i64,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            database_url: std::env::var("PEARL_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite:pearl.db?mode=rwc".into()),
            bind_addr: std::env::var("PEARL_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:50051".into()),
            service_secret: std::env::var("PEARL_SERVICE_SECRET")
                .unwrap_or_else(|_| "dev-secret".into()),
            sui_rpc_url: std::env::var("SUI_RPC_URL").ok(),
            wal_coin_type: std::env::var("WAL_COIN_TYPE").ok(),
            reconciliation_interval_secs: std::env::var("PEARL_RECONCILIATION_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300),
            pending_tx_timeout_minutes: std::env::var("PEARL_PENDING_TX_TIMEOUT_MINUTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
        }
    }
}
