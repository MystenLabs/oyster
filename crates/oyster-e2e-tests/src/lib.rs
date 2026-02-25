use std::sync::Arc;

use axum::Router;
use oyster::{
    AppState,
    blob_store::BlobStore,
    config::Config,
    db,
    direct_walrus_store::DirectWalrusBlobStore,
    pearl_client::PearlConnection,
    routes,
};
use pearl::{
    auth::check_service_secret,
    grpc::{PearlService, proto::pearl_server::PearlServer},
};
use tokio::sync::Mutex as TokioMutex;
use walrus_service::test_utils::{
    StorageNodeHandle,
    TestCluster,
    test_cluster::{AggregatorHandle, E2eTestSetupBuilder},
};
use walrus_sui::test_utils::TestClusterHandle;

const PEARL_SECRET: &str = "e2e-test-pearl-secret";

/// Holds all the running components of an Oyster E2E test environment.
pub struct OysterTestHarness {
    /// The Oyster HTTP router, ready for `tower::ServiceExt::oneshot` calls.
    pub router: Router,
    /// Pearl connection for direct gRPC calls if needed.
    pub pearl: PearlConnection,
    /// The operator Pearl account ID (used as the default blob-signing account).
    pub operator_account_id: String,
    /// The operator wallet address on Sui.
    pub operator_address: String,
    /// Handle to the Sui test cluster (for funding wallets, etc.).
    pub sui_cluster: Arc<TokioMutex<TestClusterHandle>>,
    /// The Walrus storage node cluster.
    pub walrus_cluster: TestCluster<StorageNodeHandle>,
    /// The Walrus aggregator handle.
    pub aggregator: AggregatorHandle,
    /// Oyster database pool (for direct DB operations in tests).
    pub db: db::DbPool,
}

impl OysterTestHarness {
    /// Boot the full stack: Sui → Walrus (storage nodes + aggregator) → Pearl → Oyster.
    ///
    /// This is expensive (~10-30s) so tests should share a single harness where possible.
    pub async fn start() -> Self {
        // 1. Boot the Walrus test cluster with an aggregator.
        let (sui_cluster, walrus_cluster, _walrus_client, system_ctx, aggregator) =
            E2eTestSetupBuilder::new()
                .with_aggregator()
                .build()
                .await
                .expect("failed to build walrus e2e test cluster");

        let aggregator =
            aggregator.expect("aggregator should be present (with_aggregator was set)");
        let aggregator_url = aggregator.base_url();

        // Extract Sui RPC URL.
        let rpc_url = {
            let cluster = sui_cluster.lock().await;
            cluster.rpc_url()
        };

        // Extract system/staking object IDs as hex strings.
        let system_object_str = system_ctx.system_object.to_string();
        let staking_object_str = system_ctx.staking_object.to_string();

        // 2. Start Pearl in-process.
        let pearl = start_pearl_in_process().await;

        // 3. Create an operator account in Pearl.
        let create_resp = pearl
            .create_account(0, 0, 0, 0)
            .await
            .expect("failed to create operator Pearl account");
        let operator_account_id = create_resp.account_id;
        let operator_address = create_resp.address;

        // 4. Fund the operator wallet with SUI (needed for gas).
        // Use walrus-sui's sui_types::SuiAddress (v1.66.1) for fund_addresses_with_sui.
        {
            let addr: sui_types::base_types::SuiAddress =
                operator_address.parse().expect("valid SuiAddress");
            let cluster = sui_cluster.lock().await;
            cluster
                .fund_addresses_with_sui(vec![addr], None)
                .await
                .expect("failed to fund operator wallet");
        }

        // 5. Build DirectWalrusBlobStore pointing at the test cluster.
        // Use oyster's re-exported sui_types::ObjectID (v1.65.1) to match DirectWalrusBlobStore.
        let system_object: oyster::sui_types::base_types::ObjectID =
            system_object_str.parse().expect("valid system_object");
        let staking_object: oyster::sui_types::base_types::ObjectID =
            staking_object_str.parse().expect("valid staking_object");

        let blob_store = DirectWalrusBlobStore::new(
            rpc_url,
            aggregator_url,
            system_object,
            staking_object,
            pearl.clone(),
            operator_account_id.clone(),
            1, // 1 epoch for tests
        )
        .await
        .expect("failed to create DirectWalrusBlobStore");

        // 6. Build the Oyster AppState and Router.
        let oyster_db = db::create_pool("sqlite::memory:")
            .await
            .expect("failed to create in-memory oyster db");

        let config = Config {
            bind_addr: "unused".into(),
            database_url: "sqlite::memory:".into(),
            blob_store_path: std::path::PathBuf::from("/tmp/oyster-e2e-unused"),
            enable_debug_endpoints: true,
            pearl_grpc_url: Some("in-process".into()),
            pearl_service_secret: PEARL_SECRET.into(),
            walrus_publisher_url: None,
            walrus_aggregator_url: None,
            walrus_default_epochs: 1,
            sui_rpc_url: None,
            walrus_system_object: None,
            walrus_staking_object: None,
            pearl_account_id: Some(operator_account_id.clone()),
            blob_extend_interval_secs: 3600,
            blob_extend_lookahead_days: 7,
            blob_extend_epochs: 1,
        };

        let state = AppState {
            db: oyster_db.clone(),
            blob_store: Arc::new(blob_store) as Arc<dyn BlobStore>,
            pearl: Some(pearl.clone()),
            config,
        };

        let router = routes::build_router(state);

        Self {
            router,
            pearl,
            operator_account_id,
            operator_address,
            sui_cluster,
            walrus_cluster,
            aggregator,
            db: oyster_db,
        }
    }

    /// Fund an address with SUI from the test cluster's admin wallet.
    pub async fn fund_address(&self, address: &str) {
        let addr: sui_types::base_types::SuiAddress = address.parse().expect("valid SuiAddress");
        let cluster = self.sui_cluster.lock().await;
        cluster
            .fund_addresses_with_sui(vec![addr], None)
            .await
            .expect("failed to fund address");
    }
}

/// Start Pearl's gRPC server in-process on a random port and return a connected PearlConnection.
async fn start_pearl_in_process() -> PearlConnection {
    let db = pearl::db::create_pool("sqlite::memory:")
        .await
        .expect("in-memory pearl pool");

    let service = PearlService { db };
    let interceptor = check_service_secret(PEARL_SECRET.to_string());
    let svc = PearlServer::with_interceptor(service, interceptor);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(svc)
            .serve_with_incoming(incoming)
            .await
            .expect("pearl server error");
    });

    let url = format!("http://{addr}");
    for _ in 0..20 {
        if let Ok(conn) = PearlConnection::connect(&url, PEARL_SECRET.to_string()).await {
            return conn;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("could not connect to Pearl gRPC server at {url}");
}
