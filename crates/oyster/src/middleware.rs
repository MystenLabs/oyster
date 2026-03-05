//! Axum middleware for recording HTTP request metrics.

use axum::{extract::MatchedPath, middleware::Next, response::Response};

use crate::metrics as metric_names;

/// Record HTTP request count and duration using route templates from [`MatchedPath`].
pub async fn track_http_metrics(
    matched_path: Option<MatchedPath>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let method = req.method().to_string();
    let path = matched_path
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let start = std::time::Instant::now();

    let response = next.run(req).await;

    let status = response.status().as_u16().to_string();
    let duration = start.elapsed().as_secs_f64();

    metrics::counter!(metric_names::HTTP_REQUESTS_TOTAL,
        "method" => method.clone(), "path" => path.clone(), "status" => status
    )
    .increment(1);
    metrics::histogram!(metric_names::HTTP_REQUEST_DURATION,
        "method" => method, "path" => path
    )
    .record(duration);

    response
}
