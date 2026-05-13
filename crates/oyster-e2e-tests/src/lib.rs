//! End-to-end test harness for Oyster, spinning up Sui, Walrus, Pearl, and Oyster in-process.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

/// Global mutex that serialises all E2E tests.
///
/// The Walrus `E2eTestSetupBuilder` reuses a `OnceLock`-based singleton Sui
/// cluster, so concurrent tests compete for the same faucet wallet and hit
/// object-version conflicts.  Holding this lock inside [`run_e2e`] ensures
/// only one test touches the cluster at a time, regardless of
/// `--test-threads`.
static E2E_LOCK: Mutex<()> = Mutex::new(());

/// Run an async E2E test body on a tokio runtime with 32 MiB worker stacks.
///
/// The walrus encoding pipeline is deeply recursive and overflows the default
/// 2 MiB stack, so we build a dedicated runtime with enlarged worker threads.
/// A global mutex ([`E2E_LOCK`]) serialises execution so parallel test threads
/// don't race on the shared Sui cluster.
pub fn run_e2e<F: std::future::Future<Output = ()> + Send + 'static>(f: F) {
    // The lock guards no shared state — it only serialises harness builds.
    // Recover from poison so a panic in one test (e.g., transient
    // `git ls-remote` failure inside `E2eTestSetupBuilder::build`) does not
    // cascade-fail every subsequent test with `PoisonError`.
    let _guard = E2E_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(32 * 1024 * 1024)
        .build()
        .expect("build tokio runtime");
    rt.block_on(async {
        // Spawn onto a worker thread with the enlarged stack.
        tokio::spawn(f).await.expect("e2e test panicked");
    });
}

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
use sui_sdk::{SuiClientBuilder, rpc_types::SuiTransactionBlockResponseOptions};
use sui_types::{
    base_types::SuiAddress,
    programmable_transaction_builder::ProgrammableTransactionBuilder,
    transaction::TransactionData,
    transaction_driver_types::ExecuteTransactionRequestType,
};
use tokio::sync::Mutex as TokioMutex;
use walrus_service::test_utils::{
    StorageNodeHandle,
    TestCluster,
    test_cluster::E2eTestSetupBuilder,
};
use walrus_sui::{
    client::SuiContractClient,
    test_utils::TestClusterHandle,
    utils::BYTES_PER_UNIT_SIZE,
};
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
    pub operator_account_id: oyster::AccountId,
    /// The operator wallet address on Sui.
    pub operator_address: String,
    /// Handle to the Sui test cluster (for funding wallets, etc.).
    pub sui_cluster: Arc<TokioMutex<TestClusterHandle>>,
    /// The Walrus storage node cluster.
    pub walrus_cluster: TestCluster<StorageNodeHandle>,
    /// Oyster database pool (for direct DB operations in tests).
    pub db: db::DbPool,
    /// Sui RPC URL for the test cluster.
    pub rpc_url: String,
    /// Walrus system object ID.
    pub system_object: oyster::sui_types::base_types::ObjectID,
    /// Walrus staking object ID.
    pub staking_object: oyster::sui_types::base_types::ObjectID,
    /// The walrus admin client (its temp_dir holds the admin wallet config for funding).
    walrus_client: WithTempDir<walrus_sdk::node_client::WalrusNodeClient<SuiContractClient>>,
}

impl OysterTestHarness {
    /// Boot the full stack: Sui → Walrus (storage nodes) → Pearl → Oyster.
    ///
    /// This is expensive (~10-30s) so tests should share a single harness where possible.
    pub async fn start() -> Self {
        // 1. Boot the Walrus test cluster.
        eprintln!("[harness] building walrus e2e test cluster...");
        let (sui_cluster, walrus_cluster, walrus_client, system_ctx, _aggregator) =
            E2eTestSetupBuilder::new()
                .build()
                .await
                .expect("failed to build walrus e2e test cluster");

        eprintln!("[harness] walrus cluster ready");

        // Extract Sui RPC URL.
        let rpc_url = {
            let cluster = sui_cluster.lock().await;
            cluster.rpc_url()
        };
        eprintln!("[harness] sui rpc at {rpc_url}");

        // Extract system/staking object IDs as hex strings.
        let system_object_str = system_ctx.system_object.to_string();
        let staking_object_str = system_ctx.staking_object.to_string();

        // 2. Start Pearl in-process.
        eprintln!("[harness] starting pearl...");
        let pearl = start_pearl_in_process().await;
        eprintln!("[harness] pearl ready");

        // 3. Generate an operator account ID and derive its address from Pearl.
        let operator_account_id = oyster::AccountId::new();
        let operator_address = pearl
            .get_address(&operator_account_id)
            .await
            .expect("failed to get operator Pearl address");
        eprintln!("[harness] operator address: {operator_address}");

        // 4. Fund the operator wallet with SUI (needed for gas).
        eprintln!("[harness] funding operator with SUI...");
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
        eprintln!("[harness] funding operator with WAL...");
        fund_with_wal(&walrus_client, &rpc_url, operator_sui_addr, WAL_FUND_AMOUNT).await;
        eprintln!("[harness] operator funded");

        // 6. Build DirectWalrusBlobStore pointing at the test cluster.
        // Use oyster's re-exported sui_types::ObjectID to match DirectWalrusBlobStore.
        let system_object: oyster::sui_types::base_types::ObjectID =
            system_object_str.parse().expect("valid system_object");
        let staking_object: oyster::sui_types::base_types::ObjectID =
            staking_object_str.parse().expect("valid staking_object");

        // 7. Build the Oyster AppState and Router.
        let oyster_db = db::create_pool("sqlite::memory:")
            .await
            .expect("failed to create in-memory oyster db");

        let blob_store = DirectWalrusBlobStore::new(
            rpc_url.clone(),
            system_object,
            staking_object,
            pearl.clone(),
            oyster_db.clone(),
            BYTES_PER_UNIT_SIZE, // pool_initial_encoded_capacity_bytes
            1,                   // pool_initial_epochs_ahead (1 epoch for tests)
        )
        .await
        .expect("failed to create DirectWalrusBlobStore");

        let config = Config {
            bind_addr: "unused".into(),
            database_url: "sqlite::memory:".into(),
            blob_store_path: std::path::PathBuf::from("/tmp/oyster-e2e-unused"),
            pearl_grpc_url: Some("in-process".into()),
            pearl_service_secret: PEARL_SECRET.into(),

            sui_rpc_url: None,
            walrus_system_object: None,
            walrus_staking_object: None,
            pool_initial_epochs_ahead: 1,
            pool_initial_encoded_capacity_bytes: BYTES_PER_UNIT_SIZE,
            pool_extend_epochs: 1,
            pool_extend_lookahead_epochs: 7,
            extension_idle_sleep_secs: 30,
            extension_busy_sleep_ms: 250,
            extension_claim_batch_size: 100,
            extension_claim_cooldown_secs: 60,
            extension_metrics_bind_addr: "unused".into(),
            allow_http_webhook_scheme: false,
        };

        let state = AppState {
            db: oyster_db.clone(),
            blob_store: Arc::new(blob_store) as Arc<dyn BlobStore>,
            pearl: Some(pearl.clone()),
            config,
            metrics_handle: None,
        };

        let router = routes::build_router(state);
        eprintln!("[harness] oyster router built, harness ready");

        Self {
            router,
            pearl,
            operator_account_id,
            operator_address,
            sui_cluster,
            walrus_cluster,
            db: oyster_db,
            rpc_url,
            system_object,
            staking_object,
            walrus_client,
        }
    }

    /// Access the walrus admin's `SuiContractClient` for read-only on-chain
    /// queries (e.g., `storage_pool_status`).
    pub fn walrus_sui_client(&self) -> &walrus_sui::client::SuiContractClient {
        self.walrus_client.as_ref().sui_client()
    }

    /// Run exactly one extension cycle synchronously, returning the number
    /// of pool rows that were claimed and processed (regardless of outcome).
    pub async fn trigger_extension_cycle(&self, lookahead_epochs: u32, extend_epochs: u32) -> u32 {
        let read_client = oyster::sui_transaction::build_sui_read_client(
            &self.rpc_url,
            self.system_object,
            self.staking_object,
        )
        .await
        .expect("build SuiReadClient");
        let config = oyster::extension_task::ExtensionConfig {
            lookahead_epochs,
            extend_epochs,
            idle_sleep: std::time::Duration::from_secs(30),
            busy_sleep: std::time::Duration::from_millis(250),
            claim_batch_size: 100,
            claim_cooldown: std::time::Duration::from_secs(60),
        };
        oyster::extension_task::run_extension_cycle_once(
            &self.db,
            &self.pearl,
            &self.rpc_url,
            &read_client,
            &config,
        )
        .await
    }

    /// Bind the Oyster HTTP router to a random TCP port and serve it.
    /// Returns the base URL (e.g., "http://127.0.0.1:12345").
    /// Waits until the server is accepting connections before returning.
    pub async fn serve_on_random_port(&self) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind oyster http");
        let addr = listener.local_addr().unwrap();
        let router = self.router.clone();
        tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("oyster http server error");
        });
        let url = format!("http://{addr}");
        eprintln!("[harness] waiting for HTTP server at {url} to be ready...");
        // Wait for the server to be ready.
        for i in 0..50 {
            match reqwest::Client::new()
                .get(format!("{url}/health"))
                .send()
                .await
            {
                Ok(_) => {
                    eprintln!("[harness] HTTP server ready after {i} polls");
                    return url;
                }
                Err(e) => {
                    if i % 10 == 9 {
                        eprintln!("[harness] still waiting for server (attempt {i}): {e}");
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
        panic!("oyster http server at {url} did not become ready");
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

    /// Create an app in the database and issue an admin key for it.
    ///
    /// Returns `(app_id, admin_key)`.
    pub async fn create_app_admin_key(&self, name: &str) -> (String, String) {
        let app = db::apps::create_app(&self.db, name, &format!("{name}@test.example"))
            .await
            .expect("create app");
        let raw = oyster::auth::generate_api_key();
        let hash = oyster::auth::hash_api_key(&raw);
        let prefix = oyster::auth::key_prefix(&raw);
        db::app_admin_keys::create_admin_key(&self.db, &app.id, &hash, &prefix, &raw)
            .await
            .expect("create admin key");
        (app.id.to_string(), raw)
    }

    /// Fund an address with both SUI (for gas) and WAL (for storage).
    ///
    /// This is needed for any wallet that will store blobs on-chain.
    pub async fn fund_wallet(&self, address: &str) {
        let addr: SuiAddress = address.parse().expect("valid SuiAddress");

        // Fund with SUI for gas.
        {
            let cluster = self.sui_cluster.lock().await;
            cluster
                .fund_addresses_with_sui(vec![addr], None)
                .await
                .expect("failed to fund address with SUI");
        }

        // Fund with WAL for storage.
        fund_with_wal(&self.walrus_client, &self.rpc_url, addr, WAL_FUND_AMOUNT).await;
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
    let wallet = walrus_sui::config::load_wallet_context_from_path(Some(&wallet_config_path), None)
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
    sui_client
        .quorum_driver_api()
        .execute_transaction_block(
            signed,
            SuiTransactionBlockResponseOptions::new().with_effects(),
            Some(ExecuteTransactionRequestType::WaitForEffectsCert),
        )
        .await
        .expect("execute WAL transfer to operator");

    // Poll until the recipient's WAL coins are visible on the fullnode.
    // Poll until the recipient's WAL coins are visible on the fullnode.
    // Use a generous timeout (60s) since parallel tests share a single Sui fullnode.
    for _ in 0..300 {
        let balances = sui_client
            .coin_read_api()
            .get_all_balances(recipient)
            .await
            .unwrap_or_default();
        if balances
            .iter()
            .any(|b| b.coin_type != "0x2::sui::SUI" && b.total_balance > 0)
        {
            tracing::info!("funded {recipient} with {amount} WAL");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("WAL coins never became visible on fullnode for {recipient}");
}

/// Start Pearl's gRPC server in-process on a random port and return a connected PearlConnection.
async fn start_pearl_in_process() -> PearlConnection {
    let config = pearl::config::Config {
        bind_addr: "127.0.0.1:0".into(),
        service_secret: PEARL_SECRET.into(),
        master_seed: zeroize::Zeroizing::new(hex::decode("ab".repeat(32)).expect("valid hex seed")),
        tls_cert_path: None,
        tls_key_path: None,
        metrics_bind_addr: "127.0.0.1:0".into(),
    };
    let service = PearlService { config };
    let interceptor = check_service_secret(PEARL_SECRET.to_string());
    let svc = PearlServer::with_interceptor(service, interceptor);

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<PearlServer<PearlService>>()
        .await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(health_service)
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
