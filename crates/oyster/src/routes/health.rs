use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;

use crate::AppState;

/// Response body for the liveness probe.
#[derive(Serialize, utoipa::ToSchema)]
pub struct HealthResponse {
    /// Always `"ok"`.
    status: &'static str,
}

/// Response body for the readiness probe.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ReadyResponse {
    /// Whether all dependencies are reachable.
    ready: bool,
    /// Set to `"unreachable"` when the database check fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    database: Option<&'static str>,
    /// Set to `"unreachable"` when the Pearl gRPC service is down.
    #[serde(skip_serializing_if = "Option::is_none")]
    pearl: Option<&'static str>,
}

/// Liveness probe — always returns 200.
#[utoipa::path(
    get, path = "/health", tag = "Health",
    responses((status = 200, description = "Service is alive", body = HealthResponse)),
)]
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

/// Readiness probe — returns 200 if SQLite and Pearl (when configured) are reachable.
#[utoipa::path(
    get, path = "/ready", tag = "Health",
    responses(
        (status = 200, description = "Service is ready", body = ReadyResponse),
        (status = 503, description = "Service is not ready", body = ReadyResponse),
    ),
)]
pub async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
        .is_ok();

    let pearl_ok = match &state.pearl {
        Some(pearl) => pearl.ping().await,
        None => true,
    };

    let ready = db_ok && pearl_ok;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    let body = ReadyResponse {
        ready,
        database: if db_ok { None } else { Some("unreachable") },
        pearl: if pearl_ok { None } else { Some("unreachable") },
    };

    (status, Json(body))
}
