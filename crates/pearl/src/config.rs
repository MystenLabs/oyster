use zeroize::Zeroizing;

/// Secrets that can be provided via file instead of environment variable.
pub struct SecretOverrides {
    /// Service secret, if loaded from a file.
    pub service_secret: Option<String>,
    /// Master seed hex, if loaded from a file.
    pub master_seed_hex: Option<Zeroizing<String>>,
}

/// Pearl service configuration, read from environment variables.
#[derive(Clone)]
pub struct Config {
    /// Socket address to bind the gRPC server to.
    pub bind_addr: String,
    /// Shared secret for authenticating incoming gRPC requests.
    pub service_secret: String,
    /// Master seed bytes for deterministic key derivation.
    pub master_seed: Zeroizing<Vec<u8>>,
    /// Optional path to a TLS certificate file for gRPC.
    pub tls_cert_path: Option<String>,
    /// Optional path to a TLS private key file for gRPC.
    pub tls_key_path: Option<String>,
    /// Socket address to bind the Prometheus metrics HTTP server to.
    pub metrics_bind_addr: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("service_secret", &"[redacted]")
            .field("master_seed", &"[redacted]")
            .field("tls_cert_path", &self.tls_cert_path)
            .field("tls_key_path", &self.tls_key_path)
            .field("metrics_bind_addr", &self.metrics_bind_addr)
            .finish()
    }
}

impl Config {
    /// Load configuration from environment variables, with optional secret overrides.
    pub fn new(overrides: SecretOverrides) -> Self {
        let service_secret = overrides
            .service_secret
            .or_else(|| std::env::var("PEARL_SERVICE_SECRET").ok())
            .expect(
                "PEARL_SERVICE_SECRET is required (set env var or use --pearl-service-secret-file)",
            );

        let master_seed_hex = overrides
            .master_seed_hex
            .or_else(|| std::env::var("PEARL_MASTER_SEED").ok().map(Zeroizing::new))
            .expect("PEARL_MASTER_SEED is required (set env var or use --pearl-master-seed-file)");
        let master_seed = Zeroizing::new(
            hex::decode(&*master_seed_hex).expect("PEARL_MASTER_SEED must be valid hex"),
        );
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
            bind_addr: std::env::var("PEARL_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:50051".into()),
            service_secret,
            master_seed,
            tls_cert_path,
            tls_key_path,
            metrics_bind_addr: std::env::var("PEARL_METRICS_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:50052".into()),
        }
    }

    /// Load configuration from environment variables.
    pub fn from_env() -> Self {
        Self::new(SecretOverrides {
            service_secret: None,
            master_seed_hex: None,
        })
    }
}
