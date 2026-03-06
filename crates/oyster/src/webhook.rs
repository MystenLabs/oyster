//! Outbound webhook client for notifying a fund manager of insufficient funds.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use metrics::counter;
use serde::Serialize;

use crate::metrics::{
    WEBHOOK_ATTEMPTS_TOTAL,
    WEBHOOK_CIRCUIT_OPEN_TOTAL,
    WEBHOOK_FAILURES_TOTAL,
    WEBHOOK_SUCCESSES_TOTAL,
};

/// Maximum number of retry attempts per webhook call.
const MAX_RETRIES: u32 = 3;
/// Number of consecutive failures before the circuit breaker opens.
const FAILURE_THRESHOLD: u32 = 5;
/// Seconds the circuit breaker stays open before allowing a probe request.
const COOLDOWN_SECS: u64 = 60;

/// Payload sent when a blob extension fails due to insufficient funds.
#[derive(Debug, Serialize)]
pub struct InsufficientFundsPayload {
    /// Oyster account ID that owns the blob.
    pub account_id: String,
    /// Sui wallet address that lacks funds.
    pub address: String,
    /// The error message from the failed transaction.
    pub error: String,
}

/// HTTP client with retry logic and a circuit breaker for outbound webhooks.
pub struct WebhookClient {
    client: reqwest::Client,
    url: String,
    circuit: Mutex<CircuitState>,
}

struct CircuitState {
    consecutive_failures: u32,
    opened_at: Option<Instant>,
}

impl WebhookClient {
    /// Create a new webhook client targeting the given URL.
    pub fn new(url: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("failed to build reqwest client"),
            url,
            circuit: Mutex::new(CircuitState {
                consecutive_failures: 0,
                opened_at: None,
            }),
        }
    }

    /// Send the webhook if the circuit is closed or half-open.
    ///
    /// Returns `Ok(())` even if skipped due to open circuit (fire-and-forget).
    pub async fn notify_insufficient_funds(&self, payload: &InsufficientFundsPayload) {
        if !self.should_attempt() {
            tracing::warn!(
                account_id = %payload.account_id,
                "webhook circuit open, skipping notification"
            );
            return;
        }

        counter!(WEBHOOK_ATTEMPTS_TOTAL).increment(1);

        let mut last_err = None;
        let mut delay = Duration::from_millis(100);

        for attempt in 1..=MAX_RETRIES {
            match self.client.post(&self.url).json(payload).send().await {
                Ok(resp) if resp.status().is_success() => {
                    self.record_success();
                    counter!(WEBHOOK_SUCCESSES_TOTAL).increment(1);
                    tracing::info!(
                        account_id = %payload.account_id,
                        "webhook delivered successfully"
                    );
                    return;
                }
                Ok(resp) if resp.status().is_client_error() => {
                    // 4xx — not retryable.
                    tracing::warn!(
                        account_id = %payload.account_id,
                        status = %resp.status(),
                        "webhook returned client error, not retrying"
                    );
                    self.record_failure();
                    counter!(WEBHOOK_FAILURES_TOTAL).increment(1);
                    return;
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
            account_id = %payload.account_id,
            error = last_err.as_deref().unwrap_or("unknown"),
            "webhook failed after {MAX_RETRIES} attempts"
        );
        self.record_failure();
        counter!(WEBHOOK_FAILURES_TOTAL).increment(1);
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
        let client = WebhookClient::new("http://localhost:9999/webhook".into());

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
        let client = WebhookClient::new("http://localhost:9999/webhook".into());

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
    fn test_circuit_breaker_half_open_after_cooldown() {
        let client = WebhookClient::new("http://localhost:9999/webhook".into());

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
}
