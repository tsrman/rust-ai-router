use arc_swap::ArcSwap;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::config::AppConfig;

/// Контекст аутентификации, сохраняемый в request extensions
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AuthContext {
    pub token_key: String,
    pub team: String,
    pub models: Vec<String>,
    pub rpm: u32,
    pub tpm: u64,
    pub cost_multiplier: f64,
}

/// Middleware для проверки Bearer-токена.
/// Health/metrics эндпоинты пропускаются без аутентификации.
pub async fn auth_middleware(
    State(config): State<Arc<ArcSwap<AppConfig>>>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();

    // Health и метрики — без аутентификации
    if path == "/health" || path == "/vhealth" || path == "/metrics" {
        return next.run(req).await;
    }

    let cfg = config.load();

    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let token_key = if let Some(key) = auth_header.strip_prefix("Bearer ") {
        key
    } else {
        return (StatusCode::UNAUTHORIZED, "Missing Bearer token").into_response();
    };

    match cfg.resolve_token(token_key) {
        Some(eff) => {
            let team = cfg
                .tokens
                .iter()
                .find(|t| t.key == token_key)
                .map(|t| t.team.clone())
                .unwrap_or_default();

            let ctx = AuthContext {
                token_key: token_key.to_string(),
                team,
                models: eff.models,
                rpm: eff.rpm,
                tpm: eff.tpm,
                cost_multiplier: eff.cost_multiplier,
            };

            req.extensions_mut().insert(ctx);
            next.run(req).await
        }
        None => (StatusCode::UNAUTHORIZED, "Invalid token").into_response(),
    }
}
