use dashmap::DashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Состояние одного эндпоинта
struct EndpointState {
    consecutive_failures: AtomicU32,
    total_requests: AtomicU64,
    total_failures: AtomicU64,
    banned_until: parking_lot::Mutex<Option<Instant>>,
    /// Код последней ошибки (HTTP status или 0 для network error)
    last_error_code: parking_lot::Mutex<u16>,
}

/// Circuit breaker / fail2ban для эндпоинтов
pub struct Fail2ban {
    states: DashMap<String, EndpointState>,
    max_failures: u32,
    ban_duration: Duration,
    error_threshold: f64,
    /// Паттерны HTTP-статусов, считающихся ошибкой: "5xx", "500", "429", etc.
    fail_patterns: Vec<FailPattern>,
}

#[derive(Debug, Clone)]
enum FailPattern {
    Range { min: u16, max: u16 },
    Exact(u16),
}

impl Fail2ban {
    pub fn new(
        max_failures: u32,
        ban_duration_secs: u64,
        error_threshold_pct: f64,
        fail_status_codes: &[String],
    ) -> Self {
        let patterns = parse_fail_patterns(fail_status_codes);
        Self {
            states: DashMap::new(),
            max_failures,
            ban_duration: Duration::from_secs(ban_duration_secs),
            error_threshold: error_threshold_pct,
            fail_patterns: patterns,
        }
    }

    /// Проверить, не забанен ли эндпоинт
    pub fn is_banned(&self, endpoint_key: &str) -> bool {
        if let Some(state) = self.states.get(endpoint_key) {
            let banned = state.banned_until.lock();
            if let Some(until) = *banned {
                if Instant::now() < until {
                    return true;
                }
                drop(banned);
                state.consecutive_failures.store(0, Ordering::Relaxed);
                *state.banned_until.lock() = None;
            }
        }
        false
    }

    /// Проверить, является ли HTTP-статус ошибкой согласно паттернам
    pub fn is_fail_status(&self, status: u16) -> bool {
        if self.fail_patterns.is_empty() {
            return false;
        }
        self.fail_patterns.iter().any(|p| match p {
            FailPattern::Range { min, max } => status >= *min && status <= *max,
            FailPattern::Exact(code) => status == *code,
        })
    }

    /// Записать успешный запрос
    pub fn record_success(&self, endpoint_key: &str) {
        let state = self.ensure(endpoint_key);
        state.total_requests.fetch_add(1, Ordering::Relaxed);
        state.consecutive_failures.store(0, Ordering::Relaxed);
    }

    /// Записать ошибку и проверить, нужно ли банить
    pub fn record_failure(&self, endpoint_key: &str) -> bool {
        self.record_failure_with_code(endpoint_key, 0)
    }

    /// Записать ошибку с HTTP-кодом (0 = сетевая ошибка)
    pub fn record_failure_with_code(&self, endpoint_key: &str, status_code: u16) -> bool {
        let state = self.ensure(endpoint_key);
        state.total_requests.fetch_add(1, Ordering::Relaxed);
        state.total_failures.fetch_add(1, Ordering::Relaxed);
        *state.last_error_code.lock() = status_code;

        let failures = state.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;

        if failures >= self.max_failures {
            let mut banned = state.banned_until.lock();
            *banned = Some(Instant::now() + self.ban_duration);
            tracing::warn!(
                "Fail2ban: endpoint {} banned for {}s after {} consecutive failures",
                endpoint_key,
                self.ban_duration.as_secs(),
                failures
            );
            return true;
        }

        let total = state.total_requests.load(Ordering::Relaxed);
        let total_err = state.total_failures.load(Ordering::Relaxed);
        if total >= 10 && (total_err as f64 / total as f64) > self.error_threshold {
            let mut banned = state.banned_until.lock();
            *banned = Some(Instant::now() + self.ban_duration);
            tracing::warn!(
                "Fail2ban: endpoint {} banned after {:.1}% error rate ({} errors / {} requests)",
                endpoint_key,
                (total_err as f64 / total as f64) * 100.0,
                total_err,
                total
            );
            return true;
        }

        false
    }

    pub fn reset(&self, endpoint_key: &str) {
        if let Some(state) = self.states.get(endpoint_key) {
            state.consecutive_failures.store(0, Ordering::Relaxed);
            *state.banned_until.lock() = None;
        }
    }

    /// Все эндпоинты: (key, healthy, reason)
    pub fn all_statuses(&self) -> Vec<(String, bool)> {
        let mut banned_count = 0i64;
        let result: Vec<_> = self.states
            .iter()
            .map(|entry| {
                let key = entry.key().clone();
                let banned = {
                    let guard = entry.value().banned_until.lock();
                    guard.map(|u| Instant::now() < u).unwrap_or(false)
                };
                if banned { banned_count += 1; }
                (key, !banned)
            })
            .collect();
        crate::metrics::prometheus::BANNED_ENDPOINTS.set(banned_count);
        result
    }

    /// Причина бана для эндпоинта (None = не забанен)
    pub fn ban_reason(&self, endpoint_key: &str) -> Option<String> {
        let state = self.states.get(endpoint_key)?;
        let banned = {
            let guard = state.banned_until.lock();
            guard.map(|u| Instant::now() < u).unwrap_or(false)
        };
        if !banned {
            return None;
        }
        let code = *state.last_error_code.lock();
        Some(if code == 0 {
            "network error".into()
        } else {
            format!("HTTP {code}")
        })
    }

    fn ensure(&self, key: &str) -> dashmap::mapref::one::RefMut<'_, String, EndpointState> {
        self.states.entry(key.to_string()).or_insert_with(|| EndpointState {
            consecutive_failures: AtomicU32::new(0),
            total_requests: AtomicU64::new(0),
            total_failures: AtomicU64::new(0),
            banned_until: parking_lot::Mutex::new(None),
            last_error_code: parking_lot::Mutex::new(0),
        })
    }
}

/// Парсинг паттернов: "5xx" → 500-599, "429" → точный 429
fn parse_fail_patterns(codes: &[String]) -> Vec<FailPattern> {
    codes
        .iter()
        .filter_map(|s| {
            let s = s.trim();
            if s.eq_ignore_ascii_case("5xx") {
                Some(FailPattern::Range { min: 500, max: 599 })
            } else if s.eq_ignore_ascii_case("4xx") {
                Some(FailPattern::Range { min: 400, max: 499 })
            } else if let Ok(code) = s.parse::<u16>() {
                Some(FailPattern::Exact(code))
            } else {
                tracing::warn!("Invalid fail2ban status pattern: {s}");
                None
            }
        })
        .collect()
}

impl Clone for Fail2ban {
    fn clone(&self) -> Self {
        panic!("Fail2ban should not be cloned; wrap in Arc instead")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_failures_no_ban() {
        let fb = Fail2ban::new(3, 60, 0.5, &vec!["5xx".to_string()]);
        assert!(!fb.is_banned("ep1"));
    }

    #[test]
    fn test_ban_after_max_failures() {
        let fb = Fail2ban::new(2, 60, 0.5, &vec!["5xx".to_string()]);
        fb.record_failure("ep1");
        assert!(!fb.is_banned("ep1"));
        let banned = fb.record_failure("ep1");
        assert!(banned);
        assert!(fb.is_banned("ep1"));
    }

    #[test]
    fn test_success_resets_counter() {
        let fb = Fail2ban::new(3, 60, 0.5, &vec!["5xx".to_string()]);
        fb.record_failure("ep1");
        fb.record_failure("ep1");
        fb.record_success("ep1");
        let banned = fb.record_failure("ep1");
        assert!(!banned);
    }

    #[test]
    fn test_ban_expires() {
        let fb = Fail2ban::new(1, 1, 0.5, &vec!["5xx".to_string()]);
        fb.record_failure("ep1");
        assert!(fb.is_banned("ep1"));
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(!fb.is_banned("ep1"));
    }

    #[test]
    fn test_status_code_matching() {
        let fb = Fail2ban::new(1, 60, 0.5, &vec!["5xx".to_string(), "401".to_string(), "429".to_string()]);
        assert!(fb.is_fail_status(500));
        assert!(fb.is_fail_status(503));
        assert!(fb.is_fail_status(401));
        assert!(fb.is_fail_status(429));
        assert!(!fb.is_fail_status(200));
        assert!(!fb.is_fail_status(404));
    }

    #[test]
    fn test_ban_reason_network_error() {
        let fb = Fail2ban::new(1, 60, 0.5, &vec!["5xx".to_string()]);
        fb.record_failure_with_code("ep1", 0);
        let reason = fb.ban_reason("ep1").unwrap();
        assert_eq!(reason, "network error");
    }

    #[test]
    fn test_ban_reason_http_status() {
        let fb = Fail2ban::new(1, 60, 0.5, &vec!["5xx".to_string(), "401".to_string()]);
        fb.record_failure_with_code("ep1", 401);
        let reason = fb.ban_reason("ep1").unwrap();
        assert_eq!(reason, "HTTP 401");
    }

    #[test]
    fn test_ban_reason_none_when_not_banned() {
        let fb = Fail2ban::new(3, 60, 0.5, &vec!["5xx".to_string()]);
        fb.record_failure("ep1");
        assert!(fb.ban_reason("ep1").is_none());
    }
}
