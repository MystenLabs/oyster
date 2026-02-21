use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: String,
    pub database_url: String,
    pub blob_store_path: PathBuf,
    pub enable_debug_endpoints: bool,
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
        }
    }
}
