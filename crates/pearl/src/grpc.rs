use tonic::{Request, Response, Status};

use crate::{config, db};

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
        _request: Request<proto::CreateAccountRequest>,
    ) -> Result<Response<proto::CreateAccountResponse>, Status> {
        let credentials = uuid::Uuid::new_v4().to_string();

        let account = db::accounts::create_account(&self.db, &credentials)
            .await
            .map_err(to_status)?;

        Ok(Response::new(proto::CreateAccountResponse {
            account_id: account.id,
            address: account.address,
        }))
    }

    async fn get_address(
        &self,
        request: Request<proto::GetAddressRequest>,
    ) -> Result<Response<proto::GetAddressResponse>, Status> {
        let req = request.into_inner();
        let address = db::accounts::get_address(&self.db, &req.account_id)
            .await
            .map_err(to_status)?;

        Ok(Response::new(proto::GetAddressResponse { address }))
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
