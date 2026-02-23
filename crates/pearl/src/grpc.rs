use tonic::{Request, Response, Status};

use crate::{db, models};

pub mod proto {
    tonic::include_proto!("pearl");
}

use proto::pearl_server::Pearl;

pub struct PearlService {
    pub db: db::DbPool,
}

#[tonic::async_trait]
impl Pearl for PearlService {
    async fn create_account(
        &self,
        request: Request<proto::CreateAccountRequest>,
    ) -> Result<Response<proto::CreateAccountResponse>, Status> {
        let req = request.into_inner();
        let create_req = models::CreateAccountRequest {
            min_sui_balance: req.min_sui_balance,
            min_wal_balance: req.min_wal_balance,
            top_up_target_sui: req.top_up_target_sui,
            top_up_target_wal: req.top_up_target_wal,
        };

        // For now, use a generated credential. In production this would come
        // from the caller or be derived from the service-to-service auth.
        let credentials = uuid::Uuid::new_v4().to_string();

        let account = db::accounts::create_account(&self.db, &create_req, &credentials)
            .await
            .map_err(to_status)?;

        Ok(Response::new(proto::CreateAccountResponse {
            account_id: account.id,
            address: account.address,
        }))
    }

    async fn get_account_wallets(
        &self,
        request: Request<proto::GetAccountWalletsRequest>,
    ) -> Result<Response<proto::GetAccountWalletsResponse>, Status> {
        let req = request.into_inner();
        let wallets = db::accounts::get_account_wallets(&self.db, &req.account_id)
            .await
            .map_err(to_status)?;

        let wallets = wallets
            .into_iter()
            .map(|w| proto::WalletInfo {
                account_id: w.account_id,
                address: w.address,
                min_sui_balance: w.min_sui_balance,
                min_wal_balance: w.min_wal_balance,
                top_up_target_sui: w.top_up_target_sui,
                top_up_target_wal: w.top_up_target_wal,
            })
            .collect();

        Ok(Response::new(proto::GetAccountWalletsResponse { wallets }))
    }

    async fn sign_transaction(
        &self,
        request: Request<proto::SignTransactionRequest>,
    ) -> Result<Response<proto::SignTransactionResponse>, Status> {
        let req = request.into_inner();
        let private_key = db::accounts::get_private_key(&self.db, &req.account_id)
            .await
            .map_err(to_status)?;
        let signed_bytes =
            crate::signing::sign_transaction(&private_key, &req.tx_data).map_err(to_status)?;
        Ok(Response::new(proto::SignTransactionResponse {
            signed_transaction: signed_bytes,
        }))
    }
}

fn to_status(err: crate::error::Error) -> Status {
    match err {
        crate::error::Error::AccountNotFound => Status::not_found("account not found"),
        crate::error::Error::InvalidCredentials => Status::unauthenticated("invalid credentials"),
        crate::error::Error::Db(e) => Status::internal(format!("database error: {e}")),
        crate::error::Error::InvalidPrivateKey(e) => {
            Status::internal(format!("invalid private key: {e}"))
        }
        crate::error::Error::InvalidTransactionData(e) => {
            Status::invalid_argument(format!("invalid transaction data: {e}"))
        }
        crate::error::Error::SigningError(e) => Status::internal(format!("signing error: {e}")),
    }
}
