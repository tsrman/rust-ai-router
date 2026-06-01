use axum::{
    extract::State,
    response::{Html, Json},
};
use serde_json::json;
use std::sync::Arc;

use crate::proxy::handler::AppState;

/// GET /health — JSON статус
pub async fn health_json(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let cfg = state.config.load();

    let endpoints: Vec<serde_json::Value> = cfg
        .models
        .iter()
        .flat_map(|model| {
            model.endpoints.iter().enumerate().map(|(i, ep)| {
                let ep_key = format!("{}:{}", ep.url, mask_key(&ep.key));
                let banned = state.limits.fail2ban.is_banned(&ep_key);
                let health = state.monitoring.health_store.get(&ep_key);
                let healthy = health.as_ref().map(|h| h.healthy).unwrap_or(true);
                let last_error = health.as_ref().map(|h| h.last_error.clone()).unwrap_or_default();

                json!({
                    "model": model.name,
                    "endpoint_index": i,
                    "url": ep.url,
                    "healthy": healthy,
                    "banned": banned,
                    "last_error": last_error,
                    "rpm_limit": ep.limits.as_ref().map(|l| l.rpm).unwrap_or(0),
                    "tpm_limit": ep.limits.as_ref().map(|l| l.tpm).unwrap_or(0),
                })
            })
        })
        .collect();

    Json(json!({
        "status": "ok",
        "models": cfg.models.len(),
        "endpoints": endpoints,
        "active_tokens": cfg.tokens.len(),
        "active_teams": cfg.teams.len(),
    }))
}

/// GET /vhealth — HTML дашборд
pub async fn health_dashboard(
    State(state): State<Arc<AppState>>,
) -> Html<String> {
    let cfg = state.config.load();

    let mut rows = String::new();
    for model in &cfg.models {
        for (i, ep) in model.endpoints.iter().enumerate() {
            let ep_key = format!("{}:{}", ep.url, mask_key(&ep.key));
            let banned = state.limits.fail2ban.is_banned(&ep_key);
            let health = state.monitoring.health_store.get(&ep_key);
            let healthy = health.as_ref().map(|h| h.healthy).unwrap_or(true);
            let reason = if banned {
                state.limits.fail2ban.ban_reason(&ep_key).unwrap_or_default()
            } else if !healthy {
                health.as_ref().map(|h| h.last_error.clone()).unwrap_or_default()
            } else {
                String::new()
            };

            let (color, status_text) = if banned {
                ("#f44336", "BANNED")
            } else if !healthy {
                ("#ff9800", "DOWN")
            } else {
                ("#4caf50", "UP")
            };

            rows.push_str(&format!(
                "<tr>
                    <td>{}</td>
                    <td>{}</td>
                    <td>{}</td>
                    <td style='color:{}; font-weight:bold'>{}</td>
                    <td>{}</td>
                    <td>{}</td>
                    <td>{}</td>
                </tr>",
                model.name,
                i,
                ep.url,
                color,
                status_text,
                reason,
                ep.limits.as_ref().map(|l| l.rpm).unwrap_or(0),
                ep.limits.as_ref().map(|l| l.tpm).unwrap_or(0),
            ));
        }
    }

    let total_endpoints: usize = cfg.models.iter().map(|m| m.endpoints.len()).sum();

    Html(format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>OpenAI Router - Health Dashboard</title>
    <meta charset="utf-8">
    <meta http-equiv="refresh" content="10">
    <style>
        body {{ font-family: system-ui, sans-serif; background: #1a1a2e; color: #e0e0e0; margin: 0; padding: 20px; }}
        h1 {{ color: #7c4dff; }}
        table {{ width: 100%; border-collapse: collapse; margin-top: 20px; }}
        th {{ background: #16213e; padding: 12px; text-align: left; }}
        td {{ padding: 10px; border-bottom: 1px solid #333; }}
        tr:hover {{ background: #16213e; }}
        .summary {{ display: flex; gap: 20px; margin: 20px 0; }}
        .card {{ background: #16213e; padding: 15px 25px; border-radius: 8px; }}
        .card .value {{ font-size: 2em; font-weight: bold; color: #7c4dff; }}
        .legend {{ margin-top: 10px; font-size: 0.9em; color: #888; }}
        .legend span {{ margin-right: 15px; }}
    </style>
</head>
<body>
    <h1>🔄 OpenAI Router</h1>
    <div class="summary">
        <div class="card"><div>Models</div><div class="value">{}</div></div>
        <div class="card"><div>Endpoints</div><div class="value">{}</div></div>
        <div class="card"><div>Tokens</div><div class="value">{}</div></div>
        <div class="card"><div>Teams</div><div class="value">{}</div></div>
    </div>
    <div class="legend">
        <span style='color:#4caf50'>● UP</span>
        <span style='color:#ff9800'>● DOWN</span>
        <span style='color:#f44336'>● BANNED</span>
    </div>
    <table>
        <tr><th>Model</th><th>EP #</th><th>URL</th><th>Status</th><th>Reason</th><th>RPM</th><th>TPM</th></tr>
        {}
    </table>
    <p style="margin-top:20px; color:#666">Auto-refresh: 10s | <a href="/health" style="color:#7c4dff">JSON API</a> | <a href="/metrics" style="color:#7c4dff">Prometheus</a></p>
</body>
</html>"#,
        cfg.models.len(),
        total_endpoints,
        cfg.tokens.len(),
        cfg.teams.len(),
        rows
    ))
}

/// GET /stats — статистика по токенам, командам и эндпоинтам
pub async fn stats_json(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let requests_total = state.live_stats.requests_total.load(std::sync::atomic::Ordering::Relaxed);

    let tokens: serde_json::Value = state
        .live_stats
        .requests_by_token
        .iter()
        .map(|entry| {
            let key = entry.key().clone();
            let reqs = *entry.value();
            let errs = state.live_stats.errors_by_token.get(&key).map(|v| *v).unwrap_or(0);
            (key, json!({
                "requests": reqs,
                "errors": errs,
            }))
        })
        .collect::<serde_json::Map<String, serde_json::Value>>()
        .into();

    let teams: serde_json::Value = state
        .live_stats
        .requests_by_team
        .iter()
        .map(|entry| {
            let key = entry.key().clone();
            let reqs = *entry.value();
            let errs = state.live_stats.errors_by_team.get(&key).map(|v| *v).unwrap_or(0);
            (key, json!({
                "requests": reqs,
                "errors": errs,
            }))
        })
        .collect::<serde_json::Map<String, serde_json::Value>>()
        .into();

    let cfg = state.config.load();
    let endpoints: serde_json::Value = cfg
        .models
        .iter()
        .flat_map(|model| {
            model.endpoints.iter().map(|ep| {
                let ep_key = format!("{}:{}", ep.url, mask_key(&ep.key));
                let reqs = state.live_stats.requests_by_endpoint.get(&ep_key).map(|v| *v).unwrap_or(0);
                let errs = state.live_stats.errors_by_endpoint.get(&ep_key).map(|v| *v).unwrap_or(0);
                let banned = state.limits.fail2ban.is_banned(&ep_key);
                let health = state.monitoring.health_store.get(&ep_key);
                let healthy = health.as_ref().map(|h| h.healthy).unwrap_or(true);
                json!({
                    "model": model.name,
                    "url": ep.url,
                    "requests": reqs,
                    "errors": errs,
                    "banned": banned,
                    "healthy": healthy,
                })
            })
        })
        .collect::<Vec<_>>()
        .into();

    Json(json!({
        "requests_total": requests_total,
        "tokens": tokens,
        "teams": teams,
        "endpoints": endpoints,
    }))
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".into();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}
