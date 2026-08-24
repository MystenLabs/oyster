use std::collections::BTreeMap;

use zeroize::Zeroizing;

/// Environment-variable prefix for master seeds beyond version 1
/// (e.g. `PEARL_MASTER_SEED_V2`).
const VERSIONED_SEED_PREFIX: &str = "PEARL_MASTER_SEED_V";

/// Secrets that can be provided via file instead of environment variable.
pub struct SecretOverrides {
    /// Service secret, if loaded from a file.
    pub service_secret: Option<String>,
    /// Version-1 master seed hex, if loaded from a file.
    pub master_seed_hex: Option<Zeroizing<String>>,
    /// Master seeds for versions ≥ 2 loaded from files, as
    /// `(version, hex)` pairs.
    pub versioned_master_seeds_hex: Vec<(u32, Zeroizing<String>)>,
}

/// Pearl service configuration, read from environment variables.
#[derive(Clone)]
pub struct Config {
    /// Socket address to bind the gRPC server to.
    pub bind_addr: String,
    /// Shared secret for authenticating incoming gRPC requests.
    pub service_secret: String,
    /// Master seeds for deterministic key derivation, by key version.
    /// Version 1 comes from `PEARL_MASTER_SEED` (kept unsuffixed for
    /// compatibility with existing deployments); versions ≥ 2 come from
    /// `PEARL_MASTER_SEED_V<N>`.
    pub master_seeds: BTreeMap<u32, Zeroizing<Vec<u8>>>,
    /// Seed version newly created accounts should be stamped with
    /// (`PEARL_ACTIVE_KEY_VERSION`, default 1). Rotating to a new seed
    /// means configuring its `PEARL_MASTER_SEED_V<N>` and pointing this
    /// at it; older versions stay configured so existing accounts keep
    /// deriving their original keys.
    pub active_key_version: u32,
    /// Optional path to a TLS certificate file for gRPC.
    pub tls_cert_path: Option<String>,
    /// Optional path to a TLS private key file for gRPC.
    pub tls_key_path: Option<String>,
    /// Socket address to bind the Prometheus metrics HTTP server to.
    pub metrics_bind_addr: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let seed_versions: Vec<u32> = self.master_seeds.keys().copied().collect();
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("service_secret", &"[redacted]")
            .field(
                "master_seeds",
                &format!("[redacted; versions {seed_versions:?}]"),
            )
            .field("active_key_version", &self.active_key_version)
            .field("tls_cert_path", &self.tls_cert_path)
            .field("tls_key_path", &self.tls_key_path)
            .field("metrics_bind_addr", &self.metrics_bind_addr)
            .finish()
    }
}

/// Decode and validate one master seed from its hex encoding.
fn decode_seed(version: u32, seed_hex: &str) -> Zeroizing<Vec<u8>> {
    let seed = Zeroizing::new(
        hex::decode(seed_hex)
            .unwrap_or_else(|_| panic!("master seed for key version {version} must be valid hex")),
    );
    assert!(
        seed.len() >= 32,
        "master seed for key version {version} must be at least 32 bytes (64 hex chars), got {} bytes",
        seed.len()
    );
    seed
}

/// Parse the key version out of a `PEARL_MASTER_SEED_V<N>` variable name.
/// Returns `None` for names that don't start with the versioned prefix;
/// panics on a malformed or reserved suffix (versions 0 and 1 must not be
/// spelled with the suffixed form — version 1 is always `PEARL_MASTER_SEED`).
fn parse_versioned_seed_name(name: &str) -> Option<u32> {
    let suffix = name.strip_prefix(VERSIONED_SEED_PREFIX)?;
    let version: u32 = suffix
        .parse()
        .unwrap_or_else(|_| panic!("invalid master seed variable name {name}: bad version suffix"));
    assert!(
        version >= 2,
        "invalid master seed variable name {name}: version 1 must be provided as PEARL_MASTER_SEED"
    );
    Some(version)
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

        let mut master_seeds = BTreeMap::new();
        master_seeds.insert(1, decode_seed(1, &master_seed_hex));

        for (name, value) in std::env::vars() {
            if let Some(version) = parse_versioned_seed_name(&name) {
                let previous = master_seeds.insert(version, decode_seed(version, &value));
                assert!(
                    previous.is_none(),
                    "duplicate master seed for version {version}"
                );
            }
        }
        for (version, seed_hex) in &overrides.versioned_master_seeds_hex {
            assert!(
                *version >= 2,
                "versioned master seed files are for versions >= 2 (use --pearl-master-seed-file for version 1)"
            );
            let previous = master_seeds.insert(*version, decode_seed(*version, seed_hex));
            assert!(
                previous.is_none(),
                "duplicate master seed for version {version} (set via both env and file)"
            );
        }

        let active_key_version = std::env::var("PEARL_ACTIVE_KEY_VERSION")
            .ok()
            .map(|v| {
                v.parse::<u32>()
                    .expect("PEARL_ACTIVE_KEY_VERSION must be a positive integer")
            })
            .unwrap_or(1);
        assert!(
            master_seeds.contains_key(&active_key_version),
            "PEARL_ACTIVE_KEY_VERSION is {active_key_version} but no master seed is configured for that version"
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
            master_seeds,
            active_key_version,
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
            versioned_master_seeds_hex: Vec::new(),
        })
    }

    /// Look up the master seed for a key version. Version 0 (the proto3
    /// default sent by clients that predate key versioning) resolves to
    /// version 1. Returns `None` for versions with no configured seed.
    pub fn seed_for_version(&self, version: u32) -> Option<&Zeroizing<Vec<u8>>> {
        let version = if version == 0 { 1 } else { version };
        self.master_seeds.get(&version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_seed_name_parses() {
        assert_eq!(parse_versioned_seed_name("PEARL_MASTER_SEED_V2"), Some(2));
        assert_eq!(parse_versioned_seed_name("PEARL_MASTER_SEED_V10"), Some(10));
    }

    #[test]
    fn unrelated_names_are_ignored() {
        assert_eq!(parse_versioned_seed_name("PEARL_MASTER_SEED"), None);
        assert_eq!(parse_versioned_seed_name("PEARL_SERVICE_SECRET"), None);
        assert_eq!(parse_versioned_seed_name("PATH"), None);
    }

    #[test]
    #[should_panic(expected = "bad version suffix")]
    fn malformed_version_suffix_panics() {
        parse_versioned_seed_name("PEARL_MASTER_SEED_Vtwo");
    }

    #[test]
    #[should_panic(expected = "version 1 must be provided as PEARL_MASTER_SEED")]
    fn suffixed_v1_is_rejected() {
        parse_versioned_seed_name("PEARL_MASTER_SEED_V1");
    }

    #[test]
    #[should_panic(expected = "at least 32 bytes")]
    fn short_seed_is_rejected() {
        decode_seed(1, "abcd");
    }

    fn test_config_with_seeds(versions: &[u32]) -> Config {
        let master_seeds = versions
            .iter()
            .map(|v| (*v, Zeroizing::new(vec![*v as u8; 32])))
            .collect();
        Config {
            bind_addr: "127.0.0.1:0".into(),
            service_secret: "secret".into(),
            master_seeds,
            active_key_version: *versions.last().unwrap(),
            tls_cert_path: None,
            tls_key_path: None,
            metrics_bind_addr: "127.0.0.1:0".into(),
        }
    }

    #[test]
    fn version_zero_resolves_to_version_one() {
        let config = test_config_with_seeds(&[1, 2]);
        assert_eq!(
            config.seed_for_version(0).map(|s| s.as_slice()),
            config.seed_for_version(1).map(|s| s.as_slice()),
        );
    }

    #[test]
    fn unknown_version_is_none() {
        let config = test_config_with_seeds(&[1]);
        assert!(config.seed_for_version(2).is_none());
    }

    #[test]
    fn debug_redacts_seeds_but_lists_versions() {
        let config = test_config_with_seeds(&[1, 2]);
        let debug = format!("{config:?}");
        assert!(debug.contains("[redacted; versions [1, 2]]"));
        assert!(!debug.contains("0101"));
    }
}
