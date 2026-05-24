mod auth;
mod config;
mod fail2ban;
mod health;
mod metrics;
mod proxy;
mod ratelimit;
mod router;
mod stats;
mod sync;
mod utils;

use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use prometheus::{Encoder, TextEncoder};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use router::{ModelRouter, SessionStickyStore};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── CLI аргументы ──────────────────────────────────────────────────
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    args.retain(|a| a != "--verbose" && a != "-v");

    let config_path = args
        .first()
        .cloned()
        .or_else(|| std::env::var("OPENAI_ROUTER_CONFIG").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.yaml"));

    // ── Логгирование ───────────────────────────────────────────────────
    let default_level = if verbose { "debug" } else { "info" };
    let env_filter = EnvFilter::try_from_env("RUST_LOG")
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)       // убираем module path для компактности
        .with_thread_ids(false)
        .with_line_number(false)
        .with_file(false)
        .init();

    // ── Загрузка конфига ───────────────────────────────────────────────
    tracing::info!(config = %config_path.display(), "Loading config");

    let config = config::watcher::watch_config(config_path.clone()).await?;
    let cfg = config.load();
    let listen_addr: SocketAddr = cfg.server.listen.parse()?;
    let base_path = cfg.server.base_path.clone();
    let timeouts = cfg.server.timeouts.clone();

    tracing::info!(
        models = cfg.models.len(),
        tokens = cfg.tokens.len(),
        teams = cfg.teams.len(),
        endpoints = cfg.models.iter().map(|m| m.endpoints.len()).sum::<usize>(),
        "Config loaded"
    );
    tracing::info!(
        client_idle = timeouts.client_idle_secs,
        client_read = timeouts.client_read_secs,
        upstream_connect = timeouts.upstream_connect_secs,
        upstream_read = timeouts.upstream_read_secs,
        "Timeouts"
    );

    // ── HTTP клиент ────────────────────────────────────────────────────
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(timeouts.upstream_connect_secs))
        .timeout(Duration::from_secs(timeouts.upstream_read_secs))
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(timeouts.client_idle_secs))
        .build()?;

    // ── Компоненты ─────────────────────────────────────────────────────
    let mdl_router = ModelRouter::new(config.clone());
    let rate_limiters = ratelimit::RateLimiterStore::new();
    let fail2ban = Arc::new(fail2ban::Fail2ban::new(
        cfg.fail2ban.max_failures,
        cfg.fail2ban.ban_duration_secs,
        cfg.fail2ban.error_threshold_pct,
        &cfg.fail2ban.fail_status_codes,
    ));
    let sticky = SessionStickyStore::new(cfg.session.sticky_ttl_secs);

    // ── Фоновая проверка эндпоинтов ────────────────────────────────────
    let health_check_interval = cfg.fail2ban.health_check_interval_secs;
    let health_store = Arc::new(health::checker::HealthStore::new());
    let health_check_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()?;
    health::checker::start_background_health_checker(
        config.clone(),
        fail2ban.clone(),
        health_store.clone(),
        health_check_client,
        health_check_interval,
    );

    // ── Sync (Redis/Valkey) ───────────────────────────────────────────
    let sync_store = if cfg.sync.enabled {
        match crate::sync::SyncStore::connect(
            cfg.sync.redis_url.as_deref().unwrap_or("redis://localhost:6379"),
            &cfg.sync.key_prefix,
        ).await {
            Ok(store) => {
                tracing::info!("Sync connected to Redis/Valkey");
                Arc::new(store)
            }
            Err(e) => {
                tracing::error!("Sync connection failed: {e}, continuing without sync");
                Arc::new(crate::sync::SyncStore::new())
            }
        }
    } else {
        Arc::new(crate::sync::SyncStore::new())
    };

    let stats_writer = Arc::new(stats::StatsWriter::new(
        &cfg.stats.postgres_url.clone().unwrap_or_default(),
        cfg.stats.retention_days,
        cfg.stats.cleanup_interval_secs,
        cfg.stats.aggregation_interval_secs,
    ));
    stats_writer.start_background_tasks();

    let state = Arc::new(proxy::handler::AppState {
        config: config.clone(),
        client,
        router: mdl_router,
        rate_limiters,
        fail2ban: fail2ban.clone(),
        sticky,
        stats: stats_writer,
        sync: sync_store,
        health_store,
    });

    let prometheus_registry = prometheus::default_registry();
    crate::metrics::prometheus::init();

    // ── Middleware ──────────────────────────────────────────────────────
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(|req: &axum::http::Request<_>| {
            tracing::info_span!(
                "request",
                method = %req.method(),
                uri = %req.uri().path(),
            )
        })
        .on_request(|_req: &axum::http::Request<_>, _span: &tracing::Span| {
            tracing::debug!("→ request started");
        })
        .on_response(
            |resp: &axum::http::Response<_>, latency: Duration, _span: &tracing::Span| {
                let status = resp.status().as_u16();
                let ms = latency.as_millis();
                if status >= 500 {
                    tracing::error!(status, latency_ms = ms, "← response");
                } else if status >= 400 {
                    tracing::warn!(status, latency_ms = ms, "← response");
                } else {
                    tracing::info!(status, latency_ms = ms, "← response");
                }
            },
        );

    // ── Health routes (root, без auth) ──────────────────────────────────
    let health_routes = Router::new()
        .route("/health", get(health::dashboard::health_json))
        .route("/vhealth", get(health::dashboard::health_dashboard))
        .route("/metrics", get(|| async move {
            let encoder = TextEncoder::new();
            let metric_families = prometheus::default_registry().gather();
            let mut buffer = vec![];
            encoder.encode(&metric_families, &mut buffer).unwrap();
            ([("content-type", "text/plain; version=0.0.4")], buffer)
        }))
        .with_state(state.clone());

    // ── Роутер ─────────────────────────────────────────────────────────
    let mut api_routes = Router::new()
        .route("/health", get(health::dashboard::health_json))
        .route("/vhealth", get(health::dashboard::health_dashboard))
        .route(
            "/metrics",
            get(move || async move {
                let encoder = TextEncoder::new();
                let metric_families = prometheus_registry.gather();
                let mut buffer = vec![];
                encoder.encode(&metric_families, &mut buffer).unwrap();
                ([("content-type", "text/plain; version=0.0.4")], buffer)
            }),
        )
        .route("/v1/chat/completions", post(proxy::handler::proxy_handler))
        .route("/v1/completions", post(proxy::handler::proxy_handler))
        .route("/v1/embeddings", post(proxy::handler::proxy_handler))
        .route("/v1/messages", post(proxy::handler::proxy_handler))
        .route("/v1/models", get(list_models))
        .layer(trace_layer)
        .layer(cors);

    // Таймаут на полный цикл запроса
    if timeouts.client_read_secs > 0 {
        api_routes = api_routes.layer(middleware::from_fn_with_state(
            Duration::from_secs(timeouts.client_read_secs),
            auth::timeout::timeout_middleware,
        ));
    }

    let api_routes = api_routes
        .layer(middleware::from_fn_with_state(
            config.clone(),
            auth::middleware::auth_middleware,
        ))
        .with_state(state);

    // ── Монтирование (root + base_path) ────────────────────────────────
    let app = if base_path.is_empty() {
        tracing::info!(addr = %listen_addr, "Listening (root path)");
        api_routes
    } else {
        let bp = base_path.trim_start_matches('/');
        tracing::info!(addr = %listen_addr, base_path = %bp, "Listening (root + nested)");
        health_routes.merge(api_routes.clone().nest(&format!("/{bp}"), api_routes))
    };

    tracing::info!("Server ready, accepting connections");

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

// ── GET /v1/models ─────────────────────────────────────────────────────

async fn list_models(
    axum::extract::State(state): axum::extract::State<Arc<proxy::handler::AppState>>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use axum::http::StatusCode;
    use crate::ratelimit::RateLimitScope;

    let auth = match req.extensions().get::<crate::auth::AuthContext>() {
        Some(ctx) => ctx.clone(),
        None => return (StatusCode::UNAUTHORIZED, "Authentication required").into_response(),
    };

    // Rate limiting (sync → локальный)
    if auth.rpm > 0 {
        let sync_rl = state.sync.check_rate_limit("token", &auth.token_key, auth.rpm as u64).await;
        if !sync_rl.allowed {
            return proxy::handler::rate_limit_response(&sync_rl, "Token (shared)");
        }
    }
    let rl = state.rate_limiters.check(
        &auth.token_key, auth.rpm, auth.tpm, 1,
        RateLimitScope::Token,
    );
    if !rl.allowed {
        crate::metrics::prometheus::RATE_LIMIT_HITS
            .with_label_values(&["token", &auth.token_key])
            .inc();
        return proxy::handler::rate_limit_response(&rl, "Token");
    }

    let cfg = state.config.load();
    let models: Vec<serde_json::Value> = cfg
        .models
        .iter()
        .filter(|m| cfg.token_has_model_access(&auth.token_key, &m.name))
        .map(|m| {
            serde_json::json!({
                "id": m.name,
                "object": "model",
                "owned_by": "openai-router",
            })
        })
        .collect();

    axum::Json(serde_json::json!({
        "object": "list",
        "data": models,
    }))
    .into_response()
}
