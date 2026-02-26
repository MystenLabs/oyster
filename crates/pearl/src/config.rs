/// Pearl service configuration, read from environment variables.
#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub service_secret: String,
    pub master_seed: Vec<u8>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &self.database_url)
            .field("bind_addr", &self.bind_addr)
            .field("service_secret", &"[redacted]")
            .field("master_seed", &"[redacted]")
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Self {
        let master_seed_hex =
            std::env::var("PEARL_MASTER_SEED").expect("PEARL_MASTER_SEED env var is required");
        let master_seed =
            hex::decode(&master_seed_hex).expect("PEARL_MASTER_SEED must be valid hex");
        assert!(
            master_seed.len() >= 32,
            "PEARL_MASTER_SEED must be at least 32 bytes (64 hex chars), got {} bytes",
            master_seed.len()
        );

        Self {
            database_url: std::env::var("PEARL_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite:pearl.db?mode=rwc".into()),
            bind_addr: std::env::var("PEARL_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:50051".into()),
            service_secret: std::env::var("PEARL_SERVICE_SECRET")
                .unwrap_or_else(|_| "dev-secret".into()),
            master_seed,
        }
    }
}
