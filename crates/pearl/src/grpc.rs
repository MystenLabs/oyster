use tonic::{Request, Response, Status};

use crate::{config, db, models};

pub mod proto {
    tonic::include_proto!("pearl");
}

use proto::pearl_server::Pearl;

pub struct PearlService {
    pub db: db::DbPool,
    pub config: config::Config,
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

        let ptx = db::pending_transactions::create_pending_transaction(
            &self.db,
            &req.account_id,
            req.estimated_sui_cost,
            req.estimated_wal_cost,
        )
        .await
        .map_err(to_status)?;

        Ok(Response::new(proto::SignTransactionResponse {
            signed_transaction: signed_bytes,
            pending_transaction_id: ptx.id,
        }))
    }

    async fn get_balance(
        &self,
        request: Request<proto::GetBalanceRequest>,
    ) -> Result<Response<proto::GetBalanceResponse>, Status> {
        let req = request.into_inner();
        let bal = db::accounts::get_balance(&self.db, &req.account_id)
            .await
            .map_err(to_status)?;

        Ok(Response::new(proto::GetBalanceResponse {
            account_id: req.account_id,
            cached_sui_balance: bal.cached_sui_balance,
            cached_wal_balance: bal.cached_wal_balance,
            min_sui_balance: bal.min_sui_balance,
            min_wal_balance: bal.min_wal_balance,
            balance_updated_at: bal.balance_updated_at.unwrap_or_default(),
        }))
    }

    async fn confirm_transaction(
        &self,
        request: Request<proto::ConfirmTransactionRequest>,
    ) -> Result<Response<proto::ConfirmTransactionResponse>, Status> {
        let req = request.into_inner();
        let bal = db::pending_transactions::confirm_transaction(
            &self.db,
            &req.pending_transaction_id,
            &req.tx_digest,
            req.success,
            req.actual_sui_cost,
            req.actual_wal_cost,
        )
        .await
        .map_err(to_status)?;

        Ok(Response::new(proto::ConfirmTransactionResponse {
            cached_sui_balance: bal.cached_sui_balance,
            cached_wal_balance: bal.cached_wal_balance,
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
        crate::error::Error::PendingTransactionNotFound => {
            Status::not_found("pending transaction not found")
        }
        crate::error::Error::PendingTransactionAlreadyResolved => {
            Status::failed_precondition("pending transaction already resolved")
        }
        crate::error::Error::SuiRpc(e) => Status::internal(format!("sui rpc error: {e}")),
    }
}
