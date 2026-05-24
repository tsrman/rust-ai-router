use arc_swap::ArcSwap;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response, Sse},
};
use axum::response::sse::{Event, KeepAlive};
use futures::stream::Stream;
use reqwest::Client;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::auth::AuthContext;
use crate::config::AppConfig;
use crate::fail2ban::Fail2ban;
use crate::health::checker::HealthStore;
use crate::metrics::prometheus;
use crate::ratelimit::{self, RateLimiterStore};
use crate::router::{ModelRouter, SelectedEndpoint, SessionStickyStore};
use crate::stats::StatsWriter;

/// Общее состояние приложения
pub struct AppState {
    pub config: Arc<ArcSwap<AppConfig>>,
    pub client: Client,
    pub router: ModelRouter,
    pub rate_limiters: RateLimiterStore,
    pub fail2ban: Arc<Fail2ban>,
    pub sticky: SessionStickyStore,
    pub stats: Arc<StatsWriter>,
    pub sync: Arc<crate::sync::SyncStore>,
    pub health_store: Arc<HealthStore>,
    pub sticky_ttl_secs: u64,
}

/// Основной обработчик проксирования /v1/chat/completions etc.
pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
) -> Response {
    let path = req.uri().path().to_string();

    // Аутентификация
    let auth = req.extensions().get::<AuthContext>().cloned();
    let (token_key, team, _cost_multiplier, token_rpm, token_tpm) = match auth {
        Some(ctx) => (ctx.token_key, ctx.team, ctx.cost_multiplier, ctx.rpm, ctx.tpm),
        None => return (StatusCode::UNAUTHORIZED, "Authentication required").into_response(),
    };

    // Извлекаем заголовки до перемещения req
    let forwarded_headers = {
        let h = req.headers();
        let mut map = HashMap::new();
        for name in &["x-request-id", "x-session-id", "user-agent"] {
            if let Some(val) = h.get(*name) {
                map.insert(name.to_string(), val.clone());
            }
        }
        map
    };

    // Читаем тело и парсим JSON
    let (body_bytes, body_json) = match read_body_and_parse(req).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let model_name = body_json
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let is_stream = body_json
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Anthropic → OpenAI трансляция
    let is_anthropic = crate::proxy::anthropic::is_anthropic_request(&path);
    let upstream_path = if is_anthropic {
        "/v1/chat/completions"
    } else {
        path.as_str()
    };
    let (body_bytes, body_json, model_name) = if is_anthropic {
        let (translated, model) = crate::proxy::anthropic::translate_request(&body_json);
        let new_bytes = serde_json::to_vec(&translated).unwrap_or(body_bytes);
        (new_bytes, translated, model)
    } else {
        (body_bytes, body_json, model_name)
    };

    // Каноническое имя модели + алиас → подмена в теле
    let cfg = state.config.load();
    let canonical_model = cfg.canonical_model_name(&model_name);
    let body_bytes = if canonical_model != model_name {
        let mut json = body_json;
        json["model"] = serde_json::Value::String(canonical_model.clone());
        serde_json::to_vec(&json).unwrap_or(body_bytes)
    } else {
        body_bytes
    };

    // Проверка доступа
    if !cfg.token_has_model_access(&token_key, &canonical_model) {
        prometheus::REQUEST_COUNT
            .with_label_values(&[&canonical_model, "auth", "forbidden"])
            .inc();
        return (StatusCode::FORBIDDEN, format!("Token has no access to model: {model_name}"))
            .into_response();
    }

    // Собираем список забаненных ключей (один раз для модели, локально + sync)
    let model_cfg = cfg.find_model(&canonical_model);
    let mut banned_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(ref m) = model_cfg {
        for ep in &m.endpoints {
            let k = format!("{}:{}", ep.url, mask_key(&ep.key));
            if state.fail2ban.is_banned(&k) || state.sync.is_banned(&k).await {
                banned_keys.insert(k);
            }
        }
    }

    // Если все эндпоинты модели забанены — сразу 503
    if model_cfg.as_ref().map_or(false, |m| {
        m.endpoints.len() == banned_keys.len() && m.endpoints.len() > 0
    }) {
        prometheus::REQUEST_COUNT
            .with_label_values(&[&canonical_model, "router", "all_banned"])
            .inc();
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("All endpoints banned for model: {canonical_model}"),
        ).into_response();
    }

    // Session-sticky routing: проверяем бан (локально + sync)
    let session_id = get_session_id(&body_bytes);
    let endpoint = if let Some(sid) = &session_id {
        // Пробуем локальный sticky, затем sync (Redis)
        let mut sticky_idx = state.sticky.get(sid);
        if sticky_idx.is_none() {
            sticky_idx = state.sync.get_sticky(sid).await;
        }
        if let Some(sticky_idx) = sticky_idx {
            if let Some(m) = &model_cfg {
                if sticky_idx < m.endpoints.len() {
                    let ep = &m.endpoints[sticky_idx];
                    let sticky_key = format!("{}:{}", ep.url, mask_key(&ep.key));
                    if !banned_keys.contains(&sticky_key) {
                        state.sticky.touch(sid);
                        Some(build_selected_endpoint(ep, sticky_idx, m.endpoints.len()))
                    } else {
                        // Sticky endpoint забанен — очищаем привязку
                        tracing::debug!(
                            sticky_idx,
                            "Sticky endpoint banned, falling back to round-robin"
                        );
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Fallback: round-robin с пропуском забаненных
    let initial_endpoint = endpoint.or_else(|| {
        state.router.select_available(&canonical_model, &banned_keys, &std::collections::HashSet::new())
    });

    let mut current_endpoint = match initial_endpoint {
        Some(ep) => ep,
        None => {
            prometheus::REQUEST_COUNT
                .with_label_values(&[&canonical_model, "router", "no_endpoint"])
                .inc();
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("No available endpoint for model: {canonical_model}"),
            ).into_response();
        }
    };

    // Rate limiting: команда (общий бюджет) → токен (персональный)
    let team_limits = cfg.teams.iter().find(|t| t.name == team)
        .and_then(|t| t.limits.as_ref());
    let team_rpm = team_limits.map(|l| l.rpm).unwrap_or(0);
    let team_tpm = team_limits.map(|l| l.tpm).unwrap_or(0);

    if team_rpm > 0 {
        let team_key = format!("team:{team}");
        let sync_rl = state.sync.check_rate_limit("team", &team_key, team_rpm as u64).await;
        if !sync_rl.allowed {
            prometheus::RATE_LIMIT_HITS.with_label_values(&["team", &team_key]).inc();
            return rate_limit_response(&sync_rl, &format!("Team '{team}' (shared)"));
        }
    }
    if team_rpm > 0 || team_tpm > 0 {
        let team_key = format!("team:{team}");
        let team_rl = state.rate_limiters.check(
            &team_key, team_rpm, team_tpm, 1,
            ratelimit::RateLimitScope::Token,
        );
        if !team_rl.allowed {
            prometheus::RATE_LIMIT_HITS.with_label_values(&["team", &team_key]).inc();
            return rate_limit_response(&team_rl, &format!("Team '{team}'"));
        }
    }

    // Rate limiting: токен (sync → локальный)
    if token_rpm > 0 {
        let sync_rl = state.sync.check_rate_limit("token", &token_key, token_rpm as u64).await;
        if !sync_rl.allowed {
            prometheus::RATE_LIMIT_HITS.with_label_values(&["token", &token_key]).inc();
            return rate_limit_response(&sync_rl, "Token (shared)");
        }
    }
    let token_rl = state.rate_limiters.check(
        &token_key, token_rpm, token_tpm, 1,
        ratelimit::RateLimitScope::Token,
    );
    if !token_rl.allowed {
        prometheus::RATE_LIMIT_HITS.with_label_values(&["token", &token_key]).inc();
        return rate_limit_response(&token_rl, "Token");
    }

    // ── Ретрай-луп ──────────────────────────────────────────────────
    let retry_on_failure = cfg.fail2ban.retry_on_failure;
    let mut tried_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut last_error_response: Option<Response> = None;
    let _ = &mut last_error_response; // read in retry-loop exit below

    loop {
        let ep_key = format!("{}:{}", current_endpoint.url, mask_key(&current_endpoint.api_key));

        // Rate limiting: эндпоинт (sync → локальный)
        if current_endpoint.endpoint_limits_rpm > 0 || current_endpoint.endpoint_limits_tpm > 0 {
            let ep_rl_key = format!("ep:{}", ep_key);

            // Sync (Redis) rate limit — shared across instances
            if current_endpoint.endpoint_limits_rpm > 0 {
                let sync_rl = state.sync.check_rate_limit("ep", &ep_key, current_endpoint.endpoint_limits_rpm as u64).await;
                if !sync_rl.allowed {
                    let key_copy = ep_key.clone();
                    prometheus::RATE_LIMIT_HITS.with_label_values(&["endpoint", &key_copy]).inc();
                    tracing::debug!(endpoint = %key_copy, "Endpoint sync rate limited, trying next");
                    tried_keys.insert(key_copy);
                    match state.router.select_available(&canonical_model, &banned_keys, &tried_keys) {
                        Some(ep) => { current_endpoint = ep; continue; }
                        None => {
                            prometheus::REQUEST_COUNT
                                .with_label_values(&[&canonical_model, "router", "all_failed"]).inc();
                            return rate_limit_response(&sync_rl, "Endpoint (shared)");
                        }
                    }
                }
            }

            let ep_rl = state.rate_limiters.check(
                &ep_rl_key,
                current_endpoint.endpoint_limits_rpm,
                current_endpoint.endpoint_limits_tpm,
                1,
                ratelimit::RateLimitScope::Endpoint,
            );
            if !ep_rl.allowed {
                // Rate limited — пробуем следующий
                let key_copy = ep_key.clone();
                prometheus::RATE_LIMIT_HITS.with_label_values(&["endpoint", &key_copy]).inc();
                tracing::debug!(endpoint = %key_copy, "Endpoint rate limited, trying next");
                tried_keys.insert(key_copy);
                // Переход к следующему эндпоинту
                match state.router.select_available(&canonical_model, &banned_keys, &tried_keys) {
                    Some(ep) => current_endpoint = ep,
                    None => {
                        prometheus::REQUEST_COUNT
                            .with_label_values(&[&canonical_model, "router", "all_failed"])
                            .inc();
                        return rate_limit_response(&ep_rl, "Endpoint");
                    }
                }
                continue;
            }
        }

        // Upstream URL
        let upstream_url = format!("{}{}", current_endpoint.url.trim_end_matches('/'), upstream_path);

        let start = std::time::Instant::now();
        let result = if is_stream {
            proxy_streaming(&state.client, &upstream_url, &current_endpoint.api_key, &body_bytes).await
        } else {
            proxy_regular(&state.client, &upstream_url, &current_endpoint.api_key, &body_bytes, &forwarded_headers).await
        };
        let latency = start.elapsed();

        match result {
            Ok(upstream_resp) => {
                let upstream_status = upstream_resp.status().as_u16();
                let is_fail = state.fail2ban.is_fail_status(upstream_status);
                let should_retry = retry_on_failure && is_fail;

                if is_fail {
                    let just_banned = state.fail2ban.record_failure_with_code(&ep_key, upstream_status);
                    tracing::warn!(status = upstream_status, endpoint = %ep_key, banned = just_banned, "Upstream error");
                    if just_banned {
                        let sync = state.sync.clone();
                        let ep = ep_key.clone();
                        tokio::spawn(async move {
                            sync.set_ban(&ep, 60).await;
                            sync.publish_ban(&ep, 60).await;
                        });
                    }
                } else {
                    state.fail2ban.record_success(&ep_key);
                }

                if upstream_status < 400 || !should_retry {
                    // Успех или не-retryable ошибка → возвращаем клиенту
                    if let Some(sid) = &session_id {
                        state.sticky.set(sid, current_endpoint.index);
                        // Синхронизируем sticky в Redis (fire-and-forget)
                        let sync = state.sync.clone();
                        let sid_clone = sid.clone();
                        let idx = current_endpoint.index;
                        let ttl = state.sticky_ttl_secs;
                        tokio::spawn(async move {
                            sync.set_sticky(&sid_clone, idx, ttl).await;
                        });
                    }

                    prometheus::REQUEST_COUNT
                        .with_label_values(&[&canonical_model, &ep_key, &upstream_status.to_string()])
                        .inc();
                    prometheus::REQUEST_LATENCY
                        .with_label_values(&[&canonical_model, &ep_key])
                        .observe(latency.as_secs_f64());

                    let mut resp = upstream_resp;

                    // Читаем тело для извлечения usage и Anthropic-перевода
                    let resp_body_bytes = axum::body::to_bytes(
                        std::mem::replace(resp.body_mut(), axum::body::Body::empty()),
                        1024 * 1024,
                    ).await.unwrap_or_default();

                    // Извлекаем usage из ответа
                    let (tokens_prompt, tokens_completion) = serde_json::from_slice::<serde_json::Value>(&resp_body_bytes)
                        .ok()
                        .and_then(|v| {
                            let usage = v.get("usage")?;
                            Some((
                                usage.get("prompt_tokens")?.as_u64().unwrap_or(0),
                                usage.get("completion_tokens")?.as_u64().unwrap_or(0),
                            ))
                        })
                        .unwrap_or((0, 0));

                    // Хеш токена для статистики
                    let token_hash = {
                        use std::hash::Hasher;
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        std::hash::Hash::hash(&token_key, &mut h);
                        format!("{:x}", h.finish())
                    };

                    // Используем уже прочитанные байты (без клонирования)

                    // Anthropic: перевести ответ OpenAI → Anthropic (до построения ответа)
                    if is_anthropic {
                        if let Ok(openai_json) = serde_json::from_slice::<serde_json::Value>(&resp_body_bytes) {
                            let anthropic_json = crate::proxy::anthropic::translate_response(&openai_json);
                            let new_body = serde_json::to_vec(&anthropic_json).unwrap_or_default();
                            return axum::response::Response::builder()
                                .status(upstream_status)
                                .header("content-type", "application/json")
                                .body(axum::body::Body::from(new_body))
                                .unwrap_or(resp);
                        }
                    }

                    // Строим ответ с заголовками
                    resp = axum::response::Response::builder()
                        .status(upstream_status)
                        .body(axum::body::Body::from(resp_body_bytes))
                        .unwrap_or(resp);

                    let headers = resp.headers_mut();
                    insert_header(headers, "x-endpoint-used", &current_endpoint.url);
                    insert_header(headers, "x-endpoint-index", &current_endpoint.index.to_string());
                    insert_header(headers, "x-latency-ms", &latency.as_millis().to_string());
                    insert_header(headers, "x-instance", &std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into()));
                    if current_endpoint.cost_prompt > 0.0 || current_endpoint.cost_completion > 0.0 {
                        insert_header(headers, "x-cost-prompt-per-1m", &current_endpoint.cost_prompt.to_string());
                        insert_header(headers, "x-cost-completion-per-1m", &current_endpoint.cost_completion.to_string());
                    }

                    // Fire-and-forget: запись статистики в БД
                    let stats = state.stats.clone();
                    let model = canonical_model.clone();
                    let ep_url = current_endpoint.url.clone();
                    let team_name = team.clone();
                    let cp = current_endpoint.cost_prompt;
                    let cc = current_endpoint.cost_completion;
                    let latency = latency.as_millis() as u64;
                    let th = token_hash.clone();
                    tokio::spawn(async move {
                        stats.record_request(&model, &ep_url, &team_name, tokens_prompt, tokens_completion, latency, upstream_status as u16, Some(&th), cp, cc).await;
                    });

                    return resp;
                }

                // Retryable ошибка — пробуем следующий
                let key_copy = ep_key.clone();
                tracing::info!(
                    status = upstream_status,
                    endpoint = %key_copy,
                    tried = tried_keys.len() + 1,
                    "Retrying on next endpoint"
                );
                tried_keys.insert(key_copy);
                last_error_response = Some(upstream_resp);
            }
            Err(e) => {
                let key_copy = ep_key.clone();
                let just_banned = state.fail2ban.record_failure(&key_copy);
                tracing::error!(endpoint = %key_copy, error = %e, banned = just_banned, "Upstream network error");
                if just_banned {
                    let sync = state.sync.clone();
                    let ep = key_copy.clone();
                    tokio::spawn(async move {
                        sync.set_ban(&ep, 60).await;
                        sync.publish_ban(&ep, 60).await;
                    });
                }

                if retry_on_failure {
                    tried_keys.insert(key_copy);
                    last_error_response = Some(
                        (StatusCode::BAD_GATEWAY, format!("Upstream error: {e}")).into_response()
                    );
                } else {
                    prometheus::REQUEST_COUNT
                        .with_label_values(&[&canonical_model, &key_copy, "error"])
                        .inc();
                    return (StatusCode::BAD_GATEWAY, format!("Upstream error: {e}")).into_response();
                }
            }
        }

        // Ищем следующий эндпоинт
        match state.router.select_available(&canonical_model, &banned_keys, &tried_keys) {
            Some(ep) => current_endpoint = ep,
            None => {
                // Все перебрали — возвращаем последнюю ошибку
                prometheus::REQUEST_COUNT
                    .with_label_values(&[&canonical_model, "router", "all_failed"])
                    .inc();
                return last_error_response.unwrap_or_else(|| {
                    (StatusCode::SERVICE_UNAVAILABLE, "All endpoints failed").into_response()
                });
            }
        }
    }
}

// ── Вспомогательные функции ────────────────────────────────────────────

fn build_selected_endpoint(ep: &crate::config::EndpointConfig, index: usize, total: usize) -> SelectedEndpoint {
    SelectedEndpoint {
        url: ep.url.clone(),
        api_key: ep.key.clone(),
        index,
        total_endpoints: total,
        cost_prompt: ep.cost.as_ref().map(|c| c.prompt).unwrap_or(0.0),
        cost_completion: ep.cost.as_ref().map(|c| c.completion).unwrap_or(0.0),
        endpoint_limits_rpm: ep.limits.as_ref().map(|l| l.rpm).unwrap_or(0),
        endpoint_limits_tpm: ep.limits.as_ref().map(|l| l.tpm).unwrap_or(0),
    }
}

fn insert_header(headers: &mut axum::http::HeaderMap, name: &str, value: &str) {
    if let (Ok(hname), Ok(hval)) = (
        header::HeaderName::from_bytes(name.as_bytes()),
        header::HeaderValue::from_str(value),
    ) {
        headers.insert(hname, hval);
    }
}

/// Читаем тело запроса и парсим JSON (единоразово)
async fn read_body_and_parse(req: Request<Body>) -> Result<(Vec<u8>, serde_json::Value), Response> {
    let bytes = axum::body::to_bytes(req.into_body(), 50 * 1024 * 1024)  // 50MB hard cap
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed to read body: {e}")).into_response())?;

    let json: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid JSON: {e}")).into_response())?;

    Ok((bytes.to_vec(), json))
}

fn get_session_id(body: &[u8]) -> Option<String> {
    let body_str = String::from_utf8_lossy(body);
    serde_json::from_str::<serde_json::Value>(&body_str)
        .ok()
        .and_then(|v| v.get("x-sticky-session-id").and_then(|s| s.as_str().map(String::from)))
}

/// Обычное (не-streaming) проксирование
async fn proxy_regular(
    client: &Client,
    url: &str,
    api_key: &str,
    body: &[u8],
    forwarded_headers: &HashMap<String, axum::http::HeaderValue>,
) -> Result<Response, anyhow::Error> {
    let body_value: serde_json::Value = serde_json::from_slice(body)?;

    let mut req_builder = client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body_value);

    for name in &["x-request-id", "x-session-id", "user-agent"] {
        if let Some(val) = forwarded_headers.get(*name) {
            req_builder = req_builder.header(*name, val);
        }
    }

    let upstream_resp = req_builder.send().await?;
    let status = upstream_resp.status();
    let upstream_headers = upstream_resp.headers().clone();
    let upstream_body = upstream_resp.bytes().await?;

    let mut response = Response::builder().status(status);

    for (key, val) in upstream_headers.iter() {
        if key != "transfer-encoding" && key != "content-encoding" {
            if let Some(hdr) = response.headers_mut() {
                if let Ok(name) = header::HeaderName::from_bytes(key.as_str().as_bytes()) {
                    hdr.insert(name, val.clone());
                }
            }
        }
    }

    Ok(response
        .body(Body::from(upstream_body))
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to build response").into_response()
        }))
}

/// SSE streaming проксирование
async fn proxy_streaming(
    client: &Client,
    url: &str,
    api_key: &str,
    body: &[u8],
) -> Result<Response, anyhow::Error> {
    let body_value: serde_json::Value = serde_json::from_slice(body)?;

    let upstream_resp = client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body_value)
        .send()
        .await?;

    if !upstream_resp.status().is_success() {
        let status = upstream_resp.status();
        let err_body = upstream_resp.text().await.unwrap_or_default();
        let mut resp = (status, err_body).into_response();
        resp.headers_mut().insert("content-type", "application/json".parse().unwrap());
        return Ok(resp);
    }

    let byte_stream = upstream_resp.bytes_stream();

    let sse_stream = SseStream {
        inner: Box::pin(byte_stream),
        buffer: Vec::new(),
    };

    Ok(Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// Поток, преобразующий байтовый поток reqwest в SSE события axum
struct SseStream {
    inner: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
    buffer: Vec<u8>,
}

impl Stream for SseStream {
    type Item = Result<Event, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match self.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    self.buffer.extend_from_slice(&chunk);

                    while let Some(pos) = find_sse_boundary(&self.buffer) {
                        let event_bytes = self.buffer[..pos].to_vec();
                        self.buffer.drain(..pos + 2);

                        let event_str = String::from_utf8_lossy(&event_bytes);
                        return Poll::Ready(Some(Ok(Event::default().data(event_str.to_string()))));
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    tracing::error!("SSE stream error: {e}");
                    return Poll::Ready(Some(Ok(Event::default().data(
                        format!(r#"{{"error":"{e}"}}"#)
                    ))));
                }
                Poll::Ready(None) => {
                    if !self.buffer.is_empty() {
                        let remaining = String::from_utf8_lossy(&self.buffer).to_string();
                        self.buffer.clear();
                        return Poll::Ready(Some(Ok(Event::default().data(remaining))));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn find_sse_boundary(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(1) {
        if data[i] == b'\n' && data[i + 1] == b'\n' {
            return Some(i);
        }
    }
    None
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".into();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}

/// Собрать OpenAI-совместимый ответ с заголовками при превышении rate limit.
pub fn rate_limit_response(rl: &ratelimit::RateLimitResult, scope: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": format!("Rate limit exceeded for {scope} ({rl_limit} per minute). Retry after {retry:.1}s",
                rl_limit = rl.limit,
                retry = rl.reset_after_secs
            ),
            "type": "rate_limit_exceeded",
            "param": null,
            "code": "rate_limit_exceeded"
        }
    });

    let mut resp = (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response();

    let headers = resp.headers_mut();
    // Стандартные заголовки rate limit
    insert_header(headers, "x-ratelimit-limit-requests", &rl.limit.to_string());
    insert_header(headers, "x-ratelimit-remaining-requests", "0");
    insert_header(
        headers,
        "x-ratelimit-reset-requests",
        &format!("{:.1}", rl.reset_after_secs),
    );
    // Дополнительные TPM-заголовки
    insert_header(headers, "x-ratelimit-limit-tokens", &rl.limit.to_string());
    insert_header(headers, "x-ratelimit-remaining-tokens", "0");
    insert_header(
        headers,
        "x-ratelimit-reset-tokens",
        &format!("{:.1}", rl.reset_after_secs),
    );

    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_session_id_present() {
        let body = br#"{"model":"gpt-4","messages":[],"x-sticky-session-id":"abc-123"}"#;
        assert_eq!(get_session_id(body), Some("abc-123".into()));
    }

    #[test]
    fn test_get_session_id_missing() {
        let body = br#"{"model":"gpt-4","messages":[]}"#;
        assert_eq!(get_session_id(body), None);
    }

    #[test]
    fn test_get_session_id_invalid_json() {
        let body = b"not json";
        assert_eq!(get_session_id(body), None);
    }

    #[test]
    fn test_mask_key_short() {
        assert_eq!(mask_key("abc"), "***");
    }

    #[test]
    fn test_mask_key_normal() {
        let masked = mask_key("sk-1234567890abcdef");
        assert!(masked.starts_with("sk-1"));
        assert!(masked.ends_with("cdef"));
        assert!(masked.contains("..."));
    }

    #[test]
    fn test_build_selected_endpoint() {
        let ep = crate::config::EndpointConfig {
            url: "https://api.example.com".into(),
            key: "sk-test".into(),
            weight: 1,
            cost: Some(crate::config::CostConfig { prompt: 0.5, completion: 1.5 }),
            limits: Some(crate::config::LimitsConfig { rpm: 10, tpm: 1000 }),
            headers: std::collections::HashMap::new(),
        };
        let sel = build_selected_endpoint(&ep, 2, 5);
        assert_eq!(sel.url, "https://api.example.com");
        assert_eq!(sel.api_key, "sk-test");
        assert_eq!(sel.index, 2);
        assert_eq!(sel.total_endpoints, 5);
        assert_eq!(sel.cost_prompt, 0.5);
        assert_eq!(sel.cost_completion, 1.5);
        assert_eq!(sel.endpoint_limits_rpm, 10);
        assert_eq!(sel.endpoint_limits_tpm, 1000);
    }

    #[test]
    fn test_build_selected_endpoint_no_limits() {
        let ep = crate::config::EndpointConfig {
            url: "https://api.example.com".into(),
            key: "sk-test".into(),
            weight: 1,
            cost: None,
            limits: None,
            headers: std::collections::HashMap::new(),
        };
        let sel = build_selected_endpoint(&ep, 0, 1);
        assert_eq!(sel.cost_prompt, 0.0);
        assert_eq!(sel.endpoint_limits_rpm, 0);
    }

    #[test]
    fn test_rate_limit_response_format() {
        let rl = ratelimit::RateLimitResult {
            allowed: false,
            limit: 10,
            reset_after_secs: 30.5,
            scope: ratelimit::RateLimitScope::Token,
        };
        let resp = rate_limit_response(&rl, "Token");
        assert_eq!(resp.status().as_u16(), 429);
        let headers = resp.headers();
        assert_eq!(headers.get("x-ratelimit-limit-requests").unwrap(), "10");
        assert_eq!(headers.get("x-ratelimit-remaining-requests").unwrap(), "0");
        assert!(headers.get("x-ratelimit-reset-requests").unwrap().to_str().unwrap().starts_with("30."));
    }
}

/// Generic proxy for all vLLM / OpenAI-compatible endpoints
/// (/v1/completions, /v1/embeddings, /v1/rerank, /v1/tokenize, etc.)
pub async fn proxy_generic(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request<Body>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let auth = match req.extensions().get::<AuthContext>() {
        Some(ctx) => ctx.clone(),
        None => return (StatusCode::UNAUTHORIZED, "Authentication required").into_response(),
    };

    let cfg = state.config.load();

    // Team rate-limit
    let team_limits = cfg.teams.iter().find(|t| t.name == auth.team)
        .and_then(|t| t.limits.as_ref());
    let team_rpm = team_limits.map(|l| l.rpm).unwrap_or(0);
    let team_tpm = team_limits.map(|l| l.tpm).unwrap_or(0);

    if team_rpm > 0 {
        let team_key = format!("team:{}", auth.team);
        let sync_rl = state.sync.check_rate_limit("team", &team_key, team_rpm as u64).await;
        if !sync_rl.allowed {
            return rate_limit_response(&sync_rl, &format!("Team '{}' (shared)", auth.team));
        }
    }
    if team_rpm > 0 || team_tpm > 0 {
        let team_key = format!("team:{}", auth.team);
        let team_rl = state.rate_limiters.check(&team_key, team_rpm, team_tpm, 1, ratelimit::RateLimitScope::Token);
        if !team_rl.allowed {
            return rate_limit_response(&team_rl, &format!("Team '{}'", auth.team));
        }
    }

    // Token rate-limit
    if auth.rpm > 0 {
        let sync_rl = state.sync.check_rate_limit("token", &auth.token_key, auth.rpm as u64).await;
        if !sync_rl.allowed {
            return rate_limit_response(&sync_rl, "Token (shared)");
        }
    }
    let token_rl = state.rate_limiters.check(&auth.token_key, auth.rpm, auth.tpm, 1, ratelimit::RateLimitScope::Token);
    if !token_rl.allowed {
        return rate_limit_response(&token_rl, "Token");
    }

    // Read body (extract path first — req moves into read_body_and_parse)
    let req_path = req.uri().path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());

    let content_type = req.headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let (body_bytes, model_name) = if content_type.starts_with("application/json") {
        let (bytes, json) = match read_body_and_parse(req).await {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        let model = json.get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        (bytes, model)
    } else {
        // Non-JSON body — выбираем модель по типу из пути
        let bytes = match axum::body::to_bytes(req.into_body(), 50 * 1024 * 1024).await {
            Ok(b) => b.to_vec(),
            Err(e) => return (StatusCode::BAD_REQUEST, format!("Failed to read body: {e}")).into_response(),
        };
        let model_type = req_path_type(&req_path);
        let model = cfg.models.iter()
            .find(|m| m.model_type == model_type)
            .map(|m| m.name.clone())
            .or_else(|| cfg.models.first().map(|m| m.name.clone()));
        match model {
            Some(m) => (bytes, m),
            None => return (StatusCode::SERVICE_UNAVAILABLE, "No models configured").into_response(),
        }
    };
    let canonical_model = cfg.canonical_model_name(&model_name);

    // Check model access
    if !cfg.token_has_model_access(&auth.token_key, &canonical_model) {
        return (StatusCode::FORBIDDEN, format!("Model '{}' not allowed for this token", canonical_model)).into_response();
    }

    // Select endpoint
    let Some(current_endpoint) = state.router.select_endpoint(&canonical_model) else {
        return (StatusCode::SERVICE_UNAVAILABLE, format!("No endpoint for model: {}", canonical_model)).into_response();
    };

    // Build URL: proxy path relative to endpoint base URL
    let upstream_url = format!("{}{}", current_endpoint.url.trim_end_matches('/'), req_path);

    // Proxy raw (don't parse response — just forward)
    let start = std::time::Instant::now();
    match proxy_raw(
        &state.client,
        &upstream_url,
        &current_endpoint.api_key,
        &body_bytes,
    ).await {
        Ok(resp) => {
            let latency = start.elapsed();
            let status = resp.status().as_u16();

            // Fire-and-forget stats (minimal — no token counting)
            let stats = state.stats.clone();
            let model = canonical_model.clone();
            let ep_url = current_endpoint.url.clone();
            let team_name = auth.team.clone();
            let latency_ms = latency.as_millis() as u64;
            tokio::spawn(async move {
                stats.record_request(&model, &ep_url, &team_name, 0, 0, latency_ms, status, None, 0.0, 0.0).await;
            });

            let mut resp = resp;
            let headers = resp.headers_mut();
            insert_header(headers, "x-endpoint-used", &current_endpoint.url);
            insert_header(headers, "x-endpoint-index", &current_endpoint.index.to_string());
            insert_header(headers, "x-latency-ms", &latency_ms.to_string());

            // Record success/fail for fail2ban
            if status >= 400 && state.fail2ban.is_fail_status(status) {
                let ep_key = format!("{}:{}", current_endpoint.url, mask_key(&current_endpoint.api_key));
                state.fail2ban.record_failure_with_code(&ep_key, status);
            } else {
                let ep_key = format!("{}:{}", current_endpoint.url, mask_key(&current_endpoint.api_key));
                state.fail2ban.record_success(&ep_key);
            }

            resp
        }
        Err(e) => {
            let ep_key = format!("{}:{}", current_endpoint.url, mask_key(&current_endpoint.api_key));
            state.fail2ban.record_failure(&ep_key);
            (StatusCode::BAD_GATEWAY, format!("Upstream error: {e}")).into_response()
        }
    }
}

/// Proxy raw bytes to upstream (no JSON parsing, no OpenAI-specific logic)
async fn proxy_raw(
    client: &Client,
    url: &str,
    api_key: &str,
    body: &[u8],
) -> Result<Response<Body>, anyhow::Error> {
    let resp = client
        .post(url)
        .header("Authorization", format!("Bearer {api_key}"))
        .body(body.to_vec())
        .send()
        .await?;

    let status = resp.status();
    let headers = resp.headers().clone();
    let body_bytes = resp.bytes().await?;

    Ok(Response::builder()
        .status(status)
        .body(Body::from(body_bytes))?)
}

/// Определить тип модели по пути запроса
fn req_path_type(path: &str) -> &str {
    if path.contains("/audio/") { "audio" }
    else if path.contains("/embeddings") { "embedding" }
    else if path.contains("/rerank") { "rerank" }
    else if path.contains("/completions") && !path.contains("/chat/") { "completions" }
    else { "chat" }
}
