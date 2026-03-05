//! Oyster — a content-addressed object storage service backed by Walrus and Sui.

/// API key authentication and account extraction from requests.
pub mod auth;
/// Blob storage trait and local filesystem implementation.
pub mod blob_store;
/// Server configuration loaded from environment variables.
pub mod config;
/// Database access layer (SQLite via sqlx).
pub mod db;
/// Direct Walrus blob store using on-chain transactions.
pub mod direct_walrus_store;
/// Application error types and HTTP response mapping.
pub mod error;
/// Background task that auto-extends expiring blobs.
pub mod extension_task;
/// Prometheus metric constants and recorder setup.
pub mod metrics;
/// Axum middleware for recording HTTP request metrics.
pub mod middleware;
/// API data models for requests, responses, and database rows.
pub mod models;
/// Cursor-based pagination helpers.
pub mod pagination;
/// gRPC client for the Pearl signing service.
pub mod pearl_client;
/// Axum route definitions and OpenAPI generation.
pub mod routes;
/// Sui transaction building, signing, and submission helpers.
pub mod sui_transaction;

use std::sync::Arc;

use blob_store::BlobStore;
use config::Config;
use pearl_client::PearlConnection;
pub use sui_types;

/// Shared application state threaded through all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    /// Database connection pool.
    pub db: db::DbPool,
    /// Blob storage backend.
    pub blob_store: Arc<dyn BlobStore>,
    /// Optional connection to the Pearl signing service.
    pub pearl: Option<PearlConnection>,
    /// Server configuration.
    pub config: Config,
    /// Prometheus metrics handle for rendering scrape output.
    pub metrics_handle: Option<metrics_exporter_prometheus::PrometheusHandle>,
}
