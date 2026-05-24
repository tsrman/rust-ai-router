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
    let statuses = state.fail2ban.all_statuses();

    let endpoints: Vec<serde_json::Value> = cfg
        .models
        .iter()
        .flat_map(|model| {
            model.endpoints.iter().enumerate().map(|(i, ep)| {
                let ep_key = format!("{}:{}", ep.url, mask_key(&ep.key));
                let healthy = statuses
                    .iter()
                    .find(|(k, _)| k == &ep_key)
                    .map(|(_, ok)| *ok)
                    .unwrap_or(true);

                json!({
                    "model": model.name,
                    "endpoint_index": i,
                    "url": ep.url,
                    "healthy": healthy,
                    "rpm_limit": ep.limits.as_ref().map(|l| l.rpm).unwrap_or(0),
                    "tpm_limit": ep.limits.as_ref().map(|l| l.tpm).unwrap_or(0),
                })
            })
        })
        .collect();

    let active_tokens = cfg.tokens.len();
    let active_teams = cfg.teams.len();

    Json(json!({
        "status": "ok",
        "models": cfg.models.len(),
        "endpoints": endpoints,
        "active_tokens": active_tokens,
        "active_teams": active_teams,
    }))
}

/// GET /vhealth — HTML дашборд
pub async fn health_dashboard(
    State(state): State<Arc<AppState>>,
) -> Html<String> {
    let cfg = state.config.load();
    let statuses = state.fail2ban.all_statuses();

    let mut rows = String::new();
    for model in &cfg.models {
        for (i, ep) in model.endpoints.iter().enumerate() {
            let ep_key = format!("{}:{}", ep.url, mask_key(&ep.key));
            let healthy = statuses
                .iter()
                .find(|(k, _)| k == &ep_key)
                .map(|(_, ok)| *ok)
                .unwrap_or(true);

            let reason = state.fail2ban.ban_reason(&ep_key).unwrap_or_default();

            let color = if healthy { "#4caf50" } else { "#f44336" };
            let status_text = if healthy { "UP" } else { "BANNED" };

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

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".into();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}
