//! Фоновая проверка доступности ВСЕХ эндпоинтов.
//!
//! Раз в `interval_secs` отправляет лёгкий запрос (`GET /v1/models`) на каждый
//! эндпоинт. Результат сохраняется в `HealthStore` для дашборда.
//! При успешном ответе на забаненный эндпоинт — снимает бан.
//! При ошибке на незабаненный — записывает failure (может привести к бану).

use arc_swap::ArcSwap;
use dashmap::DashMap;
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;

use crate::config::AppConfig;
use crate::fail2ban::Fail2ban;
use crate::utils::mask_key;
/// Состояние одного эндпоинта
#[derive(Debug, Clone)]
pub struct EndpointHealth {
    pub healthy: bool,
    pub last_error: String,
}

/// Хранилище статусов здоровья эндпоинтов (ключ = "url:masked_key")
#[derive(Default)]
pub struct HealthStore {
    states: DashMap<String, EndpointHealth>,
}

impl HealthStore {
    pub fn new() -> Self {
        Self { states: DashMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<EndpointHealth> {
        self.states.get(key).map(|e| e.clone())
    }

    pub fn set_healthy(&self, key: &str) {
        self.states.insert(key.to_string(), EndpointHealth {
            healthy: true,
            last_error: String::new(),
        });
    }

    pub fn set_unhealthy(&self, key: &str, error: &str) {
        self.states.insert(key.to_string(), EndpointHealth {
            healthy: false,
            last_error: error.to_string(),
        });
    }
}

/// Запустить фоновый health checker.
pub fn start_background_health_checker(
    config: Arc<ArcSwap<AppConfig>>,
    fail2ban: Arc<Fail2ban>,
    health_store: Arc<HealthStore>,
    client: Client,
    interval_secs: u64,
) {
    if interval_secs == 0 {
        tracing::info!("Background health checker disabled (interval=0)");
        return;
    }

    let interval = Duration::from_secs(interval_secs);
    tracing::info!(interval_secs, "Background health checker started");

    tokio::spawn(async move {
        tokio::time::sleep(interval).await;

        loop {
            check_all_endpoints(&config, &fail2ban, &health_store, &client).await;
            // Обновляем Prometheus gauge
            let count = fail2ban.banned_count();
            crate::metrics::prometheus::BANNED_ENDPOINTS.set(count);
            tokio::time::sleep(interval).await;
        }
    });
}

async fn check_all_endpoints(
    config: &ArcSwap<AppConfig>,
    fail2ban: &Fail2ban,
    health_store: &HealthStore,
    client: &Client,
) {
    let cfg = config.load();

    for model in &cfg.models {
        for ep in &model.endpoints {
            let ep_key = format!("{}:{}", ep.url, mask_key(&ep.key));
            let probe_url = format!("{}/v1/models", ep.url.trim_end_matches('/'));

            match client
                .get(&probe_url)
                .header("Authorization", format!("Bearer {}", ep.key))
                .timeout(Duration::from_secs(10))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    health_store.set_healthy(&ep_key);
                    if fail2ban.is_banned(&ep_key) {
                        fail2ban.reset(&ep_key);
                        tracing::info!(
                            endpoint = %ep_key,
                            status = resp.status().as_u16(),
                            "Health check passed — endpoint unbanned"
                        );
                    }
                }
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let reason = format!("HTTP {status}");
                    health_store.set_unhealthy(&ep_key, &reason);
                    if !fail2ban.is_banned(&ep_key) {
                        fail2ban.record_failure_with_code(&ep_key, status);
                    }
                    tracing::debug!(endpoint = %ep_key, status, "Health check: non-2xx");
                }
                Err(e) => {
                    let reason = if e.is_timeout() {
                        "timeout".into()
                    } else if e.is_connect() {
                        "connection refused".into()
                    } else {
                        format!("{e}")
                    };
                    health_store.set_unhealthy(&ep_key, &reason);
                    if !fail2ban.is_banned(&ep_key) {
                        fail2ban.record_failure(&ep_key);
                    }
                    tracing::debug!(endpoint = %ep_key, error = %e, "Health check: network error");
                }
            }
        }
    }
}
