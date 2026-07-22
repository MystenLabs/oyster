use std::{fmt, path::PathBuf, str::FromStr};

use walrus_sui::utils::BYTES_PER_UNIT_SIZE;

/// Secrets that can be provided via file instead of environment variable.
pub struct SecretOverrides {
    /// Pearl service secret, if loaded from a file.
    pub pearl_service_secret: Option<String>,
}

/// Gating mode for self-serve web signup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignupMode {
    /// Anyone who completes the flow gets an app + admin key.
    Open,
    /// New identities land in the `signup_requests` queue for operator
    /// review; existing users can still sign in.
    Waitlist,
    /// No new signups at all; existing users can still sign in.
    Closed,
}

impl FromStr for SignupMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" => Ok(Self::Open),
            "waitlist" => Ok(Self::Waitlist),
            "closed" => Ok(Self::Closed),
            other => Err(format!(
                "invalid OYSTER_SIGNUP_MODE {other:?} (expected open|waitlist|closed)"
            )),
        }
    }
}

/// Configuration for the self-serve web signup feature. Only
/// constructed when every external credential is present — otherwise
/// the signup routes are not mounted at all (see [`Config::new`]).
#[derive(Clone)]
pub struct SignupConfig {
    /// Signup gating mode. Defaults to [`SignupMode::Closed`] so that
    /// merely configuring credentials never silently opens signup.
    pub mode: SignupMode,
    /// Email domains (lowercase, no `@`) whose Google-verified users
    /// skip the waitlist queue.
    pub allowed_domains: Vec<String>,
    /// Public base URL of this server (e.g. `https://oyster.example.com`),
    /// used to build the OAuth redirect URI. No trailing slash.
    pub public_base_url: String,
    /// Google OAuth 2.0 web client ID.
    pub google_client_id: String,
    /// Google OAuth 2.0 web client secret.
    pub google_client_secret: String,
    /// Cloudflare Turnstile sitekey (public, embedded in the page).
    pub turnstile_site_key: String,
    /// Cloudflare Turnstile secret key (server-side siteverify).
    pub turnstile_secret_key: String,
    /// Optional environment label ("Testnet", "Mainnet", …) rendered as
    /// a badge on the signup pages so users can tell deployments apart.
    pub env_label: Option<String>,
    /// **Dev-only.** Override Google's consent-screen URL (mock server).
    pub google_auth_url: Option<String>,
    /// **Dev-only.** Override Google's token endpoint (mock server).
    pub google_token_url: Option<String>,
    /// **Dev-only.** Override Google's JWKS endpoint (mock server).
    pub google_jwks_url: Option<String>,
    /// **Dev-only.** Override Cloudflare's siteverify endpoint (mock server).
    pub turnstile_siteverify_url: Option<String>,
}

impl fmt::Debug for SignupConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignupConfig")
            .field("mode", &self.mode)
            .field("allowed_domains", &self.allowed_domains)
            .field("public_base_url", &self.public_base_url)
            .field("google_client_id", &self.google_client_id)
            .field("google_client_secret", &"[redacted]")
            .field("turnstile_site_key", &self.turnstile_site_key)
            .field("turnstile_secret_key", &"[redacted]")
            .field("env_label", &self.env_label)
            .field("google_auth_url", &self.google_auth_url)
            .field("google_token_url", &self.google_token_url)
            .field("google_jwks_url", &self.google_jwks_url)
            .field("turnstile_siteverify_url", &self.turnstile_siteverify_url)
            .finish()
    }
}

/// The environment variables that must all be set for signup to be
/// enabled. `OYSTER_PUBLIC_BASE_URL` is included because the OAuth
/// redirect URI cannot be built without it.
const SIGNUP_REQUIRED_VARS: [&str; 5] = [
    "OYSTER_PUBLIC_BASE_URL",
    "GOOGLE_OAUTH_CLIENT_ID",
    "GOOGLE_OAUTH_CLIENT_SECRET",
    "TURNSTILE_SITE_KEY",
    "TURNSTILE_SECRET_KEY",
];

/// Build the optional signup config from environment variables.
fn signup_config_from_env() -> Option<SignupConfig> {
    signup_config_from(|var| std::env::var(var).ok())
}

/// Build the optional signup config from raw values via `get` (an
/// injectable lookup so tests avoid process-global env mutation).
///
/// Returns `None` (signup disabled) when none of the required values
/// are present. Panics when only *some* are present — a partially
/// configured signup deployment is a mistake worth failing fast on —
/// or when `OYSTER_SIGNUP_MODE` doesn't parse.
fn signup_config_from(get: impl Fn(&str) -> Option<String>) -> Option<SignupConfig> {
    let values: Vec<Option<String>> = SIGNUP_REQUIRED_VARS
        .iter()
        .map(|var| get(var).filter(|v| !v.is_empty()))
        .collect();

    if values.iter().all(|v| v.is_none()) {
        return None;
    }
    if values.iter().any(|v| v.is_none()) {
        let missing: Vec<&str> = SIGNUP_REQUIRED_VARS
            .iter()
            .zip(&values)
            .filter(|(_, v)| v.is_none())
            .map(|(var, _)| *var)
            .collect();
        panic!(
            "signup is partially configured: missing {} (set all of {} or none)",
            missing.join(", "),
            SIGNUP_REQUIRED_VARS.join(", "),
        );
    }

    let mut values = values.into_iter().map(|v| v.expect("checked above"));
    let public_base_url = values.next().expect("five values");

    Some(SignupConfig {
        mode: get("OYSTER_SIGNUP_MODE")
            .map(|v| v.parse().unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or(SignupMode::Closed),
        allowed_domains: parse_allowed_domains(
            &get("OYSTER_SIGNUP_ALLOWED_DOMAINS").unwrap_or_default(),
        ),
        public_base_url: public_base_url.trim_end_matches('/').to_string(),
        google_client_id: values.next().expect("five values"),
        google_client_secret: values.next().expect("five values"),
        turnstile_site_key: values.next().expect("five values"),
        turnstile_secret_key: values.next().expect("five values"),
        env_label: get("OYSTER_SIGNUP_ENV_LABEL").filter(|v| !v.is_empty()),
        google_auth_url: get("GOOGLE_OAUTH_AUTH_URL"),
        google_token_url: get("GOOGLE_OAUTH_TOKEN_URL"),
        google_jwks_url: get("GOOGLE_OAUTH_JWKS_URL"),
        turnstile_siteverify_url: get("TURNSTILE_SITEVERIFY_URL"),
    })
}

/// Parse the comma-separated allowed-domains list: entries are trimmed,
/// lowercased, and stripped of a leading `@`; empties are dropped.
fn parse_allowed_domains(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|d| d.trim().trim_start_matches('@').to_lowercase())
        .filter(|d| !d.is_empty())
        .collect()
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
    /// Ceiling for the exponential retry backoff applied after failed
    /// extension attempts (`claim_cooldown * 2^failures`, capped here).
    /// Bounds how long a user waits after funding their wallet before the
    /// worker retries, so keep it small relative to the epoch duration.
    pub extension_backoff_cap_secs: u64,
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
    /// Maximum number of *active* admin keys per app, enforced on the
    /// self-serve web issuance path (the `oyster app` CLI is an
    /// operator escape hatch and bypasses it). Revoked keys don't count.
    pub max_admin_keys_per_app: i64,
    /// Self-serve web signup, or `None` when its credentials are not
    /// configured (the signup routes are then not mounted).
    pub signup: Option<SignupConfig>,
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
            extension_backoff_cap_secs: std::env::var("EXTENSION_BACKOFF_CAP_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3600),
            extension_metrics_bind_addr: std::env::var("OYSTER_EXTENSION_METRICS_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:50053".into()),
            default_avg_blob_size: std::env::var("OYSTER_DEFAULT_AVG_BLOB_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000_000),
            allow_http_webhook_scheme: false,
            max_admin_keys_per_app: std::env::var("OYSTER_MAX_ADMIN_KEYS_PER_APP")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            signup: signup_config_from_env(),
        }
    }

    /// Load configuration from environment variables, using sensible defaults.
    pub fn from_env() -> Self {
        Self::new(SecretOverrides {
            pearl_service_secret: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |var| map.get(var).cloned()
    }

    const ALL_SET: [(&str, &str); 5] = [
        ("OYSTER_PUBLIC_BASE_URL", "https://oyster.example.com/"),
        ("GOOGLE_OAUTH_CLIENT_ID", "client-id"),
        ("GOOGLE_OAUTH_CLIENT_SECRET", "client-secret"),
        ("TURNSTILE_SITE_KEY", "site-key"),
        ("TURNSTILE_SECRET_KEY", "secret-key"),
    ];

    #[test]
    fn no_credentials_disables_signup() {
        assert!(signup_config_from(lookup(&[])).is_none());
    }

    #[test]
    fn all_credentials_enable_signup_with_defaults() {
        let cfg = signup_config_from(lookup(&ALL_SET)).unwrap();
        assert_eq!(cfg.mode, SignupMode::Closed);
        assert!(cfg.allowed_domains.is_empty());
        // Trailing slash is stripped so redirect URIs join cleanly.
        assert_eq!(cfg.public_base_url, "https://oyster.example.com");
        assert_eq!(cfg.google_client_id, "client-id");
        assert_eq!(cfg.turnstile_site_key, "site-key");
    }

    #[test]
    #[should_panic(expected = "signup is partially configured: missing TURNSTILE_SECRET_KEY")]
    fn partial_credentials_panic() {
        signup_config_from(lookup(&ALL_SET[..4]));
    }

    #[test]
    #[should_panic(expected = "signup is partially configured")]
    fn empty_string_counts_as_missing() {
        let mut vars = ALL_SET;
        vars[2].1 = "";
        signup_config_from(lookup(&vars));
    }

    #[test]
    fn mode_and_domains_are_parsed() {
        let mut vars = ALL_SET.to_vec();
        vars.push(("OYSTER_SIGNUP_MODE", "waitlist"));
        vars.push((
            "OYSTER_SIGNUP_ALLOWED_DOMAINS",
            "MystenLabs.com, @example.org,,",
        ));
        vars.push(("OYSTER_SIGNUP_ENV_LABEL", "Testnet"));
        let cfg = signup_config_from(lookup(&vars)).unwrap();
        assert_eq!(cfg.mode, SignupMode::Waitlist);
        assert_eq!(cfg.allowed_domains, vec!["mystenlabs.com", "example.org"]);
        assert_eq!(cfg.env_label.as_deref(), Some("Testnet"));
    }

    #[test]
    fn env_label_defaults_to_none() {
        let cfg = signup_config_from(lookup(&ALL_SET)).unwrap();
        assert!(cfg.env_label.is_none());
    }

    #[test]
    #[should_panic(expected = "invalid OYSTER_SIGNUP_MODE")]
    fn invalid_mode_panics() {
        let mut vars = ALL_SET.to_vec();
        vars.push(("OYSTER_SIGNUP_MODE", "on"));
        signup_config_from(lookup(&vars));
    }

    #[test]
    fn signup_config_debug_redacts_secrets() {
        let cfg = signup_config_from(lookup(&ALL_SET)).unwrap();
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("client-secret"));
        assert!(!debug.contains("secret-key"));
        assert!(debug.contains("[redacted]"));
    }
}
