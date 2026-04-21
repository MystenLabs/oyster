#![allow(missing_docs)]

use std::{path::PathBuf, sync::Arc};

use axum::http::{HeaderName, HeaderValue};
use clap::{Parser, Subcommand};
use oyster::{
    AppId,
    AppState,
    app_auth,
    blob_store::LocalBlobStore,
    config::Config,
    db,
    direct_walrus_store::DirectWalrusBlobStore,
    pearl_client::PearlConnection,
    routes,
};
use tower_http::{cors::CorsLayer, set_header::SetResponseHeaderLayer, trace::TraceLayer};

const X_OYSTER_VERSION: HeaderName = HeaderName::from_static("x-oyster-version");

#[derive(Parser)]
#[command(name = "oysterd", about = "Oyster object storage service")]
struct Cli {
    /// Read PEARL_SERVICE_SECRET from this file instead of the environment.
    #[arg(long, value_name = "PATH")]
    pearl_service_secret_file: Option<PathBuf>,

    /// Read OYSTER_JWT_SECRET from this file instead of the environment.
    #[arg(long, value_name = "PATH")]
    oyster_jwt_secret_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the HTTP API server (default)
    Serve,
    /// Run the blob extension background worker
    Extend,
    /// Manage apps and JWTs.
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
}

#[derive(Subcommand)]
enum AppCommand {
    /// Create a new app and print its UUID.
    New {
        #[arg(long)]
        name: String,
        #[arg(long)]
        contact_email: String,
        #[arg(long)]
        webhook_url: Option<String>,
    },
    /// Generate a 24-hour JWT for an existing app.
    Jwt {
        /// The app ID (UUID).
        app_id: AppId,
    },
    /// List all registered apps.
    List,
    /// Revoke a JWT by adding its JTI to the blacklist.
    RevokeJwt {
        /// The JTI (JWT ID) to revoke.
        jti: String,
    },
}

#[tokio::main]
async fn main() {
    // Walrus SDK pulls in both aws-lc-rs and ring; rustls can't auto-detect.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install default CryptoProvider");
    app_auth::install_crypto_provider();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,s3s::ops=off".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
    let jwt_secret = cli
        .oyster_jwt_secret_file
        .as_ref()
        .map(|p| read_secret_file(p.clone()))
        .or_else(|| std::env::var("OYSTER_JWT_SECRET").ok());

    if let Some(Command::App { command }) = cli.command {
        handle_app_command(command, jwt_secret).await;
        return;
    }

    let overrides = oyster::config::SecretOverrides {
        pearl_service_secret: cli.pearl_service_secret_file.map(read_secret_file),
        oyster_jwt_secret: jwt_secret,
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
        Command::App { .. } => unreachable!("handled above"),
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
                        db.clone(),
                        config.pool_initial_encoded_capacity_bytes,
                        config.pool_initial_epochs_ahead,
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

            let app = routes::build_router(state)
                .layer(axum::middleware::from_fn(
                    oyster::middleware::track_http_metrics,
                ))
                .layer(SetResponseHeaderLayer::if_not_present(
                    X_OYSTER_VERSION,
                    HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
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

            oyster::extension_task::run_extension_loop(
                db,
                pearl_conn,
                rpc_url,
                system_object,
                staking_object,
                ext_config,
            )
            .await;
        }
    }
}

async fn handle_app_command(command: AppCommand, jwt_secret: Option<String>) {
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:oyster.db?mode=rwc".to_string());
    let pool = db::create_pool(&database_url)
        .await
        .expect("failed to create database pool");

    match command {
        AppCommand::New {
            name,
            contact_email,
            webhook_url,
        } => {
            let app = db::apps::create_app(&pool, &name, &contact_email, webhook_url.as_deref())
                .await
                .expect("failed to create app");
            println!("{}", app.id);
        }
        AppCommand::Jwt { app_id } => {
            let secret = jwt_secret.expect("OYSTER_JWT_SECRET required");
            let app = db::apps::get_app(&pool, &app_id)
                .await
                .expect("failed to query app");
            let Some(_app) = app else {
                eprintln!("app not found: {app_id}");
                std::process::exit(1);
            };
            let token = app_auth::sign_jwt(&app_id, &secret).expect("failed to sign JWT");
            println!("{token}");
        }
        AppCommand::List => {
            let apps = db::apps::list_apps(&pool)
                .await
                .expect("failed to list apps");
            println!("ID\tNAME\tCONTACT_EMAIL");
            for app in apps {
                println!("{}\t{}\t{}", app.id, app.name, app.contact_email);
            }
        }
        AppCommand::RevokeJwt { jti } => {
            let _secret = jwt_secret.expect("OYSTER_JWT_SECRET required");
            db::jwt_blacklist::blacklist_jti(&pool, &jti, "2099-01-01 00:00:00")
                .await
                .expect("failed to blacklist JTI");
        }
    }
}

fn read_secret_file(path: PathBuf) -> String {
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read secret file {}: {e}", path.display()))
        .trim_end()
        .to_string()
}
