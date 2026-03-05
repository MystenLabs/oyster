//! Prometheus `/metrics` endpoint for operational observability.

use axum::{extract::State, http::StatusCode, response::IntoResponse};

use crate::AppState;

/// Render Prometheus text-format metrics. Gauges are refreshed from the DB on each scrape.
pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let handle = match &state.metrics_handle {
        Some(h) => h,
        None => return (StatusCode::NOT_FOUND, "metrics not configured".to_string()),
    };

    if let Ok(count) = crate::db::accounts::count_accounts(&state.db).await {
        metrics::gauge!(crate::metrics::ACTIVE_ACCOUNTS).set(count as f64);
    }
    if let Ok(count) = crate::db::blobs::count_blobs(&state.db).await {
        metrics::gauge!(crate::metrics::ACTIVE_BLOBS).set(count as f64);
    }

    (StatusCode::OK, handle.render())
}
