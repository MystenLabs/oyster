#![allow(missing_docs)]

use std::{path::PathBuf, sync::Arc};

use axum::http::{HeaderName, HeaderValue};
use clap::{Parser, Subcommand};
use oyster::{
    AppId,
    AppState,
    auth,
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

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the HTTP API server (default)
    Serve,
    /// Run the blob extension background worker
    Extend,
    /// Manage apps and admin keys.
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
}

#[derive(Subcommand)]
enum AppCommand {
    /// Create a new app, print its UUID, and (unless --no-key) issue and print
    /// a fresh admin key.
    New {
        #[arg(long)]
        name: String,
        #[arg(long)]
        contact_email: String,
        #[arg(long)]
        webhook_url: Option<String>,
        /// Do not auto-issue an initial admin key.
        #[arg(long)]
        no_key: bool,
    },
    /// List all registered apps.
    List,
    /// Issue a new admin key for an existing app. Prints the raw bearer token to stdout.
    IssueAdminKey {
        /// The app ID (UUID).
        app_id: AppId,
    },
    /// List admin keys for an app (TSV: id, prefix, created_at, revoked_at).
    ListAdminKeys {
        /// The app ID (UUID).
        app_id: AppId,
    },
    /// Revoke an admin key by its id.
    RevokeAdminKey {
        /// The admin key id (UUID).
        key_id: String,
    },
}

#[tokio::main]
async fn main() {
    // Walrus SDK pulls in both aws-lc-rs and ring; rustls can't auto-detect.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install default CryptoProvider");

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
                // s3s logs every unsigned/malformed request from internet bot
                // traffic at ERROR. Force-silence the module regardless of
                // RUST_LOG so production (which sets RUST_LOG=info) stays quiet.
                .add_directive("s3s::ops=off".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    if let Some(Command::App { command }) = cli.command {
        handle_app_command(command).await;
        return;
    }

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
                lookahead_epochs: config.pool_extend_lookahead_days,
                extend_epochs: config.pool_extend_epochs,
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

async fn handle_app_command(command: AppCommand) {
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
            no_key,
        } => {
            let app = db::apps::create_app(&pool, &name, &contact_email, webhook_url.as_deref())
                .await
                .expect("failed to create app");
            println!("{}", app.id);
            if !no_key {
                let key = issue_admin_key_for(&pool, &app.id).await;
                println!("{}", key.bearer_token);
            }
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
        AppCommand::IssueAdminKey { app_id } => {
            let app = db::apps::get_app(&pool, &app_id)
                .await
                .expect("failed to query app");
            if app.is_none() {
                eprintln!("app not found: {app_id}");
                std::process::exit(1);
            }
            let key = issue_admin_key_for(&pool, &app_id).await;
            println!("{}", key.bearer_token);
        }
        AppCommand::ListAdminKeys { app_id } => {
            let keys = db::app_admin_keys::list_admin_keys(&pool, &app_id)
                .await
                .expect("failed to list admin keys");
            for k in keys {
                println!(
                    "{}\t{}\t{}\t{}",
                    k.id,
                    k.prefix,
                    k.created_at,
                    k.revoked_at.unwrap_or_default()
                );
            }
        }
        AppCommand::RevokeAdminKey { key_id } => {
            let revoked = db::app_admin_keys::revoke_admin_key(&pool, &key_id)
                .await
                .expect("failed to revoke admin key");
            if !revoked {
                eprintln!("admin key not found or already revoked: {key_id}");
                std::process::exit(1);
            }
        }
    }
}

async fn issue_admin_key_for(
    pool: &db::DbPool,
    app_id: &AppId,
) -> oyster::models::AdminKeyWithBearerToken {
    let raw = auth::generate_api_key();
    let hash = auth::hash_api_key(&raw);
    let prefix = auth::key_prefix(&raw);
    let key = db::app_admin_keys::create_admin_key(pool, app_id, &hash, &prefix, &raw)
        .await
        .expect("failed to create admin key");
    tracing::info!(app_id = %app_id, key_id = %key.id, prefix = %key.prefix, "issued admin key");
    key
}

fn read_secret_file(path: PathBuf) -> String {
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read secret file {}: {e}", path.display()))
        .trim_end()
        .to_string()
}
