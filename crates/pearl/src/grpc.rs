use tonic::{Request, Response, Status};

use crate::{config, db, derivation};

#[allow(missing_docs)]
pub mod proto {
    tonic::include_proto!("pearl");
}

use proto::pearl_server::Pearl;

/// The Pearl gRPC service implementation.
pub struct PearlService {
    /// Database connection pool.
    pub db: db::DbPool,
    /// Service configuration (contains master seed for key derivation).
    pub config: config::Config,
}

#[tonic::async_trait]
impl Pearl for PearlService {
    async fn create_account(
        &self,
        _request: Request<proto::CreateAccountRequest>,
    ) -> Result<Response<proto::CreateAccountResponse>, Status> {
        let account = db::accounts::create_account(&self.db)
            .await
            .map_err(to_status)?;

        let address = derivation::derive_address(&self.config.master_seed, &account.id);

        Ok(Response::new(proto::CreateAccountResponse {
            account_id: account.id,
            address,
        }))
    }

    async fn get_address(
        &self,
        request: Request<proto::GetAddressRequest>,
    ) -> Result<Response<proto::GetAddressResponse>, Status> {
        let req = request.into_inner();
        db::accounts::account_exists(&self.db, &req.account_id)
            .await
            .map_err(to_status)?;

        let address = derivation::derive_address(&self.config.master_seed, &req.account_id);

        Ok(Response::new(proto::GetAddressResponse { address }))
    }

    async fn sign_transaction(
        &self,
        request: Request<proto::SignTransactionRequest>,
    ) -> Result<Response<proto::SignTransactionResponse>, Status> {
        let req = request.into_inner();
        db::accounts::account_exists(&self.db, &req.account_id)
            .await
            .map_err(to_status)?;

        let private_key = derivation::derive_private_key(&self.config.master_seed, &req.account_id);
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
        crate::error::Error::Db(e) => Status::internal(format!("database error: {e}")),
        crate::error::Error::InvalidPrivateKey(e) => {
            Status::internal(format!("invalid private key: {e}"))
        }
        crate::error::Error::InvalidTransactionData(e) => {
            Status::invalid_argument(format!("invalid transaction data: {e}"))
        }
        crate::error::Error::SigningError(e) => Status::internal(format!("signing error: {e}")),
        crate::error::Error::DerivationError(e) => {
            Status::internal(format!("key derivation error: {e}"))
        }
    }
}
