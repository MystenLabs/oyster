//! Mock Google OAuth (+ Turnstile siteverify) server for local signup
//! testing — see `scripts/signup-testbed.sh`.
//!
//! Endpoints:
//! - `GET  /auth`       consent screen: a form asking which email to
//!   "sign in" as, then redirects to the app's callback with a code
//! - `POST /token`      exchanges that code for a genuinely RS256-signed
//!   id_token (test key; `kid: test-key`)
//! - `GET  /certs`      JWKS with the matching public key
//! - `POST /siteverify` Turnstile stub that always passes
//!
//! The "auth code" is just base64url(JSON{nonce, email, name}), so the
//! token endpoint can bind the real nonce into the id_token exactly as
//! Google would. Nothing here is a secret; the RSA key is a fixture.

use axum::{
    Form, Json, Router,
    extract::Query,
    response::{Html, Redirect},
    routing::{get, post},
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

const TEST_RSA_PEM: &str = include_str!("../src/signup/testdata/test_rsa.pem");
const TEST_RSA_N: &str = "v2WyZtglNbqzLRBkH5UNor5E-5UGzQU7qufO_b5vGs9ygEaBRpTgGqeZ4S6zOnaRRakSjOX-iOVXWSq9sPNqbXGgG5NV_37ZgeB55Q0g1CK_1fDuM4xNJ8kWmdxfkfL_LCEFdNKCTuJaYNOQjwkh-esnmqdXzhtxR2B2eNZh4zUBJcSqR-dwNcvPwwy41dEyp-_KcOhGk7C6PJvOLdCYv3z9sWzWrylg_GGhAEYcFRR02fHuHTZaCujtA8KrVxGgElUjMQV070qGstnFdD8zCC0cxefQf0TLtqOAqYLXfVDHe8HQhlPwLm1sch5_pR9po5n3SUmBe6yWtv20m1oiYw";

#[derive(Deserialize)]
struct AuthQuery {
    redirect_uri: String,
    state: String,
    nonce: String,
    client_id: String,
}

#[derive(Deserialize)]
struct ApproveQuery {
    redirect_uri: String,
    state: String,
    nonce: String,
    email: String,
    #[serde(default)]
    name: String,
}

#[derive(Serialize, Deserialize)]
struct CodePayload {
    nonce: String,
    email: String,
    name: String,
}

#[derive(Serialize)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    exp: i64,
    email: String,
    email_verified: bool,
    name: String,
    nonce: String,
}

async fn auth_page(Query(q): Query<AuthQuery>) -> Html<String> {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    Html(format!(
        r#"<!doctype html><html><head><title>Mock Google</title>
<style>body{{font-family:system-ui;display:flex;justify-content:center;align-items:center;min-height:100vh;background:#f2f2f2}}
main{{background:#fff;border:1px solid #ccc;border-radius:8px;padding:2rem 2.5rem}}
input{{display:block;margin:.5rem 0 1rem;padding:.5rem;width:20rem}}</style></head><body><main>
<h2>Mock Google sign-in</h2>
<p>Pretend to be any account (client: <code>{client}</code>):</p>
<form method="GET" action="/auth/approve">
  <input type="hidden" name="redirect_uri" value="{redirect}">
  <input type="hidden" name="state" value="{state}">
  <input type="hidden" name="nonce" value="{nonce}">
  <label>Email <input name="email" value="dev@example.com" required></label>
  <label>Name <input name="name" value="Dev User"></label>
  <button type="submit">Sign in</button>
</form></main></body></html>"#,
        client = esc(&q.client_id),
        redirect = esc(&q.redirect_uri),
        state = esc(&q.state),
        nonce = esc(&q.nonce),
    ))
}

async fn approve(Query(q): Query<ApproveQuery>) -> Redirect {
    let payload = CodePayload {
        nonce: q.nonce,
        email: q.email,
        name: q.name,
    };
    let code = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).expect("serialize code payload"));
    let sep = if q.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    Redirect::to(&format!(
        "{}{}code={}&state={}",
        q.redirect_uri,
        sep,
        code,
        urlencoding_encode(&q.state)
    ))
}

/// Minimal percent-encoding for the state query value.
fn urlencoding_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

async fn token(Form(form): Form<Vec<(String, String)>>) -> Json<serde_json::Value> {
    let code = form
        .iter()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let client_id = form
        .iter()
        .find(|(k, _)| k == "client_id")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let payload: CodePayload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&code)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(CodePayload {
            nonce: String::new(),
            email: "dev@example.com".into(),
            name: "Dev User".into(),
        });

    let claims = Claims {
        iss: "https://accounts.google.com".into(),
        aud: client_id,
        sub: format!("mock:{}", payload.email),
        exp: chrono::Utc::now().timestamp() + 3600,
        email: payload.email,
        email_verified: true,
        name: payload.name,
        nonce: payload.nonce,
    };
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("test-key".into());
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(TEST_RSA_PEM.as_bytes())
        .expect("test RSA key parses");
    let id_token = jsonwebtoken::encode(&header, &claims, &key).expect("sign id_token");

    Json(serde_json::json!({
        "access_token": "mock-access-token",
        "token_type": "Bearer",
        "expires_in": 3600,
        "id_token": id_token,
    }))
}

async fn certs() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "keys": [{
            "kty": "RSA", "alg": "RS256", "use": "sig",
            "kid": "test-key", "n": TEST_RSA_N, "e": "AQAB",
        }]
    }))
}

async fn siteverify() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "success": true }))
}

#[tokio::main]
async fn main() {
    let bind = std::env::var("MOCK_GOOGLE_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:9081".into());
    let app = Router::new()
        .route("/auth", get(auth_page))
        .route("/auth/approve", get(approve))
        .route("/token", post(token))
        .route("/certs", get(certs))
        .route("/siteverify", post(siteverify));

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|e| panic!("bind {bind}: {e}"));
    eprintln!("mock google listening on http://{bind}");
    eprintln!("  auth:       http://{bind}/auth");
    eprintln!("  token:      http://{bind}/token");
    eprintln!("  jwks:       http://{bind}/certs");
    eprintln!("  siteverify: http://{bind}/siteverify (always passes)");
    axum::serve(listener, app).await.expect("server error");
}
