use tonic::{Request, Response, Status};

use crate::{config, derivation, metrics as pearl_metrics};

#[allow(missing_docs)]
pub mod proto {
    tonic::include_proto!("pearl");
}

use proto::pearl_server::Pearl;

/// The Pearl gRPC service implementation.
pub struct PearlService {
    /// Service configuration (contains the master seeds for key derivation).
    pub config: config::Config,
}

impl PearlService {
    /// Resolve the master seed for a requested key version (0 = version 1,
    /// the pre-versioning default). Requests for versions with no
    /// configured seed are refused rather than falling back — signing with
    /// the wrong seed would produce a key for the wrong wallet.
    fn seed_for_version(&self, requested: u32) -> Result<&[u8], Status> {
        self.config
            .seed_for_version(requested)
            .map(|s| s.as_slice())
            .ok_or_else(|| {
                Status::failed_precondition(format!("no master seed for key version {requested}"))
            })
    }
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

            let seed = self.seed_for_version(req.key_version)?;
            let address = derivation::derive_address(seed, &req.account_id);

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

            let seed = self.seed_for_version(req.key_version)?;
            let private_key = derivation::derive_private_key(seed, &req.account_id);
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

    async fn get_active_key_version(
        &self,
        _request: Request<proto::GetActiveKeyVersionRequest>,
    ) -> Result<Response<proto::GetActiveKeyVersionResponse>, Status> {
        ::metrics::counter!(pearl_metrics::GRPC_REQUESTS_TOTAL, "method" => "get_active_key_version", "status" => "ok").increment(1);
        Ok(Response::new(proto::GetActiveKeyVersionResponse {
            key_version: self.config.active_key_version,
        }))
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
