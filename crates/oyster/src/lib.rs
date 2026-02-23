pub mod auth;
pub mod blob_store;
pub mod config;
pub mod db;
pub mod error;
pub mod extension_task;
pub mod models;
pub mod pagination;
pub mod pearl_client;
pub mod routes;
pub mod walrus_blob_store;

use std::sync::Arc;

use blob_store::BlobStore;
use config::Config;
use pearl_client::PearlConnection;

#[derive(Clone)]
pub struct AppState {
    pub db: db::DbPool,
    pub blob_store: Arc<dyn BlobStore>,
    pub pearl: Option<PearlConnection>,
    pub config: Config,
}
