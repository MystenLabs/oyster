#![allow(missing_docs)]

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
    programmable_transaction_builder::ProgrammableTransactionBuilder,
    transaction::TransactionData,
};
use tonic::{Request, transport::Channel};

const TEST_SECRET: &str = "test-secret-42";

fn test_seed() -> zeroize::Zeroizing<Vec<u8>> {
    zeroize::Zeroizing::new(hex::decode("ab".repeat(32)).unwrap())
}

fn test_config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        service_secret: TEST_SECRET.into(),
        master_seed: test_seed(),
        tls_cert_path: None,
        tls_key_path: None,
        metrics_bind_addr: "127.0.0.1:0".into(),
    }
}

/// Stand up Pearl's gRPC server in-process on a random port.
/// Returns a connected `PearlClient` and the server URL.
async fn start_server() -> (PearlClient<Channel>, String) {
    let service = PearlService {
        config: test_config(),
    };
    let interceptor = check_service_secret(TEST_SECRET.to_string());
    let svc = PearlServer::with_interceptor(service, interceptor);

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<PearlServer<PearlService>>()
        .await;

    // Bind to a random port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();

    // Convert tokio listener to a tonic-compatible incoming stream.
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(health_service)
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
            return (client, url);
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
async fn get_address() {
    let (mut client, _url) = start_server().await;

    let account_id = "test-account-1";

    let address_resp = client
        .get_address(authenticated(proto::GetAddressRequest {
            account_id: account_id.to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(
        address_resp.address.starts_with("0x"),
        "address should start with 0x, got: {}",
        address_resp.address
    );
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
    let (mut client, _url) = start_server().await;

    let status = client
        .sign_transaction(authenticated(proto::SignTransactionRequest {
            account_id: "test-account-2".to_string(),
            tx_data: vec![1, 2, 3],
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn sign_transaction_success() {
    let (mut client, _url) = start_server().await;

    let account_id = "test-account-3";

    // Get the account's address to build a valid TransactionData.
    let address_resp = client
        .get_address(authenticated(proto::GetAddressRequest {
            account_id: account_id.to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    let sender: SuiAddress = address_resp.address.parse().expect("valid SuiAddress");
    let tx_data = mock_transaction_data(sender);
    let tx_data_bytes = bcs::to_bytes(&tx_data).unwrap();

    let resp = client
        .sign_transaction(authenticated(proto::SignTransactionRequest {
            account_id: account_id.to_string(),
            tx_data: tx_data_bytes,
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
async fn auth_rejection_no_token() {
    let (mut client, _url) = start_server().await;

    // Request with no auth token.
    let status = client
        .get_address(Request::new(proto::GetAddressRequest {
            account_id: "test".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn auth_rejection_wrong_token() {
    let (mut client, _url) = start_server().await;

    let mut req = Request::new(proto::GetAddressRequest {
        account_id: "test".to_string(),
    });
    req.metadata_mut()
        .insert("authorization", "Bearer wrong-secret".parse().unwrap());

    let status = client.get_address(req).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn get_address_deterministic() {
    let (mut client, _url) = start_server().await;

    let account_id = "deterministic-test-account";

    let first_resp = client
        .get_address(authenticated(proto::GetAddressRequest {
            account_id: account_id.to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    // Call get_address 3 more times with the same account_id — all should return the identical address.
    for i in 0..3 {
        let resp = client
            .get_address(authenticated(proto::GetAddressRequest {
                account_id: account_id.to_string(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(
            resp.address, first_resp.address,
            "get_address call {i} returned a different address"
        );
    }
}

#[tokio::test]
async fn multiple_accounts_unique() {
    let (mut client, _url) = start_server().await;

    let mut addrs = std::collections::HashSet::new();

    for i in 0..10 {
        let resp = client
            .get_address(authenticated(proto::GetAddressRequest {
                account_id: format!("unique-account-{i}"),
            }))
            .await
            .unwrap()
            .into_inner();

        assert!(
            addrs.insert(resp.address),
            "duplicate address for account {i}"
        );
    }
}

#[tokio::test]
async fn health_check_without_auth() {
    let (_client, url) = start_server().await;
    let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
    let mut health_client = tonic_health::pb::health_client::HealthClient::new(channel);

    // Empty service name = overall server health.
    let resp = health_client
        .check(tonic_health::pb::HealthCheckRequest {
            service: String::new(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        resp.status(),
        tonic_health::pb::health_check_response::ServingStatus::Serving
    );

    // Named service check.
    let resp = health_client
        .check(tonic_health::pb::HealthCheckRequest {
            service: "pearl.Pearl".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        resp.status(),
        tonic_health::pb::health_check_response::ServingStatus::Serving
    );
}

#[tokio::test]
async fn health_check_unknown_service() {
    let (_client, url) = start_server().await;
    let channel = Channel::from_shared(url).unwrap().connect().await.unwrap();
    let mut health_client = tonic_health::pb::health_client::HealthClient::new(channel);

    let err = health_client
        .check(tonic_health::pb::HealthCheckRequest {
            service: "nonexistent.Service".to_string(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}
