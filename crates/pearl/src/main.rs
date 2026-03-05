#![allow(missing_docs)]

use pearl::{
    auth::check_service_secret,
    config::Config,
    grpc::{PearlService, proto::pearl_server::PearlServer},
};
use tonic::transport::Server;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = Config::from_env();
    tracing::info!("pearl starting on {}", config.bind_addr);

    let metrics_handle = pearl::metrics::setup();

    let db = pearl::db::create_pool(&config.database_url)
        .await
        .expect("failed to create database pool");

    tracing::info!("database ready");

    tokio::spawn(serve_metrics(
        metrics_handle,
        db.clone(),
        config.metrics_bind_addr.clone(),
    ));

    let service = PearlService {
        db,
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

async fn serve_metrics(
    handle: metrics_exporter_prometheus::PrometheusHandle,
    db: pearl::db::DbPool,
    bind_addr: String,
) {
    let app = axum::Router::new().route(
        "/metrics",
        axum::routing::get(move || {
            let handle = handle.clone();
            let db = db.clone();
            async move {
                if let Ok(count) = pearl::db::accounts::count_accounts(&db).await {
                    ::metrics::gauge!(pearl::metrics::ACCOUNTS_TOTAL).set(count as f64);
                }
                handle.render()
            }
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
