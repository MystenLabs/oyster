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

    let db = pearl::db::create_pool(&config.database_url)
        .await
        .expect("failed to create database pool");

    tracing::info!("database ready");

    let service = PearlService { db };
    let interceptor = check_service_secret(config.service_secret);
    let svc = PearlServer::with_interceptor(service, interceptor);

    let addr = config.bind_addr.parse().expect("invalid bind address");
    tracing::info!("gRPC server listening on {}", addr);

    Server::builder()
        .add_service(svc)
        .serve(addr)
        .await
        .expect("gRPC server error");
}
