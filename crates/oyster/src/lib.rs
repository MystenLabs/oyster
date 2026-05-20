//! Oyster — a content-addressed object storage service backed by Walrus and Sui.

/// Strongly-typed account identifier.
pub mod account_id;
/// Admin-key authentication and app extraction from requests.
pub mod app_admin;
/// Strongly-typed app identifier.
pub mod app_id;
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
/// Helpers for estimating the WAL + SUI cost of extending a `StoragePool`.
pub mod extension_cost;
/// Background task that auto-extends expiring blobs.
pub mod extension_task;
/// Shared WAL/SUI funding-amount type (sync 402 + webhook).
pub mod funding;
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
/// S3-compatible API implementation backed by Oyster's DB and blob store.
pub mod s3;
/// Sui transaction building, signing, and submission helpers.
pub mod sui_transaction;
/// Input validation helpers (e.g. bucket name rules).
pub mod validation;
/// Outbound webhook for fund manager notifications.
pub mod webhook;
/// Helpers for the per-app Ed25519 keypair that signs webhook deliveries.
pub mod webhook_keys;

use std::sync::Arc;

pub use account_id::AccountId;
pub use app_admin::AuthenticatedApp;
pub use app_id::AppId;
use blob_store::BlobStore;
use config::Config;
pub use funding::FundingAmount;
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
