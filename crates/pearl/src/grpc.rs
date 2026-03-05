use tonic::{Request, Response, Status};

use crate::{config, derivation, metrics as pearl_metrics};

#[allow(missing_docs)]
pub mod proto {
    tonic::include_proto!("pearl");
}

use proto::pearl_server::Pearl;

/// The Pearl gRPC service implementation.
pub struct PearlService {
    /// Service configuration (contains master seed for key derivation).
    pub config: config::Config,
}

#[tonic::async_trait]
impl Pearl for PearlService {
    async fn get_address(
        &self,
        request: Request<proto::GetAddressRequest>,
    ) -> Result<Response<proto::GetAddressResponse>, Status> {
        let start = std::time::Instant::now();
        let result = async {
            let req = request.into_inner();

            let address = derivation::derive_address(&self.config.master_seed, &req.account_id);

            Ok(Response::new(proto::GetAddressResponse { address }))
        }
        .await;

        let status = if result.is_ok() { "ok" } else { "err" };
        ::metrics::counter!(pearl_metrics::GRPC_REQUESTS_TOTAL, "method" => "get_address", "status" => status).increment(1);
        ::metrics::histogram!(pearl_metrics::GRPC_REQUEST_DURATION, "method" => "get_address")
            .record(start.elapsed().as_secs_f64());

        result
    }

    async fn sign_transaction(
        &self,
        request: Request<proto::SignTransactionRequest>,
    ) -> Result<Response<proto::SignTransactionResponse>, Status> {
        let start = std::time::Instant::now();
        let result = async {
            let req = request.into_inner();

            let private_key =
                derivation::derive_private_key(&self.config.master_seed, &req.account_id);
            let signed_bytes =
                crate::signing::sign_transaction(&private_key, &req.tx_data).map_err(to_status)?;

            Ok(Response::new(proto::SignTransactionResponse {
                signed_transaction: signed_bytes,
            }))
        }
        .await;

        let status = if result.is_ok() { "ok" } else { "err" };
        ::metrics::counter!(pearl_metrics::GRPC_REQUESTS_TOTAL, "method" => "sign_transaction", "status" => status).increment(1);
        ::metrics::histogram!(pearl_metrics::GRPC_REQUEST_DURATION, "method" => "sign_transaction")
            .record(start.elapsed().as_secs_f64());
        ::metrics::counter!(pearl_metrics::SIGN_TRANSACTIONS_TOTAL, "result" => status)
            .increment(1);

        result
    }
}

fn to_status(err: crate::error::Error) -> Status {
    match err {
        crate::error::Error::InvalidTransactionData(e) => {
            Status::invalid_argument(format!("invalid transaction data: {e}"))
        }
        err => {
            tracing::error!(%err, "internal error");
            Status::internal("internal error")
        }
    }
}
