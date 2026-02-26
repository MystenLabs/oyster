use tonic::{Request, transport::Channel};

pub mod proto {
    tonic::include_proto!("pearl");
}

use proto::pearl_client::PearlClient;

#[derive(Clone)]
pub struct PearlConnection {
    client: PearlClient<Channel>,
    service_secret: String,
}

impl PearlConnection {
    pub async fn connect(
        url: &str,
        service_secret: String,
    ) -> Result<Self, tonic::transport::Error> {
        let client = PearlClient::connect(url.to_string()).await?;
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

    pub async fn create_account(&self) -> Result<proto::CreateAccountResponse, tonic::Status> {
        let req = self.authenticated(proto::CreateAccountRequest {});
        self.client
            .clone()
            .create_account(req)
            .await
            .map(|r| r.into_inner())
    }

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
