/// Pearl service configuration, read from environment variables.
#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub service_secret: String,
    pub master_seed: Vec<u8>,
    pub tls_cert_path: Option<String>,
    pub tls_key_path: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &self.database_url)
            .field("bind_addr", &self.bind_addr)
            .field("service_secret", &"[redacted]")
            .field("master_seed", &"[redacted]")
            .field("tls_cert_path", &self.tls_cert_path)
            .field("tls_key_path", &self.tls_key_path)
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

        let tls_cert_path = std::env::var("PEARL_TLS_CERT_PATH").ok();
        let tls_key_path = std::env::var("PEARL_TLS_KEY_PATH").ok();
        match (&tls_cert_path, &tls_key_path) {
            (Some(_), None) | (None, Some(_)) => {
                panic!("PEARL_TLS_CERT_PATH and PEARL_TLS_KEY_PATH must both be set or both unset");
            }
            _ => {}
        }

        Self {
            database_url: std::env::var("PEARL_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite:pearl.db?mode=rwc".into()),
            bind_addr: std::env::var("PEARL_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:50051".into()),
            service_secret: std::env::var("PEARL_SERVICE_SECRET")
                .unwrap_or_else(|_| "dev-secret".into()),
            master_seed,
            tls_cert_path,
            tls_key_path,
        }
    }
}
