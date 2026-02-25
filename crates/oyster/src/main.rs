use std::sync::Arc;

use oyster::{
    AppState,
    blob_store::LocalBlobStore,
    config::Config,
    db,
    direct_walrus_store::DirectWalrusBlobStore,
    pearl_client::PearlConnection,
    routes,
    walrus_blob_store::WalrusBlobStore,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

#[tokio::main]
async fn main() {
    // Walrus SDK pulls in both aws-lc-rs and ring; rustls can't auto-detect.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install default CryptoProvider");

    tracing_subscriber::fmt::init();

    let config = Config::from_env();
    tracing::info!("starting oyster on {}", config.bind_addr);

    let db = db::create_pool(&config.database_url)
        .await
        .expect("failed to create database pool");

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

    let blob_store: Arc<dyn oyster::blob_store::BlobStore> =
        if let (Some(pearl_conn), Some(rpc_url), Some(sys_obj), Some(stk_obj), Some(agg_url)) = (
            &pearl,
            &config.sui_rpc_url,
            &config.walrus_system_object,
            &config.walrus_staking_object,
            &config.walrus_aggregator_url,
        ) {
            use sui_types::base_types::ObjectID;
            let system_object: ObjectID = sys_obj.parse().expect("invalid WALRUS_SYSTEM_OBJECT");
            let staking_object: ObjectID = stk_obj.parse().expect("invalid WALRUS_STAKING_OBJECT");
            tracing::info!(
                "using direct Walrus blob store (aggregator={agg_url}, sui_rpc_url={rpc_url})"
            );
            Arc::new(
                DirectWalrusBlobStore::new(
                    rpc_url.clone(),
                    agg_url.clone(),
                    system_object,
                    staking_object,
                    pearl_conn.clone(),
                    config.walrus_default_epochs,
                )
                .await
                .expect("failed to initialize direct Walrus blob store"),
            )
        } else {
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
            }
        };

    // Spawn blob extension task if Pearl + Sui RPC + Walrus config are all present.
    if let (Some(pearl_conn), Some(rpc_url), Some(sys_obj), Some(stk_obj)) = (
        &pearl,
        &config.sui_rpc_url,
        &config.walrus_system_object,
        &config.walrus_staking_object,
    ) {
        use sui_types::base_types::ObjectID;
        let system_object: ObjectID = sys_obj.parse().expect("invalid WALRUS_SYSTEM_OBJECT");
        let staking_object: ObjectID = stk_obj.parse().expect("invalid WALRUS_STAKING_OBJECT");
        let ext_config = oyster::extension_task::ExtensionConfig {
            check_interval: std::time::Duration::from_secs(config.blob_extend_interval_secs),
            lookahead_days: config.blob_extend_lookahead_days,
            extend_epochs: config.blob_extend_epochs,
        };
        tracing::info!("spawning blob extension background task");
        tokio::spawn(oyster::extension_task::run_extension_loop(
            db.clone(),
            pearl_conn.clone(),
            rpc_url.clone(),
            system_object,
            staking_object,
            ext_config,
        ));
    }

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
