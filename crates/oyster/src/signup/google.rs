//! Google OAuth 2.0 / OpenID Connect client for the signup flow.
//!
//! Implements the server-side authorization-code flow with `state`
//! (CSRF), PKCE (S256), and `nonce` binding, then verifies the returned
//! id_token against Google's published JWKS before trusting any claim
//! in it. Only the non-sensitive `openid email profile` scopes are
//! requested.

use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::auth;

/// Google's OAuth consent-screen endpoint.
/// (public so the router builder can default to it)
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Google's code→token exchange endpoint.
/// (public so the router builder can default to it)
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Google's JWKS endpoint for id_token signature verification.
/// (public so the router builder can default to it)
pub const JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";

/// Issuer values Google may put in an id_token (per OIDC discovery).
const GOOGLE_ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];

/// Timeout for calls to Google.
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Error from the Google OAuth flow.
#[derive(Debug)]
pub enum GoogleAuthError {
    /// Network/HTTP-level failure talking to Google.
    Http(String),
    /// The code→token exchange was rejected (e.g. bad/expired code).
    TokenExchange(String),
    /// The id_token failed verification (signature, aud, iss, exp, …).
    InvalidIdToken(String),
    /// The id_token's `nonce` claim doesn't match this login attempt.
    NonceMismatch,
}

impl std::fmt::Display for GoogleAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "google request failed: {e}"),
            Self::TokenExchange(e) => write!(f, "token exchange rejected: {e}"),
            Self::InvalidIdToken(e) => write!(f, "invalid id_token: {e}"),
            Self::NonceMismatch => write!(f, "id_token nonce mismatch"),
        }
    }
}

impl std::error::Error for GoogleAuthError {}

/// Secrets minted at the start of one login attempt. `state` and the
/// PKCE `verifier` must round-trip through the browser (signed cookie)
/// to the callback; `nonce` is checked against the id_token claim.
#[derive(Debug, Clone)]
pub struct AuthRequest {
    /// Full Google consent-screen URL to redirect the browser to.
    pub url: String,
    /// Random CSRF token, echoed back by Google in `?state=`.
    pub state: String,
    /// Random nonce bound into the id_token by Google.
    pub nonce: String,
    /// PKCE code verifier; its S256 digest went into the auth URL.
    pub pkce_verifier: String,
}

/// The Google-attested identity extracted from a verified id_token.
#[derive(Debug, Clone)]
pub struct GoogleIdentity {
    /// Stable unique Google account ID (`sub` claim) — the value stored
    /// as `provider_subject`.
    pub sub: String,
    /// Email address (`email` claim).
    pub email: String,
    /// Whether Google has verified the email (`email_verified` claim).
    /// Callers must check this before trusting `email` for gating.
    pub email_verified: bool,
    /// Display name (`name` claim), if present.
    pub name: Option<String>,
}

/// Claims we consume from the id_token. `aud`/`iss`/`exp` are enforced
/// by the JWT library, not read from here.
#[derive(Deserialize)]
struct IdTokenClaims {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<JwksKey>,
}

#[derive(Deserialize)]
struct JwksKey {
    #[serde(default)]
    kid: Option<String>,
    n: String,
    e: String,
}

/// Client for Google's OAuth endpoints. Endpoint URLs are injectable
/// so tests and the local testbed can run against a mock Google.
#[derive(Debug, Clone)]
pub struct GoogleOAuthClient {
    client: reqwest::Client,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    auth_url: String,
    token_url: String,
    jwks_url: String,
}

impl GoogleOAuthClient {
    /// Client against the real Google endpoints. `redirect_uri` must be
    /// registered on the OAuth client in the Google Cloud console.
    pub fn new(client_id: String, client_secret: String, redirect_uri: String) -> Self {
        Self::with_endpoints(
            client_id,
            client_secret,
            redirect_uri,
            AUTH_URL.into(),
            TOKEN_URL.into(),
            JWKS_URL.into(),
        )
    }

    /// Client against custom endpoints (tests, local mock testbed).
    pub fn with_endpoints(
        client_id: String,
        client_secret: String,
        redirect_uri: String,
        auth_url: String,
        token_url: String,
        jwks_url: String,
    ) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .expect("failed to build reqwest client"),
            client_id,
            client_secret,
            redirect_uri,
            auth_url,
            token_url,
            jwks_url,
        }
    }

    /// Mint the secrets for one login attempt and build the consent URL.
    pub fn begin_auth(&self) -> AuthRequest {
        let state = auth::generate_api_key();
        let nonce = auth::generate_api_key();
        // 64 hex chars — within PKCE's 43–128 unreserved-character rule.
        let pkce_verifier = auth::generate_api_key();
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(pkce_verifier.as_bytes()));

        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", "openid email profile")
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            // Always show Google's account chooser. Without this, Google
            // silently reuses the browser's active session, so signing out
            // of Oyster and back in never offers a chance to switch
            // accounts.
            .append_pair("prompt", "select_account")
            .finish();

        AuthRequest {
            url: format!("{}?{}", self.auth_url, query),
            state,
            nonce,
            pkce_verifier,
        }
    }

    /// Exchange an authorization code for the id_token.
    pub async fn exchange_code(
        &self,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<String, GoogleAuthError> {
        let form: [(&str, &str); 6] = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &self.client_id),
            ("client_secret", &self.client_secret),
            ("redirect_uri", &self.redirect_uri),
            ("code_verifier", pkce_verifier),
        ];

        let response = self
            .client
            .post(&self.token_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| GoogleAuthError::Http(format!("token request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GoogleAuthError::TokenExchange(format!(
                "status {status}: {body}"
            )));
        }

        let body: TokenResponse = response
            .json()
            .await
            .map_err(|e| GoogleAuthError::TokenExchange(format!("invalid response: {e}")))?;
        Ok(body.id_token)
    }

    /// Verify an id_token's signature (against Google's current JWKS),
    /// `aud`, `iss`, `exp`, and `nonce`, and extract the identity.
    pub async fn verify_id_token(
        &self,
        id_token: &str,
        expected_nonce: &str,
    ) -> Result<GoogleIdentity, GoogleAuthError> {
        let header = jsonwebtoken::decode_header(id_token)
            .map_err(|e| GoogleAuthError::InvalidIdToken(format!("bad header: {e}")))?;

        let jwks: Jwks = self
            .client
            .get(&self.jwks_url)
            .send()
            .await
            .map_err(|e| GoogleAuthError::Http(format!("jwks fetch failed: {e}")))?
            .json()
            .await
            .map_err(|e| GoogleAuthError::Http(format!("invalid jwks: {e}")))?;

        let key = jwks
            .keys
            .iter()
            .find(|k| k.kid == header.kid)
            .ok_or_else(|| {
                GoogleAuthError::InvalidIdToken(format!("no jwks key for kid {:?}", header.kid))
            })?;
        let decoding_key = jsonwebtoken::DecodingKey::from_rsa_components(&key.n, &key.e)
            .map_err(|e| GoogleAuthError::InvalidIdToken(format!("bad jwks key: {e}")))?;

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.set_audience(&[&self.client_id]);
        validation.set_issuer(&GOOGLE_ISSUERS);

        let token = jsonwebtoken::decode::<IdTokenClaims>(id_token, &decoding_key, &validation)
            .map_err(|e| GoogleAuthError::InvalidIdToken(e.to_string()))?;

        if token.claims.nonce.as_deref() != Some(expected_nonce) {
            return Err(GoogleAuthError::NonceMismatch);
        }
        let email = token.claims.email.ok_or_else(|| {
            GoogleAuthError::InvalidIdToken("id_token carries no email claim".into())
        })?;

        Ok(GoogleIdentity {
            sub: token.claims.sub,
            email,
            email_verified: token.claims.email_verified.unwrap_or(false),
            name: token.claims.name,
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::{Json, Router, routing::get, routing::post};
    use serde::Serialize;

    use super::*;

    /// 2048-bit RSA test key (generated for these tests only — not a
    /// secret guarding anything).
    const TEST_RSA_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQC/ZbJm2CU1urMt
EGQflQ2ivkT7lQbNBTuq5879vm8az3KARoFGlOAap5nhLrM6dpFFqRKM5f6I5VdZ
Kr2w82ptcaAbk1X/ftmB4HnlDSDUIr/V8O4zjE0nyRaZ3F+R8v8sIQV00oJO4lpg
05CPCSH56yeap1fOG3FHYHZ41mHjNQElxKpH53A1y8/DDLjV0TKn78pw6EaTsLo8
m84t0Ji/fP2xbNavKWD8YaEARhwVFHTZ8e4dNloK6O0DwqtXEaASVSMxBXTvSoay
2cV0PzMILRzF59B/RMu2o4Cpgtd9UMd7wdCGU/AubWxyHn+lH2mjmfdJSYF7rJa2
/bSbWiJjAgMBAAECggEAGUrinvWcQTPpXqCmYogLc8CqFAb3DZvN7UFR2LVUMFpO
msX2anHdBZi1XI0T0Tt+6hI0Kdtw3f1UpCtiQuJ/43ObAVngiPFl4+Rap1qrRm1L
JYX/rF8ziPjpXC7DkGFouBerBWuWHb+KyvXaShSeYUzigGzxzQJSX2jPbxuBdwk9
CMXJ5AQa+oyNxv0ac5so0lAWBN1SUeLIqvLAZeKtmxWZEjDSv4KU5pfv5cJvn3em
Z0I6fbz9PcuGjCZabPOU7MrSqOt3ls7MavHMh/RiUEQGWElN6Yb12ls41YbX2FTN
tSRLADGyji/IrTTWZHiODoNQRin/GFQr0LE9wpiWfQKBgQDonfII7nQT29PBOSn8
vzzJkJuOMExb1FvlWzCVh7vKo6HJ3BTqVljWERvv547+NPT1VeW4x1/UgKIo9/xl
sBqtps25d7cQ3fIj+KjukqX3L0LoWtyMBnA6SLA2/9b9cYi6uhMd5Rkh2kTIsqFQ
+SATRmXe635x0sLq8TA7qgee1wKBgQDSowXSjpV8wkTImX8iwcUvkbVE9YhboQPa
C+2CYYBBDtRLtXs/pogUhzBGtMPJUsqA7C6QWZe1VFZVYoUPxWGSWvusAPv2xzaf
PkGV5ugxrKeAICaPToyb6euvcYwfmCe0BUUqicxGkL7lBAHqkoxZoN53thCMQhyc
zgiE2F0jVQKBgQDSB1IyuEJ8b75pNxjvCSh0ginBn2BChaIXm1dpm612UHpTDXCh
CSea2MXVvcjBQ8VtAoqxZOrkruQ7g3UTx4a/Bd24ORxEkXEBA5JcHnLVlYmey/NY
RrPsHBdnAWb3XRxsJHgARQuFIlN6traqqtVIMgbm2NBJK1gs02qOZH4O7wKBgGfI
oV7MmEU/Zyq7ztOuS90TWxBeNlCHdmFiTSVXqxzjFKE1C0QiZpxOu++qs2kn3NVH
Ce5f5osWwe8SOuO5akj1gVmPppZCM9ykjSYx/qgzHNjZfoZPuqI70L/CH7uVecKO
cjTybm86dIRcxCDzEio7REIRt/eTv4tXTQU/oix9AoGBALv8QAz+hKGAPlxC9wLB
Z1ybwwaaocxmrKFv/zvUPV963zNgyEk1VCEJ+pzCKsY0fNbM5hrGDGJGRG1fP82I
BEJ2LNDNW9d2seB+ont52+6q+udQ6Sw9fMyaWKF7XyOY6ePZfS9u5akL9MRXDh1h
kECkZZ7S0dzVVObk3uG0Tlt0
-----END PRIVATE KEY-----";

    /// base64url modulus of `TEST_RSA_PEM`'s public key.
    const TEST_RSA_N: &str = "v2WyZtglNbqzLRBkH5UNor5E-5UGzQU7qufO_b5vGs9ygEaBRpTgGqeZ4S6zOnaRRakSjOX-iOVXWSq9sPNqbXGgG5NV_37ZgeB55Q0g1CK_1fDuM4xNJ8kWmdxfkfL_LCEFdNKCTuJaYNOQjwkh-esnmqdXzhtxR2B2eNZh4zUBJcSqR-dwNcvPwwy41dEyp-_KcOhGk7C6PJvOLdCYv3z9sWzWrylg_GGhAEYcFRR02fHuHTZaCujtA8KrVxGgElUjMQV070qGstnFdD8zCC0cxefQf0TLtqOAqYLXfVDHe8HQhlPwLm1sch5_pR9po5n3SUmBe6yWtv20m1oiYw";

    #[derive(Serialize)]
    struct TestClaims {
        iss: String,
        aud: String,
        sub: String,
        exp: i64,
        email: String,
        email_verified: bool,
        name: String,
        nonce: String,
    }

    impl TestClaims {
        fn valid(nonce: &str) -> Self {
            Self {
                iss: "https://accounts.google.com".into(),
                aud: "test-client-id".into(),
                sub: "google-sub-1".into(),
                exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
                email: "alice@example.com".into(),
                email_verified: true,
                name: "Alice".into(),
                nonce: nonce.into(),
            }
        }
    }

    fn sign(claims: &TestClaims) -> String {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some("test-key".into());
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(TEST_RSA_PEM.as_bytes()).unwrap();
        jsonwebtoken::encode(&header, claims, &key).unwrap()
    }

    /// Mock Google: JWKS endpoint plus a token endpoint returning
    /// `id_token`; records token-request forms.
    async fn mock_google(
        id_token: String,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>,
    ) {
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_clone = received.clone();
        let jwks = serde_json::json!({
            "keys": [
                { "kty": "RSA", "alg": "RS256", "use": "sig",
                  "kid": "test-key", "n": TEST_RSA_N, "e": "AQAB" },
                { "kty": "RSA", "alg": "RS256", "use": "sig",
                  "kid": "other-key", "n": TEST_RSA_N, "e": "AQAB" },
            ]
        });
        let app = Router::new()
            .route(
                "/certs",
                get(move || {
                    let jwks = jwks.clone();
                    async move { Json(jwks) }
                }),
            )
            .route(
                "/token",
                post(
                    move |axum::extract::Form(form): axum::extract::Form<Vec<(String, String)>>| {
                        let received = received_clone.clone();
                        let id_token = id_token.clone();
                        async move {
                            received.lock().unwrap().push(form);
                            Json(serde_json::json!({
                                "access_token": "unused",
                                "token_type": "Bearer",
                                "id_token": id_token,
                            }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), received)
    }

    fn test_client(base: &str) -> GoogleOAuthClient {
        GoogleOAuthClient::with_endpoints(
            "test-client-id".into(),
            "test-client-secret".into(),
            "https://oyster.example.com/signup/callback".into(),
            format!("{base}/auth"),
            format!("{base}/token"),
            format!("{base}/certs"),
        )
    }

    #[test]
    fn begin_auth_builds_consent_url() {
        let client = test_client("https://mock");
        let req = client.begin_auth();

        let url = url::Url::parse(&req.url).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["client_id"], "test-client-id");
        assert_eq!(
            params["redirect_uri"],
            "https://oyster.example.com/signup/callback"
        );
        assert_eq!(params["response_type"], "code");
        assert_eq!(params["scope"], "openid email profile");
        assert_eq!(params["state"], req.state);
        assert_eq!(params["nonce"], req.nonce);
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["prompt"], "select_account");

        // The challenge is the S256 of the returned verifier.
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(req.pkce_verifier.as_bytes()));
        assert_eq!(params["code_challenge"], expected);

        // Every attempt gets fresh secrets.
        let req2 = client.begin_auth();
        assert_ne!(req.state, req2.state);
        assert_ne!(req.nonce, req2.nonce);
    }

    #[tokio::test]
    async fn exchange_code_posts_expected_form() {
        let token = sign(&TestClaims::valid("n"));
        let (base, received) = mock_google(token.clone()).await;
        let client = test_client(&base);

        let id_token = client
            .exchange_code("auth-code-1", "verifier-1")
            .await
            .unwrap();
        assert_eq!(id_token, token);

        let forms = received.lock().unwrap();
        let form = &forms[0];
        for expected in [
            ("grant_type", "authorization_code"),
            ("code", "auth-code-1"),
            ("client_id", "test-client-id"),
            ("client_secret", "test-client-secret"),
            ("redirect_uri", "https://oyster.example.com/signup/callback"),
            ("code_verifier", "verifier-1"),
        ] {
            assert!(
                form.contains(&(expected.0.into(), expected.1.into())),
                "missing {expected:?}"
            );
        }
    }

    #[tokio::test]
    async fn verify_accepts_valid_token() {
        let token = sign(&TestClaims::valid("nonce-1"));
        let (base, _) = mock_google(token.clone()).await;
        let client = test_client(&base);

        let identity = client.verify_id_token(&token, "nonce-1").await.unwrap();
        assert_eq!(identity.sub, "google-sub-1");
        assert_eq!(identity.email, "alice@example.com");
        assert!(identity.email_verified);
        assert_eq!(identity.name.as_deref(), Some("Alice"));
    }

    #[tokio::test]
    async fn verify_rejects_wrong_audience() {
        let mut claims = TestClaims::valid("n");
        claims.aud = "someone-elses-client".into();
        let token = sign(&claims);
        let (base, _) = mock_google(token.clone()).await;

        let err = test_client(&base)
            .verify_id_token(&token, "n")
            .await
            .unwrap_err();
        assert!(matches!(err, GoogleAuthError::InvalidIdToken(_)), "{err}");
    }

    #[tokio::test]
    async fn verify_rejects_wrong_issuer() {
        let mut claims = TestClaims::valid("n");
        claims.iss = "https://evil.example.com".into();
        let token = sign(&claims);
        let (base, _) = mock_google(token.clone()).await;

        let err = test_client(&base)
            .verify_id_token(&token, "n")
            .await
            .unwrap_err();
        assert!(matches!(err, GoogleAuthError::InvalidIdToken(_)), "{err}");
    }

    #[tokio::test]
    async fn verify_rejects_expired_token() {
        let mut claims = TestClaims::valid("n");
        claims.exp = (chrono::Utc::now() - chrono::Duration::hours(1)).timestamp();
        let token = sign(&claims);
        let (base, _) = mock_google(token.clone()).await;

        let err = test_client(&base)
            .verify_id_token(&token, "n")
            .await
            .unwrap_err();
        assert!(matches!(err, GoogleAuthError::InvalidIdToken(_)), "{err}");
    }

    #[tokio::test]
    async fn verify_rejects_nonce_mismatch() {
        let token = sign(&TestClaims::valid("attempt-A"));
        let (base, _) = mock_google(token.clone()).await;

        let err = test_client(&base)
            .verify_id_token(&token, "attempt-B")
            .await
            .unwrap_err();
        assert!(matches!(err, GoogleAuthError::NonceMismatch), "{err}");
    }

    #[tokio::test]
    async fn verify_rejects_unknown_kid() {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some("not-in-jwks".into());
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(TEST_RSA_PEM.as_bytes()).unwrap();
        let token = jsonwebtoken::encode(&header, &TestClaims::valid("n"), &key).unwrap();
        let (base, _) = mock_google(token.clone()).await;

        let err = test_client(&base)
            .verify_id_token(&token, "n")
            .await
            .unwrap_err();
        assert!(matches!(err, GoogleAuthError::InvalidIdToken(_)), "{err}");
    }

    #[tokio::test]
    async fn verify_rejects_tampered_token() {
        let token = sign(&TestClaims::valid("n"));
        let (base, _) = mock_google(token.clone()).await;

        // Flip a character in the payload segment.
        let mut parts: Vec<String> = token.split('.').map(String::from).collect();
        let mut payload = parts[1].clone().into_bytes();
        payload[0] = if payload[0] == b'A' { b'B' } else { b'A' };
        parts[1] = String::from_utf8(payload).unwrap();
        let tampered = parts.join(".");

        let err = test_client(&base)
            .verify_id_token(&tampered, "n")
            .await
            .unwrap_err();
        assert!(matches!(err, GoogleAuthError::InvalidIdToken(_)), "{err}");
    }

    #[tokio::test]
    async fn exchange_rejection_is_token_exchange_error() {
        let app = Router::new().route(
            "/token",
            post(|| async {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    r#"{"error":"invalid_grant"}"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = test_client(&format!("http://{addr}"));
        let err = client.exchange_code("bad-code", "v").await.unwrap_err();
        assert!(matches!(err, GoogleAuthError::TokenExchange(_)), "{err}");
    }
}
