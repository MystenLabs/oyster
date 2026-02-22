use pearl::config::Config;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = Config::from_env();
    tracing::info!("pearl starting on {}", config.bind_addr);

    let _db = pearl::db::create_pool(&config.database_url)
        .await
        .expect("failed to create database pool");

    tracing::info!("database ready");

    // gRPC server will be added in Phase 3.
}
