pub mod account;
pub mod blobs;
pub mod buckets;

use axum::{Router, routing};

use crate::AppState;

pub fn build_router(state: AppState) -> Router {
    let mut router = Router::new()
        // Account / API keys
        .route("/account/api-keys", routing::post(account::create_api_key))
        .route(
            "/account/api-keys/{key_id}",
            routing::delete(account::revoke_api_key),
        )
        // Stubs
        .route("/account/billing", routing::put(account::update_billing))
        .route("/account/report", routing::get(account::get_report))
        .route("/account/transfer", routing::post(account::transfer))
        // Buckets
        .route(
            "/buckets",
            routing::post(buckets::create_bucket).get(buckets::list_buckets),
        )
        .route(
            "/buckets/{bucket_id}",
            routing::delete(buckets::delete_bucket),
        )
        // Blobs
        .route(
            "/buckets/{bucket_id}/blobs",
            routing::put(blobs::store_blob).get(blobs::list_blobs),
        )
        .route(
            "/blobs/{object_id}",
            routing::get(blobs::read_blob).delete(blobs::delete_blob),
        )
        .route(
            "/blobs/by-blob-id/{blob_id}",
            routing::get(blobs::read_blob_by_blob_id),
        )
        .route(
            "/blobs/{object_id}/metadata",
            routing::patch(blobs::update_blob_metadata),
        );

    if state.config.enable_debug_endpoints {
        router = router.route(
            "/debug/create-account",
            routing::post(account::debug_create_account),
        );
    }

    router.with_state(state)
}
