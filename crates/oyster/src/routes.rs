/// Account and API key management endpoints.
pub mod account;
/// Blob storage and retrieval endpoints.
pub mod blobs;
/// Bucket CRUD endpoints.
pub mod buckets;
/// Health and readiness probe endpoints.
pub mod health;
/// Prometheus metrics endpoint.
pub mod metrics;

use axum::Router;
use utoipa::{
    Modify,
    OpenApi,
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
        (name = "Debug", description = "Debug endpoints (development only)"),
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
            tracing::info!(method = %req.method(), path = %req.uri().path(), "s3 request");
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
        // Account / API keys
        .routes(routes!(account::create_api_key))
        .routes(routes!(account::revoke_api_key))
        .routes(routes!(account::get_wallet))
        // Access keys
        .routes(routes!(
            account::create_access_key,
            account::list_access_keys
        ))
        .routes(routes!(account::delete_access_key))
        // Stubs
        .routes(routes!(account::update_billing))
        .routes(routes!(account::get_report))
        .routes(routes!(account::transfer))
        // Buckets
        .routes(routes!(buckets::create_bucket, buckets::list_buckets))
        .routes(routes!(buckets::delete_bucket))
        // Blobs
        .routes(routes!(blobs::list_blobs))
        .routes(routes!(
            blobs::store_blob,
            blobs::read_blob,
            blobs::delete_blob
        ))
        .routes(routes!(blobs::update_blob_metadata))
        .routes(routes!(blobs::read_blob_by_blob_id))
        .split_for_parts();

    let mut api_router = api_router;

    if state.config.enable_debug_endpoints {
        let (debug_router, _debug_api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
            .routes(routes!(account::debug_create_account))
            .split_for_parts();
        api_router = api_router.merge(debug_router);
    }

    Router::new()
        .nest("/api/v1", api_router)
        .route("/health", axum::routing::get(health::health))
        .route("/ready", axum::routing::get(health::ready))
        .merge(Scalar::with_url("/api/docs", api))
        .route("/metrics", axum::routing::get(metrics::metrics))
        .fallback(s3_fallback)
        .with_state(state)
}
