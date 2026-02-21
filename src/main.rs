mod auth;
mod blob_store;
mod config;
mod db;
mod error;
mod models;
mod pagination;
mod routes;

use blob_store::LocalBlobStore;
use config::Config;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

#[derive(Clone)]
pub struct AppState {
    pub db: db::DbPool,
    pub blob_store: LocalBlobStore,
    pub config: Config,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = Config::from_env();
    tracing::info!("starting oyster on {}", config.bind_addr);

    let db = db::create_pool(&config.database_url)
        .await
        .expect("failed to create database pool");

    let blob_store = LocalBlobStore::new(config.blob_store_path.clone())
        .await
        .expect("failed to initialize blob store");

    let state = AppState {
        db,
        blob_store,
        config: config.clone(),
    };

    let app = routes::build_router(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(&config.bind_addr)
        .await
        .expect("failed to bind");

    tracing::info!("listening on {}", config.bind_addr);
    axum::serve(listener, app).await.expect("server error");
}
