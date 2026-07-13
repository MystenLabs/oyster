//! HTTP routes for the self-serve signup flow.
//!
//! ```text
//! GET  /signup           page: Turnstile widget + "Continue with Google"
//! POST /signup/start     verify Turnstile → set state cookie → redirect to Google
//! GET  /signup/callback  validate state → exchange code → verify id_token →
//!                        find-or-create user → (open mode) app + admin key
//! GET  /signup/keys      minimal signed-in landing (dashboard grows here)
//! ```
//!
//! These routes carry their own [`SignupService`] state (not
//! [`crate::AppState`]) and are only mounted when signup is configured.

use axum::{
    Router,
    extract::{Form, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;

use crate::{
    UserId, app_admin, auth,
    config::{SignupConfig, SignupMode},
    db::{self, users::IdentityProvider},
    models::User,
    signup::{google::GoogleOAuthClient, turnstile::TurnstileVerifier},
};

/// Cookie carrying `state.nonce.pkce_verifier` between `/signup/start`
/// and the callback. The values are compared against what Google echoes
/// back — a cross-site attacker cannot read or set this cookie, which is
/// what makes `state` an effective CSRF check (double-submit pattern).
const OAUTH_COOKIE: &str = "oyster_oauth";
/// Lifetime of the OAuth state cookie (one consent-screen round trip).
const OAUTH_COOKIE_MAX_AGE_SECS: i64 = 600;

/// Browser session cookie holding the raw session token.
const SESSION_COOKIE: &str = "oyster_session";
/// Browser session lifetime.
pub(crate) const SESSION_TTL: chrono::Duration = chrono::Duration::hours(8);

const SIGNUP_PAGE: &str = include_str!("pages/signup.html");
const REVEAL_PAGE: &str = include_str!("pages/reveal.html");
const MESSAGE_PAGE: &str = include_str!("pages/message.html");
const DASHBOARD_PAGE: &str = include_str!("pages/dashboard.html");

/// State for the signup routes.
#[derive(Clone)]
pub struct SignupService {
    /// Database pool (shared with the main app).
    pub db: db::DbPool,
    /// Signup configuration.
    pub config: SignupConfig,
    /// Active-key cap applied to web issuance.
    pub max_admin_keys_per_app: i64,
    /// Turnstile verifier.
    pub turnstile: TurnstileVerifier,
    /// Google OAuth client.
    pub google: GoogleOAuthClient,
}

impl SignupService {
    /// Whether cookies should carry the `Secure` attribute (true when
    /// the public base URL is https; false allows the http local testbed).
    fn secure_cookies(&self) -> bool {
        self.config.public_base_url.starts_with("https://")
    }
}

/// Build the production signup router from config: real Cloudflare and
/// Google endpoints, redirect URI derived from the public base URL.
pub fn build_signup_router(
    db: db::DbPool,
    config: SignupConfig,
    max_admin_keys_per_app: i64,
) -> Router {
    let redirect_uri = format!("{}/signup/callback", config.public_base_url);
    // Dev-only endpoint overrides let the local testbed point at mock
    // Google/Turnstile servers; production leaves them unset.
    let or =
        |over: &Option<String>, default: &str| over.clone().unwrap_or_else(|| default.to_string());
    let service = SignupService {
        turnstile: TurnstileVerifier::with_endpoint(
            config.turnstile_secret_key.clone(),
            or(
                &config.turnstile_siteverify_url,
                crate::signup::turnstile::SITEVERIFY_URL,
            ),
        ),
        google: GoogleOAuthClient::with_endpoints(
            config.google_client_id.clone(),
            config.google_client_secret.clone(),
            redirect_uri,
            or(&config.google_auth_url, crate::signup::google::AUTH_URL),
            or(&config.google_token_url, crate::signup::google::TOKEN_URL),
            or(&config.google_jwks_url, crate::signup::google::JWKS_URL),
        ),
        db,
        max_admin_keys_per_app,
        config,
    };
    router_with_service(service)
}

/// Build the signup router around an explicit service — used by tests
/// and the local testbed to inject mock Turnstile/Google endpoints.
pub fn router_with_service(service: SignupService) -> Router {
    Router::new()
        .route("/signup", get(signup_page))
        .route("/signup/start", post(signup_start))
        .route("/signup/callback", get(signup_callback))
        .route("/signup/keys", get(dashboard))
        .route("/signup/keys/issue", post(issue_key))
        .route("/signup/keys/revoke", post(revoke_key))
        .route("/signup/logout", post(logout))
        .with_state(service)
}

// ---------------------------------------------------------------------------
// Small HTML/cookie helpers (deliberately no template/cookie crates —
// three pages and two cookies don't justify the dependencies).
// ---------------------------------------------------------------------------

/// Minimal HTML escaping for user-derived values interpolated into pages.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Render the generic message page.
fn message_page(status: StatusCode, title: &str, message: &str) -> Response {
    let body = MESSAGE_PAGE
        .replace("{{TITLE}}", &escape_html(title))
        .replace("{{MESSAGE}}", &escape_html(message));
    (status, Html(body)).into_response()
}

/// Read a cookie value from request headers.
fn get_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|c| {
        let (k, v) = c.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

/// Build a `Set-Cookie` value. `max_age <= 0` deletes the cookie.
fn cookie_value(name: &str, value: &str, max_age_secs: i64, secure: bool) -> HeaderValue {
    let secure_attr = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/signup; Max-Age={max_age_secs}; HttpOnly; SameSite=Lax{secure_attr}"
    ))
    .expect("cookie value is header-safe")
}

/// Resolve the signed-in user from the session cookie.
async fn session_user(service: &SignupService, headers: &HeaderMap) -> Option<UserId> {
    let token = get_cookie(headers, SESSION_COOKIE)?;
    db::web_sessions::find_active_by_hash(&service.db, &auth::hash_api_key(&token))
        .await
        .ok()
        .flatten()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /signup` — the signup/login page.
async fn signup_page(State(service): State<SignupService>) -> Response {
    let body = SIGNUP_PAGE.replace("{{TURNSTILE_SITE_KEY}}", &service.config.turnstile_site_key);
    Html(body).into_response()
}

/// Form posted by the signup page; the Turnstile widget injects the
/// `cf-turnstile-response` field.
#[derive(Deserialize)]
struct StartForm {
    #[serde(rename = "cf-turnstile-response", default)]
    turnstile_token: String,
}

/// `POST /signup/start` — Turnstile gate, then off to Google.
async fn signup_start(
    State(service): State<SignupService>,
    Form(form): Form<StartForm>,
) -> Response {
    if form.turnstile_token.is_empty() {
        return message_page(
            StatusCode::BAD_REQUEST,
            "Verification required",
            "The anti-bot check did not complete. Please go back and try again.",
        );
    }

    let verdict = match service.turnstile.verify(&form.turnstile_token, None).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "turnstile siteverify failed");
            return message_page(
                StatusCode::BAD_GATEWAY,
                "Verification unavailable",
                "The anti-bot service could not be reached. Please try again shortly.",
            );
        }
    };
    if !verdict.success {
        tracing::info!(error_codes = ?verdict.error_codes, "turnstile verification failed");
        return message_page(
            StatusCode::BAD_REQUEST,
            "Verification failed",
            "The anti-bot check failed. Please go back and try again.",
        );
    }

    let auth_request = service.google.begin_auth();
    let cookie = cookie_value(
        OAUTH_COOKIE,
        &format!(
            "{}.{}.{}",
            auth_request.state, auth_request.nonce, auth_request.pkce_verifier
        ),
        OAUTH_COOKIE_MAX_AGE_SECS,
        service.secure_cookies(),
    );

    let mut response = Redirect::to(&auth_request.url).into_response();
    response.headers_mut().append(header::SET_COOKIE, cookie);
    response
}

/// Query params Google sends to the callback.
#[derive(Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    /// Set when the user cancels the consent screen.
    #[serde(default)]
    error: Option<String>,
}

/// `GET /signup/callback` — the heart of the flow.
async fn signup_callback(
    State(service): State<SignupService>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if let Some(error) = &query.error {
        tracing::info!(error, "google oauth declined");
        return message_page(
            StatusCode::BAD_REQUEST,
            "Sign-in cancelled",
            "Google sign-in was not completed.",
        );
    }
    let (Some(code), Some(state)) = (&query.code, &query.state) else {
        return message_page(
            StatusCode::BAD_REQUEST,
            "Invalid callback",
            "Missing code or state parameter.",
        );
    };

    // Recover this attempt's secrets from the cookie and validate state.
    let Some(oauth_cookie) = get_cookie(&headers, OAUTH_COOKIE) else {
        return message_page(
            StatusCode::BAD_REQUEST,
            "Sign-in expired",
            "Your sign-in attempt expired or the cookie is missing. Please try again.",
        );
    };
    let mut parts = oauth_cookie.splitn(3, '.');
    let (Some(expected_state), Some(nonce), Some(pkce_verifier)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return message_page(
            StatusCode::BAD_REQUEST,
            "Sign-in expired",
            "Your sign-in attempt could not be validated. Please try again.",
        );
    };
    if state != expected_state {
        tracing::warn!("oauth state mismatch");
        return message_page(
            StatusCode::BAD_REQUEST,
            "Sign-in rejected",
            "State validation failed. Please try signing in again.",
        );
    }

    // The state cookie is single-use: clear it on every path below.
    let clear_oauth = cookie_value(OAUTH_COOKIE, "", 0, service.secure_cookies());

    // Exchange the code and verify the id_token.
    let identity = match service.google.exchange_code(code, pkce_verifier).await {
        Ok(id_token) => match service.google.verify_id_token(&id_token, nonce).await {
            Ok(identity) => identity,
            Err(e) => {
                tracing::warn!(error = %e, "id_token verification failed");
                let mut r = message_page(
                    StatusCode::BAD_REQUEST,
                    "Sign-in rejected",
                    "Google's response could not be verified. Please try again.",
                );
                r.headers_mut().append(header::SET_COOKIE, clear_oauth);
                return r;
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "code exchange failed");
            let mut r = message_page(
                StatusCode::BAD_GATEWAY,
                "Sign-in failed",
                "Could not complete sign-in with Google. Please try again.",
            );
            r.headers_mut().append(header::SET_COOKIE, clear_oauth);
            return r;
        }
    };

    if !identity.email_verified {
        let mut r = message_page(
            StatusCode::FORBIDDEN,
            "Email not verified",
            "Your Google account's email address is not verified.",
        );
        r.headers_mut().append(header::SET_COOKIE, clear_oauth);
        return r;
    }

    let mut response = match complete_sign_in(&service, &identity).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "sign-in completion failed");
            message_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "Sign-in could not be completed. Please try again.",
            )
        }
    };
    response
        .headers_mut()
        .append(header::SET_COOKIE, clear_oauth);
    response
}

/// Post-verification: find or (mode permitting) create the user, their
/// app, and first admin key; establish a session.
async fn complete_sign_in(
    service: &SignupService,
    identity: &crate::signup::google::GoogleIdentity,
) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
    let existing =
        db::users::find_user_by_identity(&service.db, IdentityProvider::Google, &identity.sub)
            .await?;

    let user = match existing {
        Some(user) => user,
        // New identity: gate by mode. The email compared inside the
        // gate is Google-attested (email_verified was already required).
        None => match gate_new_identity(service, identity).await? {
            Gate::Admit => create_user(service, identity).await?,
            Gate::Respond(response) => return Ok(response),
        },
    };

    // Ensure the user has an app; issue the first admin key alongside a
    // fresh app. This also self-heals a crash that created the user but
    // not the app.
    let (_app, fresh_key) = ensure_app_for_user(service, &user).await?;

    // Opportunistically clear this user's expired sessions, then start
    // a new one.
    db::web_sessions::delete_expired_for_user(&service.db, &user.id).await?;
    let session_token =
        db::web_sessions::create_session(&service.db, &user.id, SESSION_TTL).await?;
    let session_cookie = cookie_value(
        SESSION_COOKIE,
        &session_token,
        SESSION_TTL.num_seconds(),
        service.secure_cookies(),
    );

    let mut response = match fresh_key {
        // One-time reveal. The raw key exists in this response only.
        Some((app_id, key)) => {
            let body = REVEAL_PAGE
                .replace("{{APP_ID}}", &escape_html(&app_id))
                .replace("{{ADMIN_KEY}}", &escape_html(&key.bearer_token));
            let mut r = Html(body).into_response();
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        None => Redirect::to("/signup/keys").into_response(),
    };
    response
        .headers_mut()
        .append(header::SET_COOKIE, session_cookie);
    Ok(response)
}

/// Create the user + identity rows for an admitted identity.
async fn create_user(
    service: &SignupService,
    identity: &crate::signup::google::GoogleIdentity,
) -> Result<User, sqlx::Error> {
    db::users::create_user_with_identity(
        &service.db,
        IdentityProvider::Google,
        &identity.sub,
        &identity.email,
        identity.name.as_deref(),
    )
    .await
}

/// Whether the (verified) email's domain is in the allowed list.
fn email_domain_allowed(allowed_domains: &[String], email: &str) -> bool {
    let Some((_, domain)) = email.rsplit_once('@') else {
        return false;
    };
    let domain = domain.to_lowercase();
    allowed_domains.contains(&domain)
}

/// Outcome of gating a brand-new identity.
enum Gate {
    /// Proceed with signup on this very sign-in.
    Admit,
    /// Stop here with this page instead.
    Respond(Response),
}

/// Apply the signup mode to an identity that has no user yet.
async fn gate_new_identity(
    service: &SignupService,
    identity: &crate::signup::google::GoogleIdentity,
) -> Result<Gate, Box<dyn std::error::Error + Send + Sync>> {
    use db::signup_requests::SignupRequestStatus;

    match service.config.mode {
        SignupMode::Open => Ok(Gate::Admit),
        SignupMode::Closed => Ok(Gate::Respond(message_page(
            StatusCode::FORBIDDEN,
            "Signup is closed",
            "New signups are not being accepted right now.",
        ))),
        SignupMode::Waitlist => {
            if email_domain_allowed(&service.config.allowed_domains, &identity.email) {
                return Ok(Gate::Admit);
            }
            let existing = db::signup_requests::find_by_identity(
                &service.db,
                IdentityProvider::Google,
                &identity.sub,
            )
            .await?;
            match existing.map(|r| r.status) {
                // Approved: signup completes on this very sign-in.
                Some(SignupRequestStatus::Approved) => Ok(Gate::Admit),
                Some(SignupRequestStatus::Rejected) => Ok(Gate::Respond(message_page(
                    StatusCode::FORBIDDEN,
                    "Request declined",
                    "Your access request was not approved.",
                ))),
                Some(SignupRequestStatus::Pending) => Ok(Gate::Respond(message_page(
                    StatusCode::OK,
                    "Still in review",
                    "Your access request is waiting for review. Sign in again once \
                     you've been approved.",
                ))),
                None => {
                    db::signup_requests::create_or_get(
                        &service.db,
                        IdentityProvider::Google,
                        &identity.sub,
                        &identity.email,
                        identity.name.as_deref(),
                    )
                    .await?;
                    tracing::info!(email = %identity.email, "signup request filed");
                    Ok(Gate::Respond(message_page(
                        StatusCode::OK,
                        "Request received",
                        "Thanks — your access request is on file. Sign in again once \
                         you've been approved.",
                    )))
                }
            }
        }
    }
}

/// Find the user's app, creating app + first admin key when absent.
/// Returns the app plus the freshly issued key (only on creation).
async fn ensure_app_for_user(
    service: &SignupService,
    user: &User,
) -> Result<
    (
        crate::models::App,
        Option<(String, crate::models::AdminKeyWithBearerToken)>,
    ),
    Box<dyn std::error::Error + Send + Sync>,
> {
    if let Some(app) = db::apps::find_app_by_owner(&service.db, &user.id).await? {
        return Ok((app, None));
    }

    // App names are globally unique; use the email and fall back to a
    // uuid-suffixed name on collision.
    let app =
        match db::apps::create_app_owned(&service.db, &user.email, &user.email, &user.id).await {
            Ok(app) => app,
            Err(e) if is_unique_violation(&e) => {
                let name = format!("{} ({})", user.email, &user.id.to_string()[..8]);
                db::apps::create_app_owned(&service.db, &name, &user.email, &user.id).await?
            }
            Err(e) => return Err(e.into()),
        };

    let key =
        app_admin::issue_admin_key(&service.db, &app.id, Some(service.max_admin_keys_per_app))
            .await?;
    Ok((app.clone(), Some((app.id.to_string(), key))))
}

/// Whether a sqlx error is a unique-constraint violation.
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(
        e.as_database_error().map(|d| d.kind()),
        Some(sqlx::error::ErrorKind::UniqueViolation)
    )
}

/// Resolve the signed-in user and their app for dashboard handlers.
/// `Err` carries the redirect/error response to return as-is.
async fn dashboard_context(
    service: &SignupService,
    headers: &HeaderMap,
) -> Result<(User, crate::models::App), Response> {
    let Some(user_id) = session_user(service, headers).await else {
        return Err(Redirect::to("/signup").into_response());
    };
    let user = db::users::get_user(&service.db, &user_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| Redirect::to("/signup").into_response())?;
    let app = db::apps::find_app_by_owner(&service.db, &user.id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| {
            // A session without an app shouldn't happen (the callback
            // self-heals missing apps); send them back through sign-in.
            Redirect::to("/signup").into_response()
        })?;
    Ok((user, app))
}

/// `GET /signup/keys` — the key management dashboard.
async fn dashboard(State(service): State<SignupService>, headers: HeaderMap) -> Response {
    let (user, app) = match dashboard_context(&service, &headers).await {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    let keys = match db::app_admin_keys::list_admin_keys(&service.db, &app.id).await {
        Ok(keys) => keys,
        Err(e) => {
            tracing::error!(error = %e, "failed to list admin keys");
            return message_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "Could not load your keys. Please try again.",
            );
        }
    };

    let rows: String = keys
        .iter()
        .map(|k| {
            let (status, action) = match &k.revoked_at {
                Some(at) => (
                    format!(
                        r#"<span class="revoked">revoked {}</span>"#,
                        escape_html(at)
                    ),
                    String::new(),
                ),
                None => (
                    "active".to_string(),
                    format!(
                        r#"<form class="inline" method="POST" action="/signup/keys/revoke">
                           <input type="hidden" name="key_id" value="{}">
                           <button type="submit" class="danger">revoke</button></form>"#,
                        escape_html(&k.id)
                    ),
                ),
            };
            format!(
                "<tr><td><code>{}…</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&k.prefix),
                escape_html(&k.created_at),
                status,
                action,
            )
        })
        .collect();
    let rows = if rows.is_empty() {
        r#"<tr><td colspan="4">No keys yet — issue one below.</td></tr>"#.to_string()
    } else {
        rows
    };

    let body = DASHBOARD_PAGE
        .replace("{{EMAIL}}", &escape_html(&user.email))
        .replace("{{APP_ID}}", &escape_html(&app.id.to_string()))
        .replace("{{KEY_ROWS}}", &rows);
    Html(body).into_response()
}

/// `POST /signup/keys/issue` — issue a fresh admin key (cap enforced)
/// and show it once.
async fn issue_key(State(service): State<SignupService>, headers: HeaderMap) -> Response {
    let (user, app) = match dashboard_context(&service, &headers).await {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };

    let key = match app_admin::issue_admin_key(
        &service.db,
        &app.id,
        Some(service.max_admin_keys_per_app),
    )
    .await
    {
        Ok(key) => key,
        Err(app_admin::IssueAdminKeyError::LimitReached(limit)) => {
            return message_page(
                StatusCode::CONFLICT,
                "Key limit reached",
                &format!(
                    "This app already has {limit} active keys. Revoke one you no longer use, \
                     then try again."
                ),
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "web admin-key issuance failed");
            return message_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "The key could not be issued. Please try again.",
            );
        }
    };

    let _ = db::audit_events::record_audit_event(
        &service.db,
        &app.id,
        None,
        "admin_key.issued_via_web",
        serde_json::json!({ "key_id": key.id, "prefix": key.prefix, "user_id": user.id }),
    )
    .await
    .inspect_err(|e| tracing::error!(error = %e, "audit event write failed"));

    let body = REVEAL_PAGE
        .replace("{{APP_ID}}", &escape_html(&app.id.to_string()))
        .replace("{{ADMIN_KEY}}", &escape_html(&key.bearer_token));
    let mut response = Html(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Form for `POST /signup/keys/revoke`.
#[derive(Deserialize)]
struct RevokeForm {
    key_id: String,
}

/// `POST /signup/keys/revoke` — revoke one of the app's own keys.
async fn revoke_key(
    State(service): State<SignupService>,
    headers: HeaderMap,
    Form(form): Form<RevokeForm>,
) -> Response {
    let (user, app) = match dashboard_context(&service, &headers).await {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };

    // Scoped to the session's app: a forged key_id belonging to another
    // app is a no-op.
    match db::app_admin_keys::revoke_admin_key_for_app(&service.db, &form.key_id, &app.id).await {
        Ok(true) => {
            let _ = db::audit_events::record_audit_event(
                &service.db,
                &app.id,
                None,
                "admin_key.revoked_via_web",
                serde_json::json!({ "key_id": form.key_id, "user_id": user.id }),
            )
            .await
            .inspect_err(|e| tracing::error!(error = %e, "audit event write failed"));
        }
        Ok(false) => {
            tracing::info!(key_id = %form.key_id, "web revoke matched no active key");
        }
        Err(e) => {
            tracing::error!(error = %e, "web admin-key revocation failed");
            return message_page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong",
                "The key could not be revoked. Please try again.",
            );
        }
    }
    Redirect::to("/signup/keys").into_response()
}

/// `POST /signup/logout` — delete the session and clear the cookie.
async fn logout(State(service): State<SignupService>, headers: HeaderMap) -> Response {
    if let Some(token) = get_cookie(&headers, SESSION_COOKIE) {
        let _ = db::web_sessions::delete_by_hash(&service.db, &auth::hash_api_key(&token))
            .await
            .inspect_err(|e| tracing::error!(error = %e, "session delete failed"));
    }
    let mut response = Redirect::to("/signup").into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        cookie_value(SESSION_COOKIE, "", 0, service.secure_cookies()),
    );
    response
}

#[cfg(test)]
mod tests {
    use axum::{Json, Router as AxRouter, routing::get as ax_get, routing::post as ax_post};

    use super::*;
    use crate::config::SignupMode;

    // Reuse the Google test key/JWKS from the google module's tests via
    // fresh constants here (test-only material, not secrets).
    const TEST_RSA_PEM: &str = include_str!("testdata/test_rsa.pem");
    const TEST_RSA_N: &str = "v2WyZtglNbqzLRBkH5UNor5E-5UGzQU7qufO_b5vGs9ygEaBRpTgGqeZ4S6zOnaRRakSjOX-iOVXWSq9sPNqbXGgG5NV_37ZgeB55Q0g1CK_1fDuM4xNJ8kWmdxfkfL_LCEFdNKCTuJaYNOQjwkh-esnmqdXzhtxR2B2eNZh4zUBJcSqR-dwNcvPwwy41dEyp-_KcOhGk7C6PJvOLdCYv3z9sWzWrylg_GGhAEYcFRR02fHuHTZaCujtA8KrVxGgElUjMQV070qGstnFdD8zCC0cxefQf0TLtqOAqYLXfVDHe8HQhlPwLm1sch5_pR9po5n3SUmBe6yWtv20m1oiYw";

    #[derive(serde::Serialize)]
    struct MockClaims {
        iss: String,
        aud: String,
        sub: String,
        exp: i64,
        email: String,
        email_verified: bool,
        name: String,
        nonce: String,
    }

    /// Mock Google whose /token endpoint mints a valid id_token for
    /// `sub`/`email`, echoing the nonce provided via the `code` (tests
    /// pass the nonce as the auth code — see below).
    async fn mock_google(sub: &str, email: &str, email_verified: bool) -> String {
        let sub = sub.to_string();
        let email = email.to_string();
        let jwks = serde_json::json!({
            "keys": [{ "kty": "RSA", "alg": "RS256", "use": "sig",
                       "kid": "test-key", "n": TEST_RSA_N, "e": "AQAB" }]
        });
        let app = AxRouter::new()
            .route(
                "/certs",
                ax_get(move || {
                    let jwks = jwks.clone();
                    async move { Json(jwks) }
                }),
            )
            .route(
                "/token",
                ax_post(
                    move |axum::extract::Form(form): axum::extract::Form<Vec<(String, String)>>| {
                        let sub = sub.clone();
                        let email = email.clone();
                        async move {
                            // The signup flow can't tell us the nonce out of
                            // band, but it *sends the auth code* — and the
                            // tests use the real nonce as the code.
                            let nonce = form
                                .iter()
                                .find(|(k, _)| k == "code")
                                .map(|(_, v)| v.clone())
                                .unwrap_or_default();
                            let claims = MockClaims {
                                iss: "https://accounts.google.com".into(),
                                aud: "test-client-id".into(),
                                sub,
                                exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
                                email,
                                email_verified,
                                name: "Test User".into(),
                                nonce,
                            };
                            let mut header =
                                jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
                            header.kid = Some("test-key".into());
                            let key =
                                jsonwebtoken::EncodingKey::from_rsa_pem(TEST_RSA_PEM.as_bytes())
                                    .unwrap();
                            let token = jsonwebtoken::encode(&header, &claims, &key).unwrap();
                            Json(serde_json::json!({ "id_token": token }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// Mock Turnstile that accepts exactly the token "pass".
    async fn mock_turnstile() -> String {
        let app = AxRouter::new().route(
            "/siteverify",
            ax_post(
                |axum::extract::Form(form): axum::extract::Form<Vec<(String, String)>>| async move {
                    let ok = form.iter().any(|(k, v)| k == "response" && v == "pass");
                    Json(serde_json::json!({ "success": ok }))
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/siteverify")
    }

    async fn test_service(
        mode: SignupMode,
        google_base: &str,
        turnstile_url: &str,
    ) -> SignupService {
        test_service_with_domains(mode, &[], google_base, turnstile_url).await
    }

    async fn test_service_with_domains(
        mode: SignupMode,
        allowed_domains: &[&str],
        google_base: &str,
        turnstile_url: &str,
    ) -> SignupService {
        SignupService {
            db: db::create_pool("sqlite::memory:").await.unwrap(),
            config: SignupConfig {
                mode,
                allowed_domains: allowed_domains.iter().map(|d| d.to_string()).collect(),
                google_auth_url: None,
                google_token_url: None,
                google_jwks_url: None,
                turnstile_siteverify_url: None,
                public_base_url: "http://localhost:3000".into(),
                google_client_id: "test-client-id".into(),
                google_client_secret: "test-client-secret".into(),
                turnstile_site_key: "test-site-key".into(),
                turnstile_secret_key: "test-turnstile-secret".into(),
            },
            max_admin_keys_per_app: 5,
            turnstile: TurnstileVerifier::with_endpoint(
                "test-turnstile-secret".into(),
                turnstile_url.into(),
            ),
            google: GoogleOAuthClient::with_endpoints(
                "test-client-id".into(),
                "test-client-secret".into(),
                "http://localhost:3000/signup/callback".into(),
                format!("{google_base}/auth"),
                format!("{google_base}/token"),
                format!("{google_base}/certs"),
            ),
        }
    }

    /// Boot the signup router on a random port; returns its base URL.
    async fn serve(service: SignupService) -> String {
        let app = router_with_service(service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    /// Extract a cookie value from Set-Cookie headers.
    fn cookie_from(response: &reqwest::Response, name: &str) -> Option<String> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .find_map(|v| {
                let v = v.to_str().ok()?;
                let (k, rest) = v.split_once('=')?;
                (k == name).then(|| rest.split(';').next().unwrap_or("").to_string())
            })
    }

    /// One start → callback round trip against an already-running
    /// signup server (mock Google mints the id_token; the nonce rides
    /// as the auth code).
    async fn sign_in(base: &str) -> reqwest::Response {
        let http = client();
        let start = http
            .post(format!("{base}/signup/start"))
            .form(&[("cf-turnstile-response", "pass")])
            .send()
            .await
            .unwrap();
        assert_eq!(start.status(), 303);
        let oauth_cookie = cookie_from(&start, OAUTH_COOKIE).unwrap();
        let location = start.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_string();
        let auth_url = url::Url::parse(&location).unwrap();
        let params: std::collections::HashMap<_, _> = auth_url.query_pairs().into_owned().collect();

        http.get(format!(
            "{base}/signup/callback?code={}&state={}",
            params["nonce"], params["state"]
        ))
        .header(header::COOKIE, format!("{OAUTH_COOKIE}={}", oauth_cookie))
        .send()
        .await
        .unwrap()
    }

    /// Drive the full happy path: page → start → callback. Returns the
    /// callback response and the service (for db assertions).
    async fn run_flow(
        mode: SignupMode,
        sub: &str,
        email: &str,
        email_verified: bool,
    ) -> (reqwest::Response, SignupService, String) {
        run_flow_with_domains(mode, &[], sub, email, email_verified).await
    }

    async fn run_flow_with_domains(
        mode: SignupMode,
        allowed_domains: &[&str],
        sub: &str,
        email: &str,
        email_verified: bool,
    ) -> (reqwest::Response, SignupService, String) {
        let google = mock_google(sub, email, email_verified).await;
        let turnstile = mock_turnstile().await;
        let service = test_service_with_domains(mode, allowed_domains, &google, &turnstile).await;
        let base = serve(service.clone()).await;
        let http = client();

        // Page renders with the sitekey.
        let page = http.get(format!("{base}/signup")).send().await.unwrap();
        assert_eq!(page.status(), 200);
        assert!(page.text().await.unwrap().contains("test-site-key"));

        let callback = sign_in(&base).await;
        (callback, service, base)
    }

    #[tokio::test]
    async fn open_mode_full_flow_creates_user_app_and_key() {
        let (callback, service, base) =
            run_flow(SignupMode::Open, "sub-flow-1", "alice@example.com", true).await;

        assert_eq!(callback.status(), 200);
        assert_eq!(callback.headers()[header::CACHE_CONTROL], "no-store");
        let session = cookie_from(&callback, SESSION_COOKIE).unwrap();
        let body = callback.text().await.unwrap();
        assert!(body.contains("Your app is ready"), "{body}");

        // User, identity, app, and key all exist.
        let user =
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-flow-1")
                .await
                .unwrap()
                .expect("user created");
        assert_eq!(user.email, "alice@example.com");
        let app = db::apps::find_app_by_owner(&service.db, &user.id)
            .await
            .unwrap()
            .expect("app created");
        assert_eq!(app.name, "alice@example.com");
        let count = db::app_admin_keys::count_active_admin_keys(&service.db, &app.id)
            .await
            .unwrap();
        assert_eq!(count, 1);
        // The page shows the app id and a 64-hex key.
        assert!(body.contains(&app.id.to_string()));

        // The session cookie authenticates the landing page.
        let landing = client()
            .get(format!("{base}/signup/keys"))
            .header(header::COOKIE, format!("{SESSION_COOKIE}={session}"))
            .send()
            .await
            .unwrap();
        assert_eq!(landing.status(), 200);
        assert!(landing.text().await.unwrap().contains("Signed in"));
    }

    #[tokio::test]
    async fn returning_user_gets_session_not_new_key() {
        let (first, service, base) =
            run_flow(SignupMode::Open, "sub-flow-2", "bob@example.com", true).await;
        assert_eq!(first.status(), 200);

        // Second sign-in through the same service/server.
        let http = client();
        let start = http
            .post(format!("{base}/signup/start"))
            .form(&[("cf-turnstile-response", "pass")])
            .send()
            .await
            .unwrap();
        let oauth_cookie = cookie_from(&start, OAUTH_COOKIE).unwrap();
        let location = start.headers()[header::LOCATION].to_str().unwrap();
        let auth_url = url::Url::parse(location).unwrap();
        let params: std::collections::HashMap<_, _> = auth_url.query_pairs().into_owned().collect();

        let callback = http
            .get(format!(
                "{base}/signup/callback?code={}&state={}",
                params["nonce"], params["state"]
            ))
            .header(header::COOKIE, format!("{OAUTH_COOKIE}={oauth_cookie}"))
            .send()
            .await
            .unwrap();

        // Redirect to the landing page, session set, and still one key.
        assert_eq!(callback.status(), 303);
        assert!(cookie_from(&callback, SESSION_COOKIE).is_some());
        let user =
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-flow-2")
                .await
                .unwrap()
                .unwrap();
        let app = db::apps::find_app_by_owner(&service.db, &user.id)
            .await
            .unwrap()
            .unwrap();
        let count = db::app_admin_keys::count_active_admin_keys(&service.db, &app.id)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn closed_mode_rejects_new_identity() {
        let (callback, service, _) =
            run_flow(SignupMode::Closed, "sub-flow-3", "carol@example.com", true).await;
        assert_eq!(callback.status(), 403);
        assert!(callback.text().await.unwrap().contains("Signup is closed"));
        let user =
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-flow-3")
                .await
                .unwrap();
        assert!(user.is_none());
    }

    #[tokio::test]
    async fn unverified_email_is_rejected() {
        let (callback, service, _) =
            run_flow(SignupMode::Open, "sub-flow-4", "dave@example.com", false).await;
        assert_eq!(callback.status(), 403);
        assert!(callback.text().await.unwrap().contains("not verified"));
        let user =
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-flow-4")
                .await
                .unwrap();
        assert!(user.is_none());
    }

    #[tokio::test]
    async fn failed_turnstile_blocks_start() {
        let google = mock_google("s", "e@x.com", true).await;
        let turnstile = mock_turnstile().await;
        let service = test_service(SignupMode::Open, &google, &turnstile).await;
        let base = serve(service).await;

        let start = client()
            .post(format!("{base}/signup/start"))
            .form(&[("cf-turnstile-response", "wrong")])
            .send()
            .await
            .unwrap();
        assert_eq!(start.status(), 400);
        assert!(start.text().await.unwrap().contains("Verification failed"));
    }

    #[tokio::test]
    async fn state_mismatch_is_rejected() {
        let google = mock_google("sub-x", "x@x.com", true).await;
        let turnstile = mock_turnstile().await;
        let service = test_service(SignupMode::Open, &google, &turnstile).await;
        let base = serve(service.clone()).await;
        let http = client();

        let start = http
            .post(format!("{base}/signup/start"))
            .form(&[("cf-turnstile-response", "pass")])
            .send()
            .await
            .unwrap();
        let oauth_cookie = cookie_from(&start, OAUTH_COOKIE).unwrap();

        let callback = http
            .get(format!(
                "{base}/signup/callback?code=whatever&state=forged-state"
            ))
            .header(header::COOKIE, format!("{OAUTH_COOKIE}={oauth_cookie}"))
            .send()
            .await
            .unwrap();
        assert_eq!(callback.status(), 400);
        assert!(
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-x")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn callback_without_cookie_is_rejected() {
        let google = mock_google("sub-y", "y@x.com", true).await;
        let turnstile = mock_turnstile().await;
        let service = test_service(SignupMode::Open, &google, &turnstile).await;
        let base = serve(service).await;

        let callback = client()
            .get(format!("{base}/signup/callback?code=c&state=s"))
            .send()
            .await
            .unwrap();
        assert_eq!(callback.status(), 400);
    }

    #[tokio::test]
    async fn landing_without_session_redirects_to_signup() {
        let google = mock_google("s", "e@x.com", true).await;
        let turnstile = mock_turnstile().await;
        let service = test_service(SignupMode::Open, &google, &turnstile).await;
        let base = serve(service).await;

        let landing = client()
            .get(format!("{base}/signup/keys"))
            .send()
            .await
            .unwrap();
        assert_eq!(landing.status(), 303);
        assert_eq!(landing.headers()[header::LOCATION], "/signup");
    }

    #[tokio::test]
    async fn dashboard_lists_keys_and_issues_new_ones() {
        let (callback, service, base) =
            run_flow(SignupMode::Open, "sub-dash-1", "dash@example.com", true).await;
        let session = cookie_from(&callback, SESSION_COOKIE).unwrap();
        let http = client();
        let session_header = format!("{SESSION_COOKIE}={session}");

        // Dashboard shows the signup key's prefix and the user's email.
        let dash = http
            .get(format!("{base}/signup/keys"))
            .header(header::COOKIE, &session_header)
            .send()
            .await
            .unwrap();
        assert_eq!(dash.status(), 200);
        let body = dash.text().await.unwrap();
        assert!(body.contains("dash@example.com"), "{body}");
        assert!(body.contains("active"), "{body}");

        // Issue a second key: one-time reveal, then two active keys.
        let issue = http
            .post(format!("{base}/signup/keys/issue"))
            .header(header::COOKIE, &session_header)
            .send()
            .await
            .unwrap();
        assert_eq!(issue.status(), 200);
        assert_eq!(issue.headers()[header::CACHE_CONTROL], "no-store");
        assert!(issue.text().await.unwrap().contains("Your app is ready"));

        let user =
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-dash-1")
                .await
                .unwrap()
                .unwrap();
        let app = db::apps::find_app_by_owner(&service.db, &user.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            db::app_admin_keys::count_active_admin_keys(&service.db, &app.id)
                .await
                .unwrap(),
            2
        );

        // Audit trail records the web issuance.
        let events = db::audit_events::list_audit_events_by_app(&service.db, &app.id)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|e| e.event_type == "admin_key.issued_via_web")
        );
    }

    #[tokio::test]
    async fn issue_at_cap_returns_conflict() {
        let (callback, _service, base) =
            run_flow(SignupMode::Open, "sub-dash-2", "cap@example.com", true).await;
        let session = cookie_from(&callback, SESSION_COOKIE).unwrap();
        let http = client();
        let session_header = format!("{SESSION_COOKIE}={session}");

        // Signup issued 1; cap is 5 → 4 more succeed, the 6th conflicts.
        for _ in 0..4 {
            let r = http
                .post(format!("{base}/signup/keys/issue"))
                .header(header::COOKIE, &session_header)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
        }
        let over = http
            .post(format!("{base}/signup/keys/issue"))
            .header(header::COOKIE, &session_header)
            .send()
            .await
            .unwrap();
        assert_eq!(over.status(), 409);
        assert!(over.text().await.unwrap().contains("Key limit reached"));
    }

    #[tokio::test]
    async fn revoke_is_scoped_to_own_app() {
        let (callback, service, base) =
            run_flow(SignupMode::Open, "sub-dash-3", "rev@example.com", true).await;
        let session = cookie_from(&callback, SESSION_COOKIE).unwrap();
        let http = client();
        let session_header = format!("{SESSION_COOKIE}={session}");

        // Another app's key must not be revocable via this session.
        let other_app = db::apps::create_app(&service.db, "other-app", "o@x.com")
            .await
            .unwrap();
        let other_key = app_admin::issue_admin_key(&service.db, &other_app.id, None)
            .await
            .unwrap();
        let forged = http
            .post(format!("{base}/signup/keys/revoke"))
            .header(header::COOKIE, &session_header)
            .form(&[("key_id", other_key.id.as_str())])
            .send()
            .await
            .unwrap();
        assert_eq!(forged.status(), 303);
        assert_eq!(
            db::app_admin_keys::count_active_admin_keys(&service.db, &other_app.id)
                .await
                .unwrap(),
            1,
            "cross-app revoke must be a no-op"
        );

        // Revoking the app's own key works.
        let user =
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-dash-3")
                .await
                .unwrap()
                .unwrap();
        let app = db::apps::find_app_by_owner(&service.db, &user.id)
            .await
            .unwrap()
            .unwrap();
        let own_keys = db::app_admin_keys::list_admin_keys(&service.db, &app.id)
            .await
            .unwrap();
        let own = http
            .post(format!("{base}/signup/keys/revoke"))
            .header(header::COOKIE, &session_header)
            .form(&[("key_id", own_keys[0].id.as_str())])
            .send()
            .await
            .unwrap();
        assert_eq!(own.status(), 303);
        assert_eq!(
            db::app_admin_keys::count_active_admin_keys(&service.db, &app.id)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn logout_kills_the_session() {
        let (callback, _service, base) =
            run_flow(SignupMode::Open, "sub-dash-4", "out@example.com", true).await;
        let session = cookie_from(&callback, SESSION_COOKIE).unwrap();
        let http = client();
        let session_header = format!("{SESSION_COOKIE}={session}");

        let logout = http
            .post(format!("{base}/signup/logout"))
            .header(header::COOKIE, &session_header)
            .send()
            .await
            .unwrap();
        assert_eq!(logout.status(), 303);

        // The old token no longer authenticates.
        let dash = http
            .get(format!("{base}/signup/keys"))
            .header(header::COOKIE, &session_header)
            .send()
            .await
            .unwrap();
        assert_eq!(dash.status(), 303);
        assert_eq!(dash.headers()[header::LOCATION], "/signup");
    }

    #[tokio::test]
    async fn waitlist_files_request_then_reports_in_review() {
        let (callback, service, base) =
            run_flow(SignupMode::Waitlist, "sub-wl-1", "wl@example.com", true).await;

        // First sign-in: request filed, no user created.
        assert_eq!(callback.status(), 200);
        assert!(callback.text().await.unwrap().contains("Request received"));
        let request = db::signup_requests::find_by_identity(
            &service.db,
            IdentityProvider::Google,
            "sub-wl-1",
        )
        .await
        .unwrap()
        .expect("request filed");
        assert_eq!(
            request.status,
            db::signup_requests::SignupRequestStatus::Pending
        );
        assert_eq!(request.email, "wl@example.com");
        assert!(
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-wl-1")
                .await
                .unwrap()
                .is_none()
        );

        // Second sign-in while pending: "still in review", still one request.
        let again = sign_in(&base).await;
        assert_eq!(again.status(), 200);
        assert!(again.text().await.unwrap().contains("Still in review"));
    }

    #[tokio::test]
    async fn waitlist_approval_completes_signup_on_next_sign_in() {
        let (callback, service, base) =
            run_flow(SignupMode::Waitlist, "sub-wl-2", "appr@example.com", true).await;
        assert_eq!(callback.status(), 200);

        let request = db::signup_requests::find_by_identity(
            &service.db,
            IdentityProvider::Google,
            "sub-wl-2",
        )
        .await
        .unwrap()
        .unwrap();
        db::signup_requests::set_status_by_id(
            &service.db,
            &request.id,
            db::signup_requests::SignupRequestStatus::Approved,
            "test-op",
        )
        .await
        .unwrap();

        // Next sign-in completes signup: reveal page, user/app/key exist.
        let approved = sign_in(&base).await;
        assert_eq!(approved.status(), 200);
        assert!(cookie_from(&approved, SESSION_COOKIE).is_some());
        assert!(approved.text().await.unwrap().contains("Your app is ready"));

        let user =
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-wl-2")
                .await
                .unwrap()
                .expect("user created after approval");
        let app = db::apps::find_app_by_owner(&service.db, &user.id)
            .await
            .unwrap()
            .expect("app created");
        assert_eq!(
            db::app_admin_keys::count_active_admin_keys(&service.db, &app.id)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn waitlist_rejection_shows_denial() {
        let (callback, service, base) =
            run_flow(SignupMode::Waitlist, "sub-wl-3", "rej@example.com", true).await;
        assert_eq!(callback.status(), 200);

        db::signup_requests::set_status_by_email(
            &service.db,
            "rej@example.com",
            db::signup_requests::SignupRequestStatus::Rejected,
            "test-op",
        )
        .await
        .unwrap();

        let rejected = sign_in(&base).await;
        assert_eq!(rejected.status(), 403);
        assert!(rejected.text().await.unwrap().contains("Request declined"));
        assert!(
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-wl-3")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn allowed_domain_bypasses_waitlist() {
        let (callback, service, _) = run_flow_with_domains(
            SignupMode::Waitlist,
            &["example.com"],
            "sub-wl-4",
            "insider@example.com",
            true,
        )
        .await;

        // Straight through to signup: reveal page, no request filed.
        assert_eq!(callback.status(), 200);
        assert!(callback.text().await.unwrap().contains("Your app is ready"));
        assert!(
            db::signup_requests::find_by_identity(
                &service.db,
                IdentityProvider::Google,
                "sub-wl-4"
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(
            db::users::find_user_by_identity(&service.db, IdentityProvider::Google, "sub-wl-4")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn domain_matching_is_case_insensitive_and_exact() {
        let domains = vec!["example.com".to_string()];
        assert!(email_domain_allowed(&domains, "a@example.com"));
        assert!(email_domain_allowed(&domains, "a@EXAMPLE.COM"));
        assert!(!email_domain_allowed(&domains, "a@notexample.com"));
        assert!(!email_domain_allowed(&domains, "a@sub.example.com"));
        assert!(!email_domain_allowed(&domains, "no-at-sign"));
        assert!(!email_domain_allowed(&[], "a@example.com"));
    }

    #[test]
    fn html_escaping_covers_the_specials() {
        assert_eq!(
            escape_html(r#"<script>"a"&'b'</script>"#),
            "&lt;script&gt;&quot;a&quot;&amp;&#39;b&#39;&lt;/script&gt;"
        );
    }
}
