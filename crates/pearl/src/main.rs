#![allow(missing_docs)]

use std::path::PathBuf;

use clap::Parser;
use pearl::{
    auth::check_service_secret,
    config::Config,
    grpc::{PearlService, proto::pearl_server::PearlServer},
};
use tonic::transport::Server;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(name = "pearl", about = "Pearl wallet custody service")]
struct Cli {
    /// Read PEARL_SERVICE_SECRET from this file instead of the environment.
    #[arg(long, value_name = "PATH")]
    pearl_service_secret_file: Option<PathBuf>,

    /// Read PEARL_MASTER_SEED from this file instead of the environment.
    #[arg(long, value_name = "PATH")]
    pearl_master_seed_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();
    tracing::info!("pearl starting up");

    let cli = Cli::parse();
    let overrides = pearl::config::SecretOverrides {
        service_secret: cli.pearl_service_secret_file.map(read_secret_file),
        master_seed_hex: cli
            .pearl_master_seed_file
            .map(|p| Zeroizing::new(read_secret_file(p))),
    };
    let config = Config::new(overrides);
    tracing::info!("pearl starting on {}", config.bind_addr);

    let metrics_handle = pearl::metrics::setup();

    tokio::spawn(serve_metrics(
        metrics_handle,
        config.metrics_bind_addr.clone(),
    ));

    let service = PearlService {
        config: config.clone(),
    };
    let interceptor = check_service_secret(config.service_secret);
    let svc = PearlServer::with_interceptor(service, interceptor);

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<PearlServer<PearlService>>()
        .await;

    let addr = config.bind_addr.parse().expect("invalid bind address");
    tracing::info!("gRPC server listening on {}", addr);

    let mut builder = Server::builder();

    if let (Some(cert_path), Some(key_path)) = (&config.tls_cert_path, &config.tls_key_path) {
        let cert_pem = std::fs::read(cert_path)
            .unwrap_or_else(|e| panic!("failed to read TLS cert at {cert_path}: {e}"));
        let key_pem = std::fs::read(key_path)
            .unwrap_or_else(|e| panic!("failed to read TLS key at {key_path}: {e}"));
        let identity = tonic::transport::Identity::from_pem(cert_pem, key_pem);
        let tls_config = tonic::transport::ServerTlsConfig::new().identity(identity);
        builder = builder
            .tls_config(tls_config)
            .expect("invalid TLS configuration");
        tracing::info!("TLS enabled (cert={cert_path}, key={key_path})");
    } else {
        tracing::info!("TLS not configured, serving plaintext");
    }

    builder
        .add_service(health_service)
        .add_service(svc)
        .serve(addr)
        .await
        .expect("gRPC server error");
}

fn read_secret_file(path: PathBuf) -> String {
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read secret file {}: {e}", path.display()))
        .trim_end()
        .to_string()
}

async fn serve_metrics(handle: metrics_exporter_prometheus::PrometheusHandle, bind_addr: String) {
    let app = axum::Router::new().route(
        "/metrics",
        axum::routing::get(move || {
            let handle = handle.clone();
            async move { handle.render() }
        }),
    );

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind metrics server to {bind_addr}: {e}"));
    tracing::info!("metrics server listening on {bind_addr}");
    axum::serve(listener, app)
        .await
        .expect("metrics server error");
}
