//! Фоновая проверка доступности забаненных эндпоинтов.
//!
//! Раз в `interval_secs` отправляет лёгкий запрос (`GET /v1/models`) на каждый
//! забаненный эндпоинт. При успешном ответе (2xx) — снимает бан досрочно.

use arc_swap::ArcSwap;
use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;

use crate::config::AppConfig;
use crate::fail2ban::Fail2ban;

/// Запустить фоновый health checker.
///
/// Параметры:
/// - `config` — для получения списка эндпоинтов
/// - `fail2ban` — для сброса бана при успешной проверке
/// - `client` — HTTP-клиент (с таймаутами)
/// - `interval_secs` — периодичность проверки (0 = не запускать)
pub fn start_background_health_checker(
    config: Arc<ArcSwap<AppConfig>>,
    fail2ban: Arc<Fail2ban>,
    client: Client,
    interval_secs: u64,
) {
    if interval_secs == 0 {
        tracing::info!("Background health checker disabled (interval=0)");
        return;
    }

    let interval = Duration::from_secs(interval_secs);
    tracing::info!(
        interval_secs,
        "Background health checker started"
    );

    tokio::spawn(async move {
        // Первая проверка через interval (не сразу при старте)
        tokio::time::sleep(interval).await;

        loop {
            check_all_endpoints(&config, &fail2ban, &client).await;
            tokio::time::sleep(interval).await;
        }
    });
}

/// Проверить все забаненные эндпоинты.
async fn check_all_endpoints(
    config: &ArcSwap<AppConfig>,
    fail2ban: &Fail2ban,
    client: &Client,
) {
    let cfg = config.load();

    for model in &cfg.models {
        for ep in &model.endpoints {
            let ep_key = format!("{}:{}", ep.url, mask_key(&ep.key));

            // Проверяем только забаненные
            if !fail2ban.is_banned(&ep_key) {
                continue;
            }

            // Лёгкий probing-запрос: GET /v1/models
            let probe_url = format!("{}/v1/models", ep.url.trim_end_matches('/'));
            match client
                .get(&probe_url)
                .header("Authorization", format!("Bearer {}", ep.key))
                .timeout(Duration::from_secs(10))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    fail2ban.reset(&ep_key);
                    tracing::info!(
                        endpoint = %ep_key,
                        status = resp.status().as_u16(),
                        "Health check passed — endpoint unbanned"
                    );
                }
                Ok(resp) => {
                    tracing::debug!(
                        endpoint = %ep_key,
                        status = resp.status().as_u16(),
                        "Health check failed (non-2xx)"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        endpoint = %ep_key,
                        error = %e,
                        "Health check failed (network error)"
                    );
                }
            }
        }
    }
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".into();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}
