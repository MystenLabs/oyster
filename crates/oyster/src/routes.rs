/// Account and API key management endpoints.
pub mod account;
/// Admin API routes authenticated via per-app admin keys.
pub mod admin;
/// Blob storage and retrieval endpoints.
pub mod blobs;
/// Bucket CRUD endpoints.
pub mod buckets;
/// Health and readiness probe endpoints.
pub mod health;
/// Prometheus metrics endpoint.
pub mod metrics;

use axum::{Router, extract::DefaultBodyLimit};
use utoipa::{
    Modify, OpenApi,
    openapi::{
        security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
        server::Server,
    },
};
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_scalar::{Scalar, Servable};

use crate::AppState;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Oyster Object Storage API",
        version = "0.1.0",
        description = "A content-addressed object storage API with buckets, blobs, and API key authentication."
    ),
    tags(
        (name = "Account", description = "Account and API key management"),
        (name = "Buckets", description = "Bucket CRUD operations"),
        (name = "Blobs", description = "Blob storage and retrieval"),
        (name = "Health", description = "Health and readiness probes"),
        (name = "Admin", description = "Admin endpoints (admin-key authenticated)"),
    ),
    modifiers(&SecurityAddon, &ServerAddon),
)]
struct ApiDoc;

struct SecurityAddon;

struct ServerAddon;

impl Modify for ServerAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi.servers = Some(vec![Server::new("/api/v1")]);
    }
}

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_default();
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("API Key")
                    .build(),
            ),
        );
    }
}

/// Build the Axum router with all API routes under `/api/v1`, S3-compatible API at the root
/// (as a fallback), and infrastructure routes (`/health`, `/ready`, `/api/docs`, `/metrics`) at root.
pub fn build_router(state: AppState) -> Router {
    let s3_service = crate::s3::build_s3_service(&state);
    let s3_fallback = move |req: axum::extract::Request| {
        let svc = s3_service.clone();
        async move {
            let path = req.uri().path();
            // JSON API and infrastructure paths should never fall through to S3 —
            // an unmatched /api/v1/... URL is a real 404, not an S3 bucket name.
            if path.starts_with("/api/") {
                tracing::info!(method = %req.method(), %path, "unmatched api path");
                return axum::http::Response::builder()
                    .status(axum::http::StatusCode::NOT_FOUND)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"error":"not found"}"#))
                    .unwrap();
            }
            tracing::info!(method = %req.method(), %path, "s3 request");
            let req = req.map(s3s::Body::http_body_unsync);
            match svc.call(req).await {
                Ok(resp) => resp.map(axum::body::Body::new),
                Err(err) => {
                    tracing::error!(?err, "S3 service error");
                    axum::http::Response::builder()
                        .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                        .body(axum::body::Body::from("Internal Server Error"))
                        .unwrap()
                }
            }
        }
    };

    let (api_router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        // Admin (admin-key authenticated)
        .routes(routes!(admin::create_account, admin::list_accounts))
        .routes(routes!(
            admin::admin_create_api_key,
            admin::list_api_keys_for_account
        ))
        .routes(routes!(admin::admin_revoke_api_key))
        .routes(routes!(
            admin::admin_create_access_key,
            admin::admin_list_access_keys
        ))
        .routes(routes!(admin::admin_delete_access_key))
        .routes(routes!(admin::get_app))
        .routes(routes!(admin::set_webhook_url, admin::clear_webhook_url))
        .routes(routes!(admin::update_max_storage))
        // Account
        .routes(routes!(account::get_wallet))
        // Stubs
        .routes(routes!(account::update_billing))
        .routes(routes!(account::get_report))
        .routes(routes!(account::transfer))
        // Buckets
        .routes(routes!(buckets::create_bucket, buckets::list_buckets))
        .routes(routes!(buckets::delete_bucket))
        // Blobs
        .routes(routes!(blobs::list_blobs))
        .merge(
            OpenApiRouter::new()
                .routes(routes!(blobs::store_blob))
                .layer(DefaultBodyLimit::max(blobs::MAX_BLOB_SIZE)),
        )
        .routes(routes!(blobs::read_blob, blobs::delete_blob))
        .routes(routes!(blobs::update_blob_metadata))
        .routes(routes!(blobs::read_blob_by_blob_id))
        .routes(routes!(
            blobs::list_blob_tags,
            blobs::replace_blob_tags,
            blobs::patch_blob_tags,
            blobs::clear_blob_tags
        ))
        .routes(routes!(blobs::put_blob_tag, blobs::delete_blob_tag))
        .split_for_parts();

    // Self-serve signup pages, mounted only when configured. These
    // carry their own state and stay out of the OpenAPI spec.
    let signup_router = match &state.config.signup {
        Some(signup_config) => {
            tracing::info!(mode = ?signup_config.mode, "signup enabled at /signup");
            crate::signup::routes::build_signup_router(
                state.db.clone(),
                signup_config.clone(),
                state.config.max_admin_keys_per_app,
            )
        }
        None => Router::new(),
    };

    Router::new()
        .nest("/api/v1", api_router)
        .route("/health", axum::routing::get(health::health))
        .route("/ready", axum::routing::get(health::ready))
        .merge(Scalar::with_url("/api/docs", api))
        .route("/metrics", axum::routing::get(metrics::metrics))
        .fallback(s3_fallback)
        .with_state(state)
        // Merged after `with_state`: the signup router carries its own
        // `SignupService` state. Explicit routes win over the fallback.
        .merge(signup_router)
}
