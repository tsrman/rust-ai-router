use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::time::Duration;

/// Middleware для таймаута обработки запроса.
/// Принимает `State<Duration>` — максимальное время обработки.
/// При превышении возвращает 504 Gateway Timeout.
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
