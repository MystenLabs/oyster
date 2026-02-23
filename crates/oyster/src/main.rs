use std::sync::Arc;

use oyster::{
    AppState,
    blob_store::LocalBlobStore,
    config::Config,
    db,
    pearl_client::PearlConnection,
    routes,
    walrus_blob_store::WalrusBlobStore,
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

    let blob_store: Arc<dyn oyster::blob_store::BlobStore> =
        match (&config.walrus_publisher_url, &config.walrus_aggregator_url) {
            (Some(pub_url), Some(agg_url)) => {
                tracing::info!(
                    "using Walrus blob store (publisher={pub_url}, aggregator={agg_url})"
                );
                Arc::new(WalrusBlobStore::new(
                    pub_url.clone(),
                    agg_url.clone(),
                    config.walrus_default_epochs,
                ))
            }
            (Some(_), None) => {
                panic!("WALRUS_PUBLISHER_URL is set but WALRUS_AGGREGATOR_URL is not");
            }
            _ => {
                tracing::info!("using local blob store at {:?}", config.blob_store_path);
                Arc::new(
                    LocalBlobStore::new(config.blob_store_path.clone())
                        .await
                        .expect("failed to initialize blob store"),
                )
            }
        };

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
        blob_store,
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
