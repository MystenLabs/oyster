//! Server-side verification of Cloudflare Turnstile tokens via the
//! `siteverify` API.
//!
//! The widget on the signup page produces a single-use token in the
//! browser; nothing about that token can be trusted until this module
//! posts it (with our secret key) to Cloudflare and reads the verdict.
//! For local development Cloudflare publishes dummy sitekey/secret
//! pairs that always pass or always fail — see `.env.example`.

use std::time::Duration;

use serde::Deserialize;

/// Production `siteverify` endpoint.
const SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";

/// Timeout for the `siteverify` HTTP call.
const SITEVERIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// Verdict from a Turnstile verification.
#[derive(Debug, Clone)]
pub struct TurnstileVerdict {
    /// Whether Cloudflare judged the token valid.
    pub success: bool,
    /// Cloudflare error codes on failure (e.g. `invalid-input-response`,
    /// `timeout-or-duplicate`).
    pub error_codes: Vec<String>,
}

/// Error talking to the `siteverify` API (network failure, non-2xx
/// response, or unparsable body). Distinct from a *failed verification*,
/// which is a successful API call with `success: false`.
#[derive(Debug)]
pub struct TurnstileApiError(pub String);

impl std::fmt::Display for TurnstileApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "turnstile siteverify error: {}", self.0)
    }
}

impl std::error::Error for TurnstileApiError {}

/// Shape of the `siteverify` JSON response (fields we consume).
#[derive(Deserialize)]
struct SiteverifyResponse {
    success: bool,
    #[serde(rename = "error-codes", default)]
    error_codes: Vec<String>,
}

/// Client for Cloudflare Turnstile `siteverify`.
#[derive(Debug, Clone)]
pub struct TurnstileVerifier {
    client: reqwest::Client,
    endpoint: String,
    secret_key: String,
}

impl TurnstileVerifier {
    /// Verifier against the production Cloudflare endpoint.
    pub fn new(secret_key: String) -> Self {
        Self::with_endpoint(secret_key, SITEVERIFY_URL.to_string())
    }

    /// Verifier against a custom endpoint — used by tests and the local
    /// mock testbed.
    pub fn with_endpoint(secret_key: String, endpoint: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(SITEVERIFY_TIMEOUT)
                .build()
                .expect("failed to build reqwest client"),
            endpoint,
            secret_key,
        }
    }

    /// Verify a widget token. `remote_ip` (the visitor's IP) is optional
    /// but improves Cloudflare's signal when available.
    pub async fn verify(
        &self,
        token: &str,
        remote_ip: Option<&str>,
    ) -> Result<TurnstileVerdict, TurnstileApiError> {
        let mut form: Vec<(&str, &str)> =
            vec![("secret", self.secret_key.as_str()), ("response", token)];
        if let Some(ip) = remote_ip {
            form.push(("remoteip", ip));
        }

        let response = self
            .client
            .post(&self.endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|e| TurnstileApiError(format!("request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            return Err(TurnstileApiError(format!("unexpected status {status}")));
        }

        let body: SiteverifyResponse = response
            .json()
            .await
            .map_err(|e| TurnstileApiError(format!("invalid response body: {e}")))?;

        Ok(TurnstileVerdict {
            success: body.success,
            error_codes: body.error_codes,
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::{Json, Router, extract::Form, routing::post};

    use super::*;

    /// Boot an in-process mock siteverify endpoint; returns its URL and
    /// a handle to the received form payloads.
    async fn mock_siteverify(
        response: serde_json::Value,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>,
    ) {
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_clone = received.clone();
        let app = Router::new().route(
            "/siteverify",
            post(move |Form(form): Form<Vec<(String, String)>>| {
                let response = response.clone();
                let received = received_clone.clone();
                async move {
                    received.lock().unwrap().push(form);
                    Json(response)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/siteverify"), received)
    }

    #[tokio::test]
    async fn success_verdict() {
        let (url, received) = mock_siteverify(serde_json::json!({
            "success": true,
            "challenge_ts": "2026-07-12T00:00:00Z",
            "hostname": "localhost",
        }))
        .await;

        let verifier = TurnstileVerifier::with_endpoint("test-secret".into(), url);
        let verdict = verifier
            .verify("widget-token", Some("1.2.3.4"))
            .await
            .unwrap();
        assert!(verdict.success);
        assert!(verdict.error_codes.is_empty());

        // The secret, token, and remote IP all reach Cloudflare.
        let forms = received.lock().unwrap();
        let form = &forms[0];
        assert!(form.contains(&("secret".into(), "test-secret".into())));
        assert!(form.contains(&("response".into(), "widget-token".into())));
        assert!(form.contains(&("remoteip".into(), "1.2.3.4".into())));
    }

    #[tokio::test]
    async fn failure_verdict_with_error_codes() {
        let (url, _) = mock_siteverify(serde_json::json!({
            "success": false,
            "error-codes": ["invalid-input-response"],
        }))
        .await;

        let verifier = TurnstileVerifier::with_endpoint("test-secret".into(), url);
        let verdict = verifier.verify("bad-token", None).await.unwrap();
        assert!(!verdict.success);
        assert_eq!(verdict.error_codes, vec!["invalid-input-response"]);
    }

    #[tokio::test]
    async fn missing_error_codes_field_defaults_empty() {
        let (url, _) = mock_siteverify(serde_json::json!({ "success": false })).await;
        let verifier = TurnstileVerifier::with_endpoint("s".into(), url);
        let verdict = verifier.verify("t", None).await.unwrap();
        assert!(!verdict.success);
        assert!(verdict.error_codes.is_empty());
    }

    #[tokio::test]
    async fn http_error_is_api_error() {
        let app = Router::new().route(
            "/siteverify",
            post(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let verifier =
            TurnstileVerifier::with_endpoint("s".into(), format!("http://{addr}/siteverify"));
        let err = verifier.verify("t", None).await.unwrap_err();
        assert!(err.0.contains("unexpected status"), "{err}");
    }

    #[tokio::test]
    async fn unreachable_endpoint_is_api_error() {
        // Port 1 is essentially guaranteed closed.
        let verifier =
            TurnstileVerifier::with_endpoint("s".into(), "http://127.0.0.1:1/siteverify".into());
        let err = verifier.verify("t", None).await.unwrap_err();
        assert!(err.0.contains("request failed"), "{err}");
    }

    #[tokio::test]
    async fn garbage_body_is_api_error() {
        let app = Router::new().route("/siteverify", post(|| async { "not json" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let verifier =
            TurnstileVerifier::with_endpoint("s".into(), format!("http://{addr}/siteverify"));
        let err = verifier.verify("t", None).await.unwrap_err();
        assert!(err.0.contains("invalid response body"), "{err}");
    }
}
