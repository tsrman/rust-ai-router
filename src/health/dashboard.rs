use axum::{
    extract::{Extension, State},
    response::{Html, Json},
};
use serde_json::json;
use std::sync::Arc;

use crate::auth::AuthContext;
use crate::proxy::handler::AppState;

/// GET /health — JSON status
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
                let requests = health.as_ref().map(|h| h.request_count).unwrap_or(0);
                let errors = health.as_ref().map(|h| h.error_count).unwrap_or(0);
                let latency_ms = health.as_ref().map(|h| h.last_latency_ms).unwrap_or(0);

                json!({
                    "model": model.name,
                    "endpoint_index": i,
                    "url": crate::utils::mask_endpoint_url(&ep.url),
                    "healthy": healthy,
                    "banned": banned,
                    "last_error": last_error,
                    "requests": requests,
                    "errors": errors,
                    "last_latency_ms": latency_ms,
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

/// GET /vhealth — HTML dashboard
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

            let requests = health.as_ref().map(|h| h.request_count).unwrap_or(0);
            let errors = health.as_ref().map(|h| h.error_count).unwrap_or(0);
            let success_rate = if requests > 0 {
                ((requests - errors) as f64 / requests as f64 * 100.0).round()
            } else {
                100.0
            };
            let latency_ms = health.as_ref().map(|h| h.last_latency_ms).unwrap_or(0);
            let last_check_secs = health.as_ref()
                .map(|h| h.last_check.elapsed().as_secs())
                .unwrap_or(9999);

            rows.push_str(&format!(
                "<tr>
                    <td>{}</td>
                    <td>{}</td>
                    <td>{}</td>
                    <td style='color:{}; font-weight:bold'>{}</td>
                    <td>{}</td>
                    <td>{}</td>
                    <td>{}</td>
                    <td>{:.0}%</td>
                    <td>{} ms</td>
                    <td>{} s</td>
                    <td>{}</td>
                    <td>{}</td>
                </tr>",
                model.name,
                i,
                crate::utils::mask_endpoint_url(&ep.url),
                color,
                status_text,
                reason,
                requests,
                errors,
                success_rate,
                latency_ms,
                last_check_secs,
                ep.limits.as_ref().map(|l| l.rpm).unwrap_or(0),
                ep.limits.as_ref().map(|l| l.tpm).unwrap_or(0),
            ));
        }
    }

    let total_endpoints: usize = cfg.models.iter().map(|m| m.endpoints.len()).sum();
    let total_requests = state.monitoring.health_store.total_requests();
    let total_errors = state.monitoring.health_store.total_errors();
    let banned_count = state.limits.fail2ban.banned_count();

    let base_prefix = if state.base_path.is_empty() {
        String::new()
    } else {
        format!("/{}", state.base_path.trim_start_matches('/'))
    };

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
        table {{ width: 100%; border-collapse: collapse; margin-top: 20px; font-size: 0.9em; }}
        th {{ background: #16213e; padding: 10px; text-align: left; }}
        td {{ padding: 8px; border-bottom: 1px solid #333; }}
        tr:hover {{ background: #16213e; }}
        .summary {{ display: flex; gap: 15px; margin: 20px 0; flex-wrap: wrap; }}
        .card {{ background: #16213e; padding: 12px 20px; border-radius: 8px; min-width: 120px; }}
        .card .value {{ font-size: 1.8em; font-weight: bold; color: #7c4dff; }}
        .card .label {{ font-size: 0.85em; color: #aaa; }}
        .legend {{ margin-top: 10px; font-size: 0.9em; color: #888; }}
        .legend span {{ margin-right: 15px; }}
    </style>
</head>
<body>
    <h1>🔄 OpenAI Router</h1>
    <div class="summary">
        <div class="card"><div class="label">Models</div><div class="value">{}</div></div>
        <div class="card"><div class="label">Endpoints</div><div class="value">{}</div></div>
        <div class="card"><div class="label">Tokens</div><div class="value">{}</div></div>
        <div class="card"><div class="label">Teams</div><div class="value">{}</div></div>
        <div class="card"><div class="label">Total Requests</div><div class="value">{}</div></div>
        <div class="card"><div class="label">Total Errors</div><div class="value" style="color:#f44336">{}</div></div>
        <div class="card"><div class="label">Banned</div><div class="value" style="color:#ff9800">{}</div></div>
    </div>
    <div class="legend">
        <span style='color:#4caf50'>● UP</span>
        <span style='color:#ff9800'>● DOWN</span>
        <span style='color:#f44336'>● BANNED</span>
    </div>
    <table>
        <tr>
            <th>Model</th>
            <th>EP #</th>
            <th>URL</th>
            <th>Status</th>
            <th>Reason</th>
            <th>Reqs</th>
            <th>Errs</th>
            <th>Success</th>
            <th>Latency</th>
            <th>Check</th>
            <th>RPM</th>
            <th>TPM</th>
        </tr>
        {}
    </table>
    <p style="margin-top:20px; color:#666">Auto-refresh: 10s | <a href="{}/health" style="color:#7c4dff">JSON API</a> | <a href="{}/metrics" style="color:#7c4dff">Prometheus</a></p>
</body>
</html>"#,
        cfg.models.len(),
        total_endpoints,
        cfg.tokens.len(),
        cfg.teams.len(),
        total_requests,
        total_errors,
        banned_count,
        rows,
        base_prefix,
        base_prefix,
    ))
}

/// GET /stats — token/team statistics (admin sees everything)
pub async fn stats_json(
    State(state): State<Arc<AppState>>,
    Extension(auth): Extension<AuthContext>,
) -> Json<serde_json::Value> {
    let is_admin = auth.models.iter().any(|m| m == "*");

    let token_reqs = state.live_stats.requests_by_token.get(&auth.token_key).map(|v| *v).unwrap_or(0);
    let token_errs = state.live_stats.errors_by_token.get(&auth.token_key).map(|v| *v).unwrap_or(0);

    let team_reqs = state.live_stats.requests_by_team.get(&auth.team).map(|v| *v).unwrap_or(0);
    let team_errs = state.live_stats.errors_by_team.get(&auth.team).map(|v| *v).unwrap_or(0);

    let cfg = state.config.load();

    // Token / team TPM state
    let token_tpm_limit = auth.tpm as u64;
    let token_tpm_remaining = if token_tpm_limit > 0 {
        state.limits.limiters.get_tpm_state(&auth.token_key, token_tpm_limit).1
    } else {
        0
    };

    let team_limits = cfg.teams.iter().find(|t| t.name == auth.team).and_then(|t| t.limits.as_ref());
    let team_tpm_limit = team_limits.map(|l| l.tpm).unwrap_or(0);
    let team_tpm_remaining = if team_tpm_limit > 0 {
        let team_key = format!("team:{}", auth.team);
        state.limits.limiters.get_tpm_state(&team_key, team_tpm_limit).1
    } else {
        0
    };

    let endpoints: Vec<serde_json::Value> = if is_admin {
        cfg.models.iter().flat_map(|model| {
            model.endpoints.iter().map(|ep| {
                let ep_key = format!("{}:{}", ep.url, mask_key(&ep.key));
                let reqs = state.live_stats.requests_by_endpoint.get(&ep_key).map(|v| *v).unwrap_or(0);
                let errs = state.live_stats.errors_by_endpoint.get(&ep_key).map(|v| *v).unwrap_or(0);
                let banned = state.limits.fail2ban.is_banned(&ep_key);
                let health = state.monitoring.health_store.get(&ep_key);
                let healthy = health.as_ref().map(|h| h.healthy).unwrap_or(true);
                let ep_tpm_limit = ep.limits.as_ref().map(|l| l.tpm).unwrap_or(0);
                let ep_tpm_remaining = if ep_tpm_limit > 0 {
                    let ep_rl_key = format!("ep:{}", ep_key);
                    state.limits.limiters.get_tpm_state(&ep_rl_key, ep_tpm_limit).1
                } else {
                    0
                };
                json!({
                    "model": model.name,
                    "url": crate::utils::mask_endpoint_url(&ep.url),
                    "requests": reqs,
                    "errors": errs,
                    "banned": banned,
                    "healthy": healthy,
                    "tpm_limit": ep_tpm_limit,
                    "tpm_remaining": ep_tpm_remaining,
                })
            })
        }).collect()
    } else {
        cfg.models
            .iter()
            .filter(|m| cfg.token_has_model_access(&auth.token_key, &m.name))
            .flat_map(|model| {
                model.endpoints.iter().map(|ep| {
                    let ep_key = format!("{}:{}", ep.url, mask_key(&ep.key));
                    let reqs = state.live_stats.requests_by_endpoint.get(&ep_key).map(|v| *v).unwrap_or(0);
                    let errs = state.live_stats.errors_by_endpoint.get(&ep_key).map(|v| *v).unwrap_or(0);
                    let banned = state.limits.fail2ban.is_banned(&ep_key);
                    let health = state.monitoring.health_store.get(&ep_key);
                    let healthy = health.as_ref().map(|h| h.healthy).unwrap_or(true);
                    let ep_tpm_limit = ep.limits.as_ref().map(|l| l.tpm).unwrap_or(0);
                    let ep_tpm_remaining = if ep_tpm_limit > 0 {
                        let ep_rl_key = format!("ep:{}", ep_key);
                        state.limits.limiters.get_tpm_state(&ep_rl_key, ep_tpm_limit).1
                    } else {
                        0
                    };
                    json!({
                        "model": model.name,
                        "url": crate::utils::mask_endpoint_url(&ep.url),
                        "requests": reqs,
                        "errors": errs,
                        "banned": banned,
                        "healthy": healthy,
                        "tpm_limit": ep_tpm_limit,
                        "tpm_remaining": ep_tpm_remaining,
                    })
                })
            })
            .collect()
    };

    let mut resp = json!({
        "token_key": auth.token_key,
        "team": auth.team,
        "is_admin": is_admin,
        "token_requests": token_reqs,
        "token_errors": token_errs,
        "token_tpm_limit": token_tpm_limit,
        "token_tpm_remaining": token_tpm_remaining,
        "team_requests": team_reqs,
        "team_errors": team_errs,
        "team_tpm_limit": team_tpm_limit,
        "team_tpm_remaining": team_tpm_remaining,
        "endpoints": endpoints,
    });

    if is_admin {
        let all_tokens: serde_json::Map<String, serde_json::Value> = state
            .live_stats
            .requests_by_token
            .iter()
            .map(|entry| {
                let key = entry.key().clone();
                let reqs = *entry.value();
                let errs = state.live_stats.errors_by_token.get(&key).map(|v| *v).unwrap_or(0);
                (key, json!({ "requests": reqs, "errors": errs }))
            })
            .collect();

        let all_teams: serde_json::Map<String, serde_json::Value> = state
            .live_stats
            .requests_by_team
            .iter()
            .map(|entry| {
                let key = entry.key().clone();
                let reqs = *entry.value();
                let errs = state.live_stats.errors_by_team.get(&key).map(|v| *v).unwrap_or(0);
                (key, json!({ "requests": reqs, "errors": errs }))
            })
            .collect();

        resp["all_tokens"] = all_tokens.into();
        resp["all_teams"] = all_teams.into();
    }

    Json(resp)
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".into();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}


