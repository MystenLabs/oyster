/// Account and API key management endpoints.
pub mod account;
/// Blob storage and retrieval endpoints.
pub mod blobs;
/// Bucket CRUD endpoints.
pub mod buckets;

use axum::Router;
use utoipa::{
    Modify,
    OpenApi,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
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
        (name = "Debug", description = "Debug endpoints (development only)"),
    ),
    modifiers(&SecurityAddon),
)]
struct ApiDoc;

struct SecurityAddon;

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

/// Build the Axum router with all API routes and OpenAPI docs.
pub fn build_router(state: AppState) -> Router {
    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        // Account / API keys
        .routes(routes!(account::create_api_key))
        .routes(routes!(account::revoke_api_key))
        .routes(routes!(account::get_wallets))
        // Stubs
        .routes(routes!(account::update_billing))
        .routes(routes!(account::get_report))
        .routes(routes!(account::transfer))
        // Buckets
        .routes(routes!(buckets::create_bucket, buckets::list_buckets))
        .routes(routes!(buckets::delete_bucket))
        // Blobs
        .routes(routes!(blobs::store_blob, blobs::list_blobs))
        .routes(routes!(blobs::read_blob, blobs::delete_blob))
        .routes(routes!(blobs::read_blob_by_blob_id))
        .routes(routes!(blobs::update_blob_metadata))
        .split_for_parts();

    let mut router = router;

    if state.config.enable_debug_endpoints {
        let (debug_router, _debug_api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
            .routes(routes!(account::debug_create_account))
            .split_for_parts();
        router = router.merge(debug_router);
    }

    router
        .merge(Scalar::with_url("/docs", api))
        .with_state(state)
}
