//! Outbound webhook client for notifying apps that an account's Pearl wallet
//! needs funding.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use metrics::counter;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    AccountId, FundingAmount,
    metrics::{
        FUNDING_REQUIRED_WEBHOOKS_TOTAL, WEBHOOK_ATTEMPTS_TOTAL, WEBHOOK_CIRCUIT_OPEN_TOTAL,
        WEBHOOK_FAILURES_TOTAL, WEBHOOK_SUCCESSES_TOTAL,
    },
    webhook_keys,
};

/// Maximum number of retry attempts per webhook call.
const MAX_RETRIES: u32 = 3;
/// Number of consecutive failures before the circuit breaker opens.
const FAILURE_THRESHOLD: u32 = 5;
/// Seconds the circuit breaker stays open before allowing a probe request.
const COOLDOWN_SECS: u64 = 60;

/// Webhook event type emitted when an account's Pearl wallet cannot cover the
/// next storage-pool extension.
pub const EVENT_TYPE_FUNDING_REQUIRED: &str = "account.funding_required";

/// Webhook event: an admin registered or replaced the account's
/// withdrawal address. Emitted immediately so the app operator can spot
/// a hostile registration during the cooldown and revoke the key.
pub const EVENT_TYPE_WITHDRAWAL_ADDRESS_SET: &str = "account.withdrawal_address_set";
/// Webhook event: a withdrawal was requested and awaits a second key.
pub const EVENT_TYPE_WITHDRAWAL_REQUESTED: &str = "account.withdrawal_requested";
/// Webhook event: a withdrawal was approved and landed on-chain.
pub const EVENT_TYPE_WITHDRAWAL_COMPLETED: &str = "account.withdrawal_completed";

/// Payload for the `account.withdrawal_*` events.
#[derive(Debug, Serialize)]
pub struct WithdrawalEventPayload {
    /// Stable id for this delivery.
    pub event_id: Uuid,
    /// One of the `EVENT_TYPE_WITHDRAWAL_*` constants.
    #[serde(rename = "type")]
    pub event_type: &'static str,
    /// The account.
    pub account_id: AccountId,
    /// Withdrawal request id (absent for `withdrawal_address_set`).
    pub withdrawal_id: Option<String>,
    /// Destination address.
    pub destination: String,
    /// SUI in MIST, when a fixed amount was requested.
    pub sui_mist: Option<i64>,
    /// WAL in FROST, when a fixed amount was requested.
    pub wal_frost: Option<i64>,
    /// Whether the request empties the wallet.
    pub all: bool,
    /// For `withdrawal_address_set`: when the address becomes usable.
    pub usable_at: Option<String>,
    /// Admin key id that performed the action.
    pub admin_key_id: String,
    /// Sui transaction digest for `withdrawal_completed`.
    pub tx_digest: Option<String>,
    /// ISO-8601 UTC timestamp when the event was emitted.
    pub timestamp: DateTime<Utc>,
}

impl WithdrawalEventPayload {
    /// Event for a (re)registered withdrawal address.
    pub fn address_set(
        account_id: AccountId,
        destination: String,
        usable_at: String,
        admin_key_id: String,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            event_type: EVENT_TYPE_WITHDRAWAL_ADDRESS_SET,
            account_id,
            withdrawal_id: None,
            destination,
            sui_mist: None,
            wal_frost: None,
            all: false,
            usable_at: Some(usable_at),
            admin_key_id,
            tx_digest: None,
            timestamp: Utc::now(),
        }
    }

    /// Event describing a withdrawal request (requested / completed).
    pub fn from_withdrawal(
        event_type: &'static str,
        w: &crate::db::withdrawals::Withdrawal,
        admin_key_id: &str,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            event_type,
            account_id: w.account_id,
            withdrawal_id: Some(w.id.clone()),
            destination: w.destination.clone(),
            sui_mist: w.sui_mist,
            wal_frost: w.wal_frost,
            all: w.drain,
            usable_at: None,
            admin_key_id: admin_key_id.to_string(),
            tx_digest: w.tx_digest.clone(),
            timestamp: Utc::now(),
        }
    }
}

/// Deliver a withdrawal event to `app_id`'s webhook, if one is
/// configured. Builds a one-off client (these events are rare) and
/// swallows every failure after logging: the ledger and audit log are
/// the source of truth, the webhook is a heads-up.
pub async fn notify_app_withdrawal_event(
    db: &crate::db::DbPool,
    app_id: &crate::AppId,
    payload: &WithdrawalEventPayload,
) {
    let cfg = match crate::db::accounts::fetch_webhook_for_apps(db, &[*app_id]).await {
        Ok(mut map) => map.remove(app_id).flatten(),
        Err(e) => {
            tracing::warn!(%app_id, error = %e, "could not load webhook config for withdrawal event");
            return;
        }
    };
    let Some(cfg) = cfg else {
        tracing::debug!(%app_id, event = payload.event_type, "no webhook configured; withdrawal event not delivered");
        return;
    };
    let (private_bytes, public_bytes) = match (
        webhook_keys::decode_key(&cfg.private_key_b64),
        webhook_keys::decode_key(&cfg.public_key_b64),
    ) {
        (Ok(p), Ok(q)) => (p, q),
        _ => {
            tracing::warn!(%app_id, "skipping withdrawal event: invalid webhook keys");
            return;
        }
    };
    let client = WebhookClient::new(
        cfg.url,
        SigningKey::from_bytes(&private_bytes),
        public_bytes,
    );
    let body = serde_json::to_vec(payload).expect("serialize WithdrawalEventPayload");
    client
        .deliver(&payload.account_id, payload.event_type, body)
        .await;
}

/// Payload posted when a blob extension cannot be performed because Pearl's
/// wallet for the account is short on either WAL or SUI.
///
/// `event_id` is generated once per delivery and is stable across the
/// internal retry loop, so receivers can dedupe.
#[derive(Debug, Serialize)]
pub struct FundingRequiredPayload {
    /// Stable id for this delivery — same across all retry attempts.
    pub event_id: Uuid,
    /// Event type discriminator (always `account.funding_required`).
    #[serde(rename = "type")]
    pub event_type: &'static str,
    /// Oyster account whose pool needs extension funding.
    pub account_id: AccountId,
    /// Sui wallet address derived by Pearl for this account.
    pub pearl_address: String,
    /// Token amounts the wallet needs to perform the next extension.
    pub amount: FundingAmount,
    /// ISO-8601 UTC timestamp when the event was emitted.
    pub timestamp: DateTime<Utc>,
}

/// HTTP client with retry logic and a circuit breaker for outbound webhooks.
pub struct WebhookClient {
    client: reqwest::Client,
    url: String,
    signing_key: SigningKey,
    public_key: [u8; webhook_keys::KEY_LEN],
    circuit: Mutex<CircuitState>,
}

struct CircuitState {
    consecutive_failures: u32,
    opened_at: Option<Instant>,
}

impl WebhookClient {
    /// Create a new webhook client targeting the given URL with the per-app
    /// keypair used to sign every delivery.
    pub fn new(
        url: String,
        signing_key: SigningKey,
        public_key: [u8; webhook_keys::KEY_LEN],
    ) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("failed to build reqwest client"),
            url,
            signing_key,
            public_key,
            circuit: Mutex::new(CircuitState {
                consecutive_failures: 0,
                opened_at: None,
            }),
        }
    }

    /// Send the webhook if the circuit is closed or half-open.
    ///
    /// Returns `Ok(())` even if skipped due to open circuit (fire-and-forget).
    pub async fn notify_funding_required(&self, payload: &FundingRequiredPayload) {
        // Serialize once; sign exactly the bytes that go on the wire.
        let body_bytes = serde_json::to_vec(payload).expect("serialize FundingRequiredPayload");
        let delivered = self
            .deliver(&payload.account_id, payload.event_type, body_bytes)
            .await;
        let outcome = if delivered { "success" } else { "failure" };
        counter!(FUNDING_REQUIRED_WEBHOOKS_TOTAL, "outcome" => outcome).increment(1);
    }

    /// Sign and POST `body_bytes`, retrying transient failures, subject
    /// to the circuit breaker. Returns whether the receiver accepted it
    /// (an open circuit counts as not delivered).
    pub async fn deliver(&self, account_id: &AccountId, event: &str, body_bytes: Vec<u8>) -> bool {
        if !self.should_attempt() {
            tracing::warn!(%account_id, event, "webhook circuit open, skipping notification");
            return false;
        }

        counter!(WEBHOOK_ATTEMPTS_TOTAL).increment(1);

        let signature = webhook_keys::sign(&self.signing_key, &body_bytes);
        let sig_header = format!(
            "ed25519={}",
            base64::engine::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                signature.to_bytes(),
            ),
        );
        let fp_header = webhook_keys::fingerprint(&self.public_key);

        let mut last_err = None;
        let mut delay = Duration::from_millis(100);

        for attempt in 1..=MAX_RETRIES {
            match self
                .client
                .post(&self.url)
                .header("content-type", "application/json")
                .header("X-Oyster-Signature", &sig_header)
                .header("X-Oyster-Public-Key-Fingerprint", &fp_header)
                .body(body_bytes.clone())
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    self.record_success();
                    counter!(WEBHOOK_SUCCESSES_TOTAL).increment(1);
                    tracing::info!(%account_id, event, "webhook delivered successfully");
                    return true;
                }
                Ok(resp) if resp.status().is_client_error() => {
                    // 4xx — not retryable.
                    tracing::warn!(
                        %account_id,
                        event,
                        status = %resp.status(),
                        "webhook returned client error, not retrying"
                    );
                    self.record_failure();
                    counter!(WEBHOOK_FAILURES_TOTAL).increment(1);
                    return false;
                }
                Ok(resp) => {
                    last_err = Some(format!("HTTP {}", resp.status()));
                }
                Err(e) => {
                    last_err = Some(e.to_string());
                }
            }

            if attempt < MAX_RETRIES {
                tracing::debug!(
                    attempt,
                    error = last_err.as_deref().unwrap_or("unknown"),
                    "webhook attempt failed, retrying"
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        }

        tracing::warn!(
            %account_id,
            event,
            error = last_err.as_deref().unwrap_or("unknown"),
            "webhook failed after {MAX_RETRIES} attempts"
        );
        self.record_failure();
        counter!(WEBHOOK_FAILURES_TOTAL).increment(1);
        false
    }

    /// Check whether a request should be attempted based on circuit state.
    fn should_attempt(&self) -> bool {
        let mut state = self.circuit.lock().expect("circuit lock poisoned");
        match state.opened_at {
            None => true,
            Some(opened) => {
                if opened.elapsed() >= Duration::from_secs(COOLDOWN_SECS) {
                    // Half-open: allow one probe.
                    state.opened_at = Some(Instant::now());
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful webhook delivery, resetting the circuit breaker.
    fn record_success(&self) {
        let mut state = self.circuit.lock().expect("circuit lock poisoned");
        state.consecutive_failures = 0;
        state.opened_at = None;
    }

    /// Record a failed webhook delivery, potentially opening the circuit.
    fn record_failure(&self) {
        let mut state = self.circuit.lock().expect("circuit lock poisoned");
        state.consecutive_failures += 1;
        if state.consecutive_failures >= FAILURE_THRESHOLD && state.opened_at.is_none() {
            tracing::warn!(
                consecutive_failures = state.consecutive_failures,
                "webhook circuit breaker opened"
            );
            state.opened_at = Some(Instant::now());
            counter!(WEBHOOK_CIRCUIT_OPEN_TOTAL).increment(1);
        }
    }
}

/// Check whether an error indicates insufficient funds on-chain.
pub fn is_insufficient_funds_error(error: &(dyn std::error::Error + 'static)) -> bool {
    let msg = error.to_string().to_lowercase();
    msg.contains("insufficientgas")
        || msg.contains("insufficientcoinbalance")
        || msg.contains("insufficient")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FakeError(String);
    impl std::fmt::Display for FakeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }
    impl std::error::Error for FakeError {}

    fn test_client(url: &str) -> WebhookClient {
        let (sk, pk) = webhook_keys::generate_keypair();
        WebhookClient::new(url.to_string(), sk, pk)
    }

    #[test]
    fn test_is_insufficient_funds_error() {
        assert!(is_insufficient_funds_error(
            &FakeError("InsufficientGas".into()) as &dyn std::error::Error
        ));
        assert!(is_insufficient_funds_error(
            &FakeError("InsufficientCoinBalance".into()) as &dyn std::error::Error
        ));
        assert!(is_insufficient_funds_error(&FakeError(
            "transaction failed: insufficient balance".into()
        )
            as &dyn std::error::Error));
        assert!(!is_insufficient_funds_error(
            &FakeError("object not found".into()) as &dyn std::error::Error
        ));
        assert!(!is_insufficient_funds_error(
            &FakeError("network timeout".into()) as &dyn std::error::Error
        ));
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let client = test_client("http://localhost:9999/webhook");

        // Circuit should be closed initially.
        assert!(client.should_attempt());

        // Record failures up to threshold.
        for _ in 0..FAILURE_THRESHOLD {
            client.record_failure();
        }

        // Circuit should now be open.
        assert!(!client.should_attempt());
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let client = test_client("http://localhost:9999/webhook");

        for _ in 0..FAILURE_THRESHOLD {
            client.record_failure();
        }
        assert!(!client.should_attempt());

        // A success resets everything.
        client.record_success();
        assert!(client.should_attempt());

        let state = client.circuit.lock().unwrap();
        assert_eq!(state.consecutive_failures, 0);
        assert!(state.opened_at.is_none());
    }

    #[test]
    fn test_funding_required_payload_json_shape() {
        let event_id = Uuid::nil();
        let timestamp = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let account_id: AccountId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let payload = FundingRequiredPayload {
            event_id,
            event_type: EVENT_TYPE_FUNDING_REQUIRED,
            account_id,
            pearl_address: "0xabc".into(),
            amount: FundingAmount {
                wal_frost: 12345,
                sui_mist: 67890,
            },
            timestamp,
        };

        let json: serde_json::Value = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["event_id"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(json["type"], "account.funding_required");
        assert_eq!(json["account_id"], "00000000-0000-0000-0000-000000000001");
        assert_eq!(json["pearl_address"], "0xabc");
        assert_eq!(json["amount"]["wal_frost"], "12345");
        assert_eq!(json["amount"]["sui_mist"], "67890");
        assert!(
            json["timestamp"]
                .as_str()
                .unwrap()
                .starts_with("2023-11-14")
        );
    }

    #[test]
    fn test_circuit_breaker_half_open_after_cooldown() {
        let client = test_client("http://localhost:9999/webhook");

        for _ in 0..FAILURE_THRESHOLD {
            client.record_failure();
        }

        // Manually set opened_at to the past to simulate cooldown expiry.
        {
            let mut state = client.circuit.lock().unwrap();
            state.opened_at = Some(Instant::now() - Duration::from_secs(COOLDOWN_SECS + 1));
        }

        // Half-open: should allow one probe.
        assert!(client.should_attempt());

        // But a second immediate attempt should be blocked (circuit re-armed during half-open).
        assert!(!client.should_attempt());
    }

    fn sample_payload() -> FundingRequiredPayload {
        FundingRequiredPayload {
            event_id: Uuid::nil(),
            event_type: EVENT_TYPE_FUNDING_REQUIRED,
            account_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
            pearl_address: "0xabc".into(),
            amount: FundingAmount {
                wal_frost: 1,
                sui_mist: 2,
            },
            timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    /// Drive one `notify_funding_required` against a local axum handler
    /// under a thread-local Prometheus recorder, and return the rendered
    /// metric text. The handler is built from `make_handler`, which is
    /// called fresh for each request — useful for stub handlers that need
    /// to return different statuses per attempt.
    fn drive_and_render<F, Fut>(make_handler: F) -> String
    where
        F: Fn() -> Fut + Clone + Send + Sync + 'static,
        Fut: std::future::Future<Output = axum::http::StatusCode> + Send + 'static,
    {
        use axum::{Router, routing::post};

        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        metrics::with_local_recorder(&recorder, || {
            rt.block_on(async move {
                let handler = make_handler.clone();
                let app: Router = Router::new().route("/hook", post(move || (handler.clone())()));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tokio::spawn(async move {
                    axum::serve(listener, app).await.unwrap();
                });

                let url = format!("http://{addr}/hook");
                let client = test_client(&url);
                client.notify_funding_required(&sample_payload()).await;
            });
        });

        handle.render()
    }

    #[test]
    fn notify_funding_required_increments_success_counter() {
        let rendered = drive_and_render(|| async { axum::http::StatusCode::OK });
        assert!(
            rendered.contains(r#"oyster_funding_required_webhooks_total{outcome="success"} 1"#,),
            "expected success counter == 1, got:\n{rendered}",
        );
        assert!(
            !rendered.contains(r#"outcome="failure""#),
            "unexpected failure counter present, got:\n{rendered}",
        );
    }

    #[test]
    fn notify_funding_required_increments_failure_counter_on_4xx() {
        let rendered = drive_and_render(|| async { axum::http::StatusCode::BAD_REQUEST });
        assert!(
            rendered.contains(r#"oyster_funding_required_webhooks_total{outcome="failure"} 1"#,),
            "expected failure counter == 1 on 4xx, got:\n{rendered}",
        );
        assert!(
            !rendered.contains(r#"outcome="success""#),
            "unexpected success counter present, got:\n{rendered}",
        );
    }

    #[test]
    fn notify_funding_required_increments_failure_counter_on_retries_exhausted() {
        let rendered = drive_and_render(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR });
        // Exactly one terminal failure increment, regardless of how many 5xx
        // retry attempts the loop made.
        assert!(
            rendered.contains(r#"oyster_funding_required_webhooks_total{outcome="failure"} 1"#,),
            "expected failure counter == 1 after retries exhausted, got:\n{rendered}",
        );
        assert!(
            !rendered.contains(r#"outcome="success""#),
            "unexpected success counter present, got:\n{rendered}",
        );
    }
}
