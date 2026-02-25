use fastcrypto::ed25519::Ed25519KeyPair;
use pearl::{
    auth::check_service_secret,
    config::Config,
    grpc::{
        PearlService,
        proto::{self, pearl_client::PearlClient, pearl_server::PearlServer},
    },
};
use sui_types::{
    base_types::{ObjectDigest, ObjectID, SuiAddress},
    crypto::get_key_pair,
    programmable_transaction_builder::ProgrammableTransactionBuilder,
    transaction::TransactionData,
};
use tonic::{Request, transport::Channel};

const TEST_SECRET: &str = "test-secret-42";

fn test_config() -> Config {
    Config {
        database_url: "sqlite::memory:".into(),
        bind_addr: "127.0.0.1:0".into(),
        service_secret: TEST_SECRET.into(),
        sui_rpc_url: None,
        wal_coin_type: None,
        reconciliation_interval_secs: 300,
        pending_tx_timeout_minutes: 30,
    }
}

/// Stand up Pearl's gRPC server in-process on a random port.
/// Returns a connected `PearlClient`.
async fn start_server() -> PearlClient<Channel> {
    let db = pearl::db::create_pool("sqlite::memory:")
        .await
        .expect("in-memory pool");

    let service = PearlService {
        db,
        config: test_config(),
    };
    let interceptor = check_service_secret(TEST_SECRET.to_string());
    let svc = PearlServer::with_interceptor(service, interceptor);

    // Bind to a random port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();

    // Convert tokio listener to a tonic-compatible incoming stream.
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(svc)
            .serve_with_incoming(incoming)
            .await
            .expect("server error");
    });

    // Connect a client.
    let url = format!("http://{addr}");
    // Small retry loop — server may not be listening yet.
    for _ in 0..20 {
        if let Ok(client) = PearlClient::connect(url.clone()).await {
            return client;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("could not connect to gRPC server at {url}");
}

/// Wrap a request with the expected Bearer token.
fn authenticated<T>(msg: T) -> Request<T> {
    let mut req = Request::new(msg);
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {TEST_SECRET}")
            .parse()
            .expect("valid metadata"),
    );
    req
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_account() {
    let mut client = start_server().await;

    let resp = client
        .create_account(authenticated(proto::CreateAccountRequest {
            min_sui_balance: 100,
            min_wal_balance: 200,
            top_up_target_sui: 500,
            top_up_target_wal: 1000,
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(
        !resp.account_id.is_empty(),
        "account_id should be non-empty"
    );
    assert!(
        resp.address.starts_with("0x"),
        "address should start with 0x, got: {}",
        resp.address
    );
}

#[tokio::test]
async fn get_account_wallets() {
    let mut client = start_server().await;

    let create_resp = client
        .create_account(authenticated(proto::CreateAccountRequest {
            min_sui_balance: 100,
            min_wal_balance: 200,
            top_up_target_sui: 500,
            top_up_target_wal: 1000,
        }))
        .await
        .unwrap()
        .into_inner();

    let wallets_resp = client
        .get_account_wallets(authenticated(proto::GetAccountWalletsRequest {
            account_id: create_resp.account_id.clone(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(wallets_resp.wallets.len(), 1);
    let wallet = &wallets_resp.wallets[0];
    assert_eq!(wallet.account_id, create_resp.account_id);
    assert_eq!(wallet.address, create_resp.address);
    assert_eq!(wallet.min_sui_balance, 100);
    assert_eq!(wallet.min_wal_balance, 200);
    assert_eq!(wallet.top_up_target_sui, 500);
    assert_eq!(wallet.top_up_target_wal, 1000);
}

#[tokio::test]
async fn get_wallets_nonexistent_account() {
    let mut client = start_server().await;

    let status = client
        .get_account_wallets(authenticated(proto::GetAccountWalletsRequest {
            account_id: "nonexistent-id".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::NotFound);
}

fn mock_transaction_data(sender: SuiAddress) -> TransactionData {
    let gas_ref = (
        ObjectID::random(),
        sui_types::base_types::SequenceNumber::new(),
        ObjectDigest::random(),
    );
    let pt = ProgrammableTransactionBuilder::new().finish();
    TransactionData::new_programmable(sender, vec![gas_ref], pt, 5_000_000, 1_000)
}

#[tokio::test]
async fn sign_transaction_invalid_tx_data() {
    let mut client = start_server().await;

    let create_resp = client
        .create_account(authenticated(proto::CreateAccountRequest {
            min_sui_balance: 0,
            min_wal_balance: 0,
            top_up_target_sui: 0,
            top_up_target_wal: 0,
        }))
        .await
        .unwrap()
        .into_inner();

    let status = client
        .sign_transaction(authenticated(proto::SignTransactionRequest {
            account_id: create_resp.account_id,
            tx_data: vec![1, 2, 3],
            estimated_sui_cost: 0,
            estimated_wal_cost: 0,
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn sign_transaction_success() {
    let mut client = start_server().await;

    let create_resp = client
        .create_account(authenticated(proto::CreateAccountRequest {
            min_sui_balance: 0,
            min_wal_balance: 0,
            top_up_target_sui: 0,
            top_up_target_wal: 0,
        }))
        .await
        .unwrap()
        .into_inner();

    // Parse the account's address to build a valid TransactionData.
    let sender: SuiAddress = create_resp.address.parse().expect("valid SuiAddress");
    let tx_data = mock_transaction_data(sender);
    let tx_data_bytes = bcs::to_bytes(&tx_data).unwrap();

    let resp = client
        .sign_transaction(authenticated(proto::SignTransactionRequest {
            account_id: create_resp.account_id,
            tx_data: tx_data_bytes,
            estimated_sui_cost: 0,
            estimated_wal_cost: 0,
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(
        !resp.signed_transaction.is_empty(),
        "signed transaction should be non-empty"
    );

    // Verify the response deserializes back into a valid Transaction.
    let _tx: sui_types::transaction::Transaction =
        bcs::from_bytes(&resp.signed_transaction).expect("valid Transaction");
}

#[tokio::test]
async fn sign_transaction_account_not_found() {
    let mut client = start_server().await;

    let (sender, _kp): (SuiAddress, Ed25519KeyPair) = get_key_pair();
    let tx_data = mock_transaction_data(sender);
    let tx_data_bytes = bcs::to_bytes(&tx_data).unwrap();

    let status = client
        .sign_transaction(authenticated(proto::SignTransactionRequest {
            account_id: "nonexistent-account-id".to_string(),
            tx_data: tx_data_bytes,
            estimated_sui_cost: 0,
            estimated_wal_cost: 0,
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn auth_rejection_no_token() {
    let mut client = start_server().await;

    // Request with no auth token.
    let status = client
        .create_account(Request::new(proto::CreateAccountRequest {
            min_sui_balance: 0,
            min_wal_balance: 0,
            top_up_target_sui: 0,
            top_up_target_wal: 0,
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn auth_rejection_wrong_token() {
    let mut client = start_server().await;

    let mut req = Request::new(proto::CreateAccountRequest {
        min_sui_balance: 0,
        min_wal_balance: 0,
        top_up_target_sui: 0,
        top_up_target_wal: 0,
    });
    req.metadata_mut()
        .insert("authorization", "Bearer wrong-secret".parse().unwrap());

    let status = client.create_account(req).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn multiple_accounts_unique() {
    let mut client = start_server().await;

    let mut ids = std::collections::HashSet::new();
    let mut addrs = std::collections::HashSet::new();

    for _ in 0..10 {
        let resp = client
            .create_account(authenticated(proto::CreateAccountRequest {
                min_sui_balance: 0,
                min_wal_balance: 0,
                top_up_target_sui: 0,
                top_up_target_wal: 0,
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(ids.insert(resp.account_id), "duplicate account_id");
        assert!(addrs.insert(resp.address), "duplicate address");
    }
}

// ---------------------------------------------------------------------------
// Balance tracking tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sign_transaction_returns_pending_id() {
    let mut client = start_server().await;

    let create_resp = client
        .create_account(authenticated(proto::CreateAccountRequest {
            min_sui_balance: 0,
            min_wal_balance: 0,
            top_up_target_sui: 0,
            top_up_target_wal: 0,
        }))
        .await
        .unwrap()
        .into_inner();

    let sender: SuiAddress = create_resp.address.parse().unwrap();
    let tx_data = mock_transaction_data(sender);
    let tx_data_bytes = bcs::to_bytes(&tx_data).unwrap();

    let resp = client
        .sign_transaction(authenticated(proto::SignTransactionRequest {
            account_id: create_resp.account_id,
            tx_data: tx_data_bytes,
            estimated_sui_cost: 100,
            estimated_wal_cost: 200,
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(
        !resp.pending_transaction_id.is_empty(),
        "pending_transaction_id should be non-empty"
    );
}

#[tokio::test]
async fn get_balance_rpc() {
    let mut client = start_server().await;

    let create_resp = client
        .create_account(authenticated(proto::CreateAccountRequest {
            min_sui_balance: 100,
            min_wal_balance: 200,
            top_up_target_sui: 500,
            top_up_target_wal: 1000,
        }))
        .await
        .unwrap()
        .into_inner();

    let bal = client
        .get_balance(authenticated(proto::GetBalanceRequest {
            account_id: create_resp.account_id.clone(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(bal.account_id, create_resp.account_id);
    assert_eq!(bal.cached_sui_balance, 0);
    assert_eq!(bal.cached_wal_balance, 0);
    assert_eq!(bal.min_sui_balance, 100);
    assert_eq!(bal.min_wal_balance, 200);
}

#[tokio::test]
async fn confirm_transaction_rpc() {
    let mut client = start_server().await;

    let create_resp = client
        .create_account(authenticated(proto::CreateAccountRequest {
            min_sui_balance: 0,
            min_wal_balance: 0,
            top_up_target_sui: 0,
            top_up_target_wal: 0,
        }))
        .await
        .unwrap()
        .into_inner();

    let sender: SuiAddress = create_resp.address.parse().unwrap();
    let tx_data = mock_transaction_data(sender);
    let tx_data_bytes = bcs::to_bytes(&tx_data).unwrap();

    let sign_resp = client
        .sign_transaction(authenticated(proto::SignTransactionRequest {
            account_id: create_resp.account_id.clone(),
            tx_data: tx_data_bytes,
            estimated_sui_cost: 500,
            estimated_wal_cost: 0,
        }))
        .await
        .unwrap()
        .into_inner();

    // Balance should be deducted after signing.
    let bal = client
        .get_balance(authenticated(proto::GetBalanceRequest {
            account_id: create_resp.account_id.clone(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(bal.cached_sui_balance, -500);

    // Confirm the transaction as successful with actual cost of 300.
    let confirm_resp = client
        .confirm_transaction(authenticated(proto::ConfirmTransactionRequest {
            pending_transaction_id: sign_resp.pending_transaction_id,
            tx_digest: "test-digest".into(),
            success: true,
            actual_sui_cost: 300,
            actual_wal_cost: 0,
        }))
        .await
        .unwrap()
        .into_inner();

    // Balance should reflect the correction: -500 + (500 - 300) = -300
    assert_eq!(confirm_resp.cached_sui_balance, -300);
}

#[tokio::test]
async fn sign_with_estimates_deducts() {
    let mut client = start_server().await;

    let create_resp = client
        .create_account(authenticated(proto::CreateAccountRequest {
            min_sui_balance: 0,
            min_wal_balance: 0,
            top_up_target_sui: 0,
            top_up_target_wal: 0,
        }))
        .await
        .unwrap()
        .into_inner();

    let sender: SuiAddress = create_resp.address.parse().unwrap();
    let tx_data = mock_transaction_data(sender);
    let tx_data_bytes = bcs::to_bytes(&tx_data).unwrap();

    client
        .sign_transaction(authenticated(proto::SignTransactionRequest {
            account_id: create_resp.account_id.clone(),
            tx_data: tx_data_bytes,
            estimated_sui_cost: 1000,
            estimated_wal_cost: 2000,
        }))
        .await
        .unwrap();

    let bal = client
        .get_balance(authenticated(proto::GetBalanceRequest {
            account_id: create_resp.account_id,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(bal.cached_sui_balance, -1000);
    assert_eq!(bal.cached_wal_balance, -2000);
}
