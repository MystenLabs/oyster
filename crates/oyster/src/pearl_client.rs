use tonic::{Request, transport::Channel};

#[allow(missing_docs)]
pub mod proto {
    tonic::include_proto!("pearl");
}

use proto::pearl_client::PearlClient;

/// gRPC client wrapper for the Pearl signing service.
#[derive(Clone)]
pub struct PearlConnection {
    client: PearlClient<Channel>,
    service_secret: String,
}

impl PearlConnection {
    /// Connect to a Pearl gRPC server at the given URL.
    pub async fn connect(
        url: &str,
        service_secret: String,
    ) -> Result<Self, tonic::transport::Error> {
        let channel = if url.starts_with("https://") {
            let tls_config = tonic::transport::ClientTlsConfig::new().with_enabled_roots();
            tonic::transport::Endpoint::from_shared(url.to_string())
                .expect("invalid Pearl gRPC URL")
                .tls_config(tls_config)?
                .connect()
                .await?
        } else {
            tonic::transport::Endpoint::from_shared(url.to_string())
                .expect("invalid Pearl gRPC URL")
                .connect()
                .await?
        };
        let client = PearlClient::new(channel);
        Ok(Self {
            client,
            service_secret,
        })
    }

    fn authenticated<T>(&self, msg: T) -> Request<T> {
        let mut req = Request::new(msg);
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", self.service_secret)
                .parse()
                .expect("valid metadata value"),
        );
        req
    }

    /// Create a new Pearl account and return its ID and wallet address.
    pub async fn create_account(&self) -> Result<proto::CreateAccountResponse, tonic::Status> {
        let req = self.authenticated(proto::CreateAccountRequest {});
        self.client
            .clone()
            .create_account(req)
            .await
            .map(|r| r.into_inner())
    }

    /// Get the Sui wallet address for a Pearl account.
    pub async fn get_address(&self, account_id: &str) -> Result<String, tonic::Status> {
        let req = self.authenticated(proto::GetAddressRequest {
            account_id: account_id.to_string(),
        });
        self.client
            .clone()
            .get_address(req)
            .await
            .map(|r| r.into_inner().address)
    }

    /// Check if the Pearl gRPC service is reachable.
    pub async fn ping(&self) -> bool {
        match self.get_address("__health_check__").await {
            Ok(_) => true,
            Err(status) => {
                // Any gRPC-level error (NOT_FOUND, INVALID_ARGUMENT, etc.) means
                // the service is reachable. Only transport failures are unreachable.
                status.code() != tonic::Code::Unavailable && status.code() != tonic::Code::Unknown
            }
        }
    }

    /// Sign a transaction using the Pearl account's derived key.
    pub async fn sign_transaction(
        &self,
        account_id: &str,
        tx_data: Vec<u8>,
    ) -> Result<Vec<u8>, tonic::Status> {
        let req = self.authenticated(proto::SignTransactionRequest {
            account_id: account_id.to_string(),
            tx_data,
        });
        self.client
            .clone()
            .sign_transaction(req)
            .await
            .map(|r| r.into_inner().signed_transaction)
    }
}
