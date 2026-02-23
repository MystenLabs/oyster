use std::sync::Arc;

use oyster::{
    AppState,
    blob_store::LocalBlobStore,
    config::Config,
    db,
    pearl_client::PearlConnection,
    routes,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

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

    let pearl = match &config.pearl_grpc_url {
        Some(url) => {
            tracing::info!("connecting to Pearl at {url}");
            let conn = PearlConnection::connect(url, config.pearl_service_secret.clone())
                .await
                .expect("failed to connect to Pearl");
            tracing::info!("pearl connected");
            Some(conn)
        }
        None => {
            tracing::info!("PEARL_GRPC_URL not set, running in local-only mode");
            None
        }
    };

    let state = AppState {
        db,
        blob_store: Arc::new(blob_store),
        pearl,
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
