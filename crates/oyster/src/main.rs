#![allow(missing_docs)]

use std::{path::PathBuf, sync::Arc};

use clap::{Parser, Subcommand};
use oyster::{
    AppState,
    blob_store::LocalBlobStore,
    config::Config,
    db,
    direct_walrus_store::DirectWalrusBlobStore,
    pearl_client::PearlConnection,
    routes,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

#[derive(Parser)]
#[command(name = "oysterd", about = "Oyster object storage service")]
struct Cli {
    /// Read PEARL_SERVICE_SECRET from this file instead of the environment.
    #[arg(long, value_name = "PATH")]
    pearl_service_secret_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the HTTP API server (default)
    Serve,
    /// Run the blob extension background worker
    Extend,
}

#[tokio::main]
async fn main() {
    // Walrus SDK pulls in both aws-lc-rs and ring; rustls can't auto-detect.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install default CryptoProvider");

    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let overrides = oyster::config::SecretOverrides {
        pearl_service_secret: cli.pearl_service_secret_file.map(read_secret_file),
    };
    let config = Config::new(overrides);

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

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            tracing::info!("starting oyster server on {}", config.bind_addr);

            let blob_store: Arc<dyn oyster::blob_store::BlobStore> = if let (
                Some(pearl_conn),
                Some(rpc_url),
                Some(sys_obj),
                Some(stk_obj),
                Some(agg_url),
            ) = (
                &pearl,
                &config.sui_rpc_url,
                &config.walrus_system_object,
                &config.walrus_staking_object,
                &config.walrus_aggregator_url,
            ) {
                use sui_types::base_types::ObjectID;
                let system_object: ObjectID =
                    sys_obj.parse().expect("invalid WALRUS_SYSTEM_OBJECT");
                let staking_object: ObjectID =
                    stk_obj.parse().expect("invalid WALRUS_STAKING_OBJECT");
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
                tracing::info!("using local blob store at {:?}", config.blob_store_path);
                Arc::new(
                    LocalBlobStore::new(config.blob_store_path.clone())
                        .await
                        .expect("failed to initialize blob store"),
                )
            };

            let metrics_handle = oyster::metrics::setup();

            let state = AppState {
                db,
                blob_store,
                pearl,
                config: config.clone(),
                metrics_handle: Some(metrics_handle),
            };

            if let Some(ref s3_addr) = config.s3_bind_addr {
                let s3_state = state.clone();
                let s3_addr = s3_addr.clone();
                tokio::spawn(async move { oyster::s3::serve_s3(s3_state, s3_addr).await });
            }

            let app = routes::build_router(state)
                .layer(axum::middleware::from_fn(
                    oyster::middleware::track_http_metrics,
                ))
                .layer(CorsLayer::permissive())
                .layer(TraceLayer::new_for_http());

            let listener = tokio::net::TcpListener::bind(&config.bind_addr)
                .await
                .expect("failed to bind");

            tracing::info!("listening on {}", config.bind_addr);
            axum::serve(listener, app).await.expect("server error");
        }
        Command::Extend => {
            tracing::info!("starting oyster extension worker");

            let metrics_handle = oyster::metrics::setup();
            tokio::spawn(oyster::metrics::serve_metrics(
                metrics_handle,
                config.extension_metrics_bind_addr.clone(),
            ));

            let pearl_conn = pearl.expect("PEARL_GRPC_URL is required for the extend worker");
            let rpc_url = config
                .sui_rpc_url
                .clone()
                .expect("SUI_RPC_URL is required for the extend worker");
            let sys_obj = config
                .walrus_system_object
                .as_ref()
                .expect("WALRUS_SYSTEM_OBJECT is required for the extend worker");
            let stk_obj = config
                .walrus_staking_object
                .as_ref()
                .expect("WALRUS_STAKING_OBJECT is required for the extend worker");

            use sui_types::base_types::ObjectID;
            let system_object: ObjectID = sys_obj.parse().expect("invalid WALRUS_SYSTEM_OBJECT");
            let staking_object: ObjectID = stk_obj.parse().expect("invalid WALRUS_STAKING_OBJECT");

            let ext_config = oyster::extension_task::ExtensionConfig {
                check_interval: std::time::Duration::from_secs(config.blob_extend_interval_secs),
                lookahead_days: config.blob_extend_lookahead_days,
                extend_epochs: config.blob_extend_epochs,
            };

            let webhook_client = config
                .fund_manager_webhook_url
                .map(oyster::webhook::WebhookClient::new);

            oyster::extension_task::run_extension_loop(
                db,
                pearl_conn,
                rpc_url,
                system_object,
                staking_object,
                ext_config,
                webhook_client,
            )
            .await;
        }
    }
}

fn read_secret_file(path: PathBuf) -> String {
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read secret file {}: {e}", path.display()))
        .trim_end()
        .to_string()
}
