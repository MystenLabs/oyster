pub mod auth;
pub mod blob_store;
pub mod config;
pub mod db;
pub mod error;
pub mod models;
pub mod pagination;
pub mod routes;

use blob_store::LocalBlobStore;
use config::Config;

#[derive(Clone)]
pub struct AppState {
    pub db: db::DbPool,
    pub blob_store: LocalBlobStore,
    pub config: Config,
}
