use tonic::{Request, transport::Channel};
use tonic_health::pb::{health_check_response::ServingStatus, health_client::HealthClient};

use crate::AccountId;

#[allow(missing_docs)]
pub mod proto {
    tonic::include_proto!("pearl");
}

use proto::pearl_client::PearlClient;

/// gRPC client wrapper for the Pearl signing service.
#[derive(Clone)]
pub struct PearlConnection {
    client: PearlClient<Channel>,
    /// gRPC health check client (unauthenticated).
    health_client: HealthClient<Channel>,
    service_secret: String,
    /// Pearl's active key version at connect time. New accounts are
    /// stamped with this; existing accounts always sign with their
    /// stored per-account version, so a value that goes stale while a
    /// rotation flips Pearl's active version only affects which (still
    /// valid) version brand-new accounts land on until Oyster restarts.
    active_key_version: u32,
}

impl PearlConnection {
    /// Connect to a Pearl gRPC server at the given URL and fetch its
    /// active key version.
    pub async fn connect(
        url: &str,
        service_secret: String,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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
        let client = PearlClient::new(channel.clone());
        let health_client = HealthClient::new(channel);
        let mut conn = Self {
            client,
            health_client,
            service_secret,
            active_key_version: 0,
        };
        let req = conn.authenticated(proto::GetActiveKeyVersionRequest {});
        let resp = conn
            .client
            .get_active_key_version(req)
            .await
            .map_err(|e| format!("pearl get_active_key_version failed: {e}"))?
            .into_inner();
        conn.active_key_version = resp.key_version;
        Ok(conn)
    }

    /// The key version Pearl stamps on newly created accounts.
    pub fn active_key_version(&self) -> u32 {
        self.active_key_version
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

    /// Get the Sui wallet address for a Pearl account, derived from the
    /// given master-seed version (the account's stored `key_version`).
    pub async fn get_address(
        &self,
        account_id: &AccountId,
        key_version: u32,
    ) -> Result<String, tonic::Status> {
        let req = self.authenticated(proto::GetAddressRequest {
            account_id: account_id.as_bytes().to_vec(),
            key_version,
        });
        let start = std::time::Instant::now();
        let result = self.client.clone().get_address(req).await;
        let duration = start.elapsed().as_secs_f64();
        let outcome = if result.is_ok() { "ok" } else { "error" };
        metrics::counter!(crate::metrics::PEARL_GRPC_CALLS_TOTAL,
            "method" => "get_address", "result" => outcome
        )
        .increment(1);
        metrics::histogram!(crate::metrics::PEARL_GRPC_LATENCY,
            "method" => "get_address"
        )
        .record(duration);
        result.map(|r| r.into_inner().address)
    }

    /// Check if the Pearl gRPC service is reachable and serving.
    pub async fn ping(&self) -> bool {
        let req = tonic_health::pb::HealthCheckRequest {
            service: "pearl.Pearl".to_string(),
        };
        match self.health_client.clone().check(req).await {
            Ok(resp) => resp.into_inner().status() == ServingStatus::Serving,
            Err(_) => false,
        }
    }

    /// Sign a transaction with the Pearl account's key, derived from the
    /// given master-seed version (the account's stored `key_version`).
    pub async fn sign_transaction(
        &self,
        account_id: &AccountId,
        key_version: u32,
        tx_data: Vec<u8>,
    ) -> Result<Vec<u8>, tonic::Status> {
        let req = self.authenticated(proto::SignTransactionRequest {
            account_id: account_id.as_bytes().to_vec(),
            key_version,
            tx_data,
        });
        let start = std::time::Instant::now();
        let result = self.client.clone().sign_transaction(req).await;
        let duration = start.elapsed().as_secs_f64();
        let outcome = if result.is_ok() { "ok" } else { "error" };
        metrics::counter!(crate::metrics::PEARL_GRPC_CALLS_TOTAL,
            "method" => "sign_transaction", "result" => outcome
        )
        .increment(1);
        metrics::histogram!(crate::metrics::PEARL_GRPC_LATENCY,
            "method" => "sign_transaction"
        )
        .record(duration);
        result.map(|r| r.into_inner().signed_transaction)
    }
}
