use std::{collections::BTreeSet, sync::Arc};

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
use sui_sdk::SuiClientBuilder;
use sui_types::{
    base_types::SuiAddress,
    programmable_transaction_builder::ProgrammableTransactionBuilder,
    transaction::TransactionData,
};
use tokio::sync::Mutex as TokioMutex;
use walrus_service::test_utils::{
    StorageNodeHandle,
    TestCluster,
    test_cluster::{AggregatorHandle, E2eTestSetupBuilder},
};
use walrus_sui::{client::SuiContractClient, test_utils::TestClusterHandle};
use walrus_test_utils::WithTempDir;

const PEARL_SECRET: &str = "e2e-test-pearl-secret";
const GAS_BUDGET: u64 = 50_000_000;
const WAL_FUND_AMOUNT: u64 = 500_000_000_000; // 500 WAL in FROST

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
    /// Keep the walrus admin client alive (its temp_dir holds the admin wallet config).
    _walrus_client: WithTempDir<walrus_sdk::node_client::WalrusNodeClient<SuiContractClient>>,
}

impl OysterTestHarness {
    /// Boot the full stack: Sui → Walrus (storage nodes + aggregator) → Pearl → Oyster.
    ///
    /// This is expensive (~10-30s) so tests should share a single harness where possible.
    pub async fn start() -> Self {
        // 1. Boot the Walrus test cluster with an aggregator.
        let (sui_cluster, walrus_cluster, walrus_client, system_ctx, aggregator) =
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
        let operator_sui_addr: SuiAddress = operator_address.parse().expect("valid SuiAddress");
        {
            let cluster = sui_cluster.lock().await;
            cluster
                .fund_addresses_with_sui(vec![operator_sui_addr], None)
                .await
                .expect("failed to fund operator wallet with SUI");
        }

        // 5. Fund the operator wallet with WAL (needed for reserve_space).
        // The admin wallet (inside walrus_client) has WAL from contract deployment.
        // Load it from the temp_dir, find WAL coins, and transfer to the operator.
        fund_with_wal(&walrus_client, &rpc_url, operator_sui_addr, WAL_FUND_AMOUNT).await;

        // 6. Build DirectWalrusBlobStore pointing at the test cluster.
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

        // 7. Build the Oyster AppState and Router.
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
            _walrus_client: walrus_client,
        }
    }

    /// Fund an address with SUI from the test cluster's admin wallet.
    pub async fn fund_address(&self, address: &str) {
        let addr: SuiAddress = address.parse().expect("valid SuiAddress");
        let cluster = self.sui_cluster.lock().await;
        cluster
            .fund_addresses_with_sui(vec![addr], None)
            .await
            .expect("failed to fund address");
    }
}

/// Transfer WAL from the admin wallet (inside walrus_client) to the given recipient.
///
/// The admin wallet has WAL from deploying the Walrus contracts during test setup.
/// We load it from the temp_dir's wallet config, find WAL coins, and send them.
async fn fund_with_wal(
    walrus_client: &WithTempDir<walrus_sdk::node_client::WalrusNodeClient<SuiContractClient>>,
    rpc_url: &str,
    recipient: SuiAddress,
    amount: u64,
) {
    let wallet_config_path = walrus_client.temp_dir.path().join("wallet_config.yaml");
    #[allow(deprecated)]
    let wallet =
        walrus_sui::config::load_wallet_context_from_path(Some(&wallet_config_path), None)
            .expect("load admin wallet from walrus client temp_dir");

    let sender = wallet.active_address();

    // Query all coins to find the WAL coin type (it's the non-SUI coin).
    let sui_client = SuiClientBuilder::default()
        .build(rpc_url)
        .await
        .expect("build SuiClient for WAL funding");

    let all_balances = sui_client
        .coin_read_api()
        .get_all_balances(sender)
        .await
        .expect("query admin balances");

    let wal_balance = all_balances
        .iter()
        .find(|b| b.coin_type != "0x2::sui::SUI")
        .unwrap_or_else(|| {
            panic!(
                "admin wallet {sender} has no non-SUI coins (expected WAL). Balances: {all_balances:?}"
            )
        });
    let wal_coin_type = wal_balance.coin_type.clone();

    tracing::info!(
        "funding {recipient} with {amount} WAL (type: {wal_coin_type}) from admin {sender}"
    );

    // Get specific WAL coins.
    let coins = sui_client
        .coin_read_api()
        .get_coins(sender, Some(wal_coin_type), None, None)
        .await
        .expect("query admin WAL coins");

    let coin = coins
        .data
        .first()
        .expect("admin should have at least one WAL coin");

    assert!(
        coin.balance >= amount,
        "admin WAL coin balance ({}) < requested amount ({amount})",
        coin.balance
    );

    // Build a PAY transaction to transfer WAL.
    let coin_ref = coin.object_ref();
    let mut ptb = ProgrammableTransactionBuilder::new();
    ptb.pay(vec![coin_ref], vec![recipient], vec![amount])
        .expect("build pay tx");

    #[allow(deprecated)]
    let gas_obj = wallet
        .gas_for_owner_budget(sender, GAS_BUDGET, BTreeSet::new())
        .await
        .expect("find gas coin for WAL transfer");
    let gas_ref = gas_obj.1.compute_object_reference();

    #[allow(deprecated)]
    let gas_price = wallet
        .get_reference_gas_price()
        .await
        .expect("get gas price");

    let tx_data = TransactionData::new_programmable(
        sender,
        vec![gas_ref],
        ptb.finish(),
        GAS_BUDGET,
        gas_price,
    );

    let signed = wallet.sign_transaction(&tx_data).await;
    #[allow(deprecated)]
    wallet
        .execute_transaction_may_fail(signed)
        .await
        .expect("execute WAL transfer to operator");

    tracing::info!("funded {recipient} with {amount} WAL");
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
