use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::time::Duration;

/// Middleware for request processing timeout.
/// Accepts `State<Duration>` — maximum processing time.
/// Returns 504 Gateway Timeout when exceeded.
pub async fn timeout_middleware(
    State(timeout): State<Duration>,
    req: Request,
    next: Next,
) -> Response {
    match tokio::time::timeout(timeout, next.run(req)).await {
        Ok(response) => response,
        Err(_elapsed) => {
            tracing::warn!("Request timeout after {:?}", timeout);
            (StatusCode::GATEWAY_TIMEOUT, "Request timeout").into_response()
        }
    }
}
