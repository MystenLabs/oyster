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

    pub async fn create_account(
        &self,
        min_sui_balance: i64,
        min_wal_balance: i64,
        top_up_target_sui: i64,
        top_up_target_wal: i64,
    ) -> Result<proto::CreateAccountResponse, tonic::Status> {
        let req = self.authenticated(proto::CreateAccountRequest {
            min_sui_balance,
            min_wal_balance,
            top_up_target_sui,
            top_up_target_wal,
        });
        self.client
            .clone()
            .create_account(req)
            .await
            .map(|r| r.into_inner())
    }

    pub async fn get_account_wallets(
        &self,
        account_id: &str,
    ) -> Result<proto::GetAccountWalletsResponse, tonic::Status> {
        let req = self.authenticated(proto::GetAccountWalletsRequest {
            account_id: account_id.to_string(),
        });
        self.client
            .clone()
            .get_account_wallets(req)
            .await
            .map(|r| r.into_inner())
    }

    pub async fn sign_transaction(
        &self,
        account_id: &str,
        tx_data: Vec<u8>,
        estimated_sui_cost: i64,
        estimated_wal_cost: i64,
    ) -> Result<proto::SignTransactionResponse, tonic::Status> {
        let req = self.authenticated(proto::SignTransactionRequest {
            account_id: account_id.to_string(),
            tx_data,
            estimated_sui_cost,
            estimated_wal_cost,
        });
        self.client
            .clone()
            .sign_transaction(req)
            .await
            .map(|r| r.into_inner())
    }

    pub async fn get_balance(
        &self,
        account_id: &str,
    ) -> Result<proto::GetBalanceResponse, tonic::Status> {
        let req = self.authenticated(proto::GetBalanceRequest {
            account_id: account_id.to_string(),
        });
        self.client
            .clone()
            .get_balance(req)
            .await
            .map(|r| r.into_inner())
    }

    pub async fn confirm_transaction(
        &self,
        pending_transaction_id: &str,
        tx_digest: &str,
        success: bool,
        actual_sui_cost: i64,
        actual_wal_cost: i64,
    ) -> Result<proto::ConfirmTransactionResponse, tonic::Status> {
        let req = self.authenticated(proto::ConfirmTransactionRequest {
            pending_transaction_id: pending_transaction_id.to_string(),
            tx_digest: tx_digest.to_string(),
            success,
            actual_sui_cost,
            actual_wal_cost,
        });
        self.client
            .clone()
            .confirm_transaction(req)
            .await
            .map(|r| r.into_inner())
    }
}
