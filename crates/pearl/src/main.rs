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

    if config.sui_rpc_url.is_some() {
        let db_clone = db.clone();
        let config_clone = config.clone();
        tokio::spawn(async move {
            pearl::reconciliation::run_reconciliation_loop(db_clone, config_clone).await;
        });
        tracing::info!("reconciliation task spawned");
    } else {
        tracing::info!("SUI_RPC_URL not set, reconciliation task disabled");
    }

    let service = PearlService {
        db,
        config: config.clone(),
    };
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
