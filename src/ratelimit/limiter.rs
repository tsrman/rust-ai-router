use dashmap::DashMap;
use governor::clock::Clock;
use governor::{Quota, RateLimiter as GovernorLimiter};
use std::num::NonZeroU32;
use std::sync::Arc;

/// Результат проверки rate limit
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RateLimitResult {
    /// Запрос разрешён?
    pub allowed: bool,
    /// Лимит (RPM или TPM), который был превышен. 0 = unlimited.
    pub limit: u64,
    /// Через сколько секунд можно повторить. 0 = можно сейчас.
    pub reset_after_secs: f64,
    /// Ключ лимитера (для заголовков)
    pub scope: RateLimitScope,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RateLimitScope {
    Token,
    Endpoint,
}

/// Два rate limiter'а: RPM и TPM
struct RateLimitPair {
    rpm: GovernorLimiter<String, dashmap::DashMap<String, governor::state::InMemoryState>, governor::clock::DefaultClock, governor::middleware::NoOpMiddleware>,
    tpm: GovernorLimiter<String, dashmap::DashMap<String, governor::state::InMemoryState>, governor::clock::DefaultClock, governor::middleware::NoOpMiddleware>,
}

/// Хранилище rate limiter'ов по ключам
pub struct RateLimiterStore {
    limiters: DashMap<String, Arc<RateLimitPair>>,
}

impl RateLimiterStore {
    pub fn new() -> Self {
        Self {
            limiters: DashMap::new(),
        }
    }

    fn get_or_create(&self, key: &str, rpm: u32, tpm: u64) -> Arc<RateLimitPair> {
        self.limiters
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(RateLimitPair::new(rpm, tpm)))
            .clone()
    }

    /// Проверить RPM + TPM. Возвращает результат с деталями.
    pub fn check(&self, key: &str, rpm: u32, tpm: u64, estimated_tokens: u64, scope: RateLimitScope) -> RateLimitResult {
        // RPM проверка
        if rpm > 0 {
            let pair = self.get_or_create(key, rpm, tpm);
            match pair.rpm.check_key(&key.to_string()) {
                Err(not_until) => {
                    let wait = not_until.wait_time_from(governor::clock::DefaultClock::default().now());
                    return RateLimitResult {
                        allowed: false,
                        limit: rpm as u64,
                        reset_after_secs: wait.as_secs_f64(),
                        scope,
                    };
                }
                Ok(_) => {}
            }
        }

        // TPM проверка (атомарно — check_key_n для N токенов)
        if tpm > 0 && estimated_tokens > 0 {
            let pair = self.get_or_create(key, rpm, tpm);
            let n = NonZeroU32::new(estimated_tokens as u32).unwrap_or(NonZeroU32::MIN);
            match pair.tpm.check_key_n(&key.to_string(), n) {
                Err(_insufficient) => {
                    // Запрос с estimated_tokens > capacity бакета — всегда отклоняем
                    return RateLimitResult {
                        allowed: false,
                        limit: tpm,
                        reset_after_secs: 60.0,
                        scope,
                    };
                }
                Ok(Err(not_until)) => {
                    let wait = not_until.wait_time_from(governor::clock::DefaultClock::default().now());
                    return RateLimitResult {
                        allowed: false,
                        limit: tpm,
                        reset_after_secs: wait.as_secs_f64(),
                        scope,
                    };
                }
                Ok(Ok(_)) => {}  // запрос разрешён
            }
        }

        RateLimitResult {
            allowed: true,
            limit: if rpm > 0 { rpm as u64 } else { tpm },
            reset_after_secs: 0.0,
            scope,
        }
    }
}

impl RateLimitPair {
    fn new(rpm: u32, tpm: u64) -> Self {
        let rpm_q = if rpm > 0 {
            Quota::per_minute(NonZeroU32::new(rpm).unwrap_or(NonZeroU32::MIN))
        } else {
            Quota::per_minute(NonZeroU32::new(1).unwrap())
        };

        let tpm_q = if tpm > 0 {
            Quota::per_minute(NonZeroU32::new(tpm as u32).unwrap_or(NonZeroU32::MIN))
        } else {
            Quota::per_minute(NonZeroU32::new(1).unwrap())
        };

        Self {
            rpm: GovernorLimiter::keyed(rpm_q),
            tpm: GovernorLimiter::keyed(tpm_q),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_allows_within_budget() {
        let store = RateLimiterStore::new();
        for _ in 0..5 {
            let r = store.check("test-key", 10, 0, 1, RateLimitScope::Token);
            assert!(r.allowed, "should allow under limit");
        }
    }

    #[test]
    fn test_rate_limit_blocks_after_exceed() {
        let store = RateLimiterStore::new();
        // RPM=2 should allow 2, block the 3rd
        assert!(store.check("test-key", 2, 0, 1, RateLimitScope::Token).allowed);
        assert!(store.check("test-key", 2, 0, 1, RateLimitScope::Token).allowed);
        let r = store.check("test-key", 2, 0, 1, RateLimitScope::Token);
        assert!(!r.allowed, "3rd request should be blocked");
        assert!(r.reset_after_secs > 0.0);
    }

    #[test]
    fn test_rate_limit_zero_means_unlimited() {
        let store = RateLimiterStore::new();
        for _ in 0..100 {
            assert!(store.check("test-key", 0, 0, 1, RateLimitScope::Token).allowed);
        }
    }

    #[test]
    fn test_rate_limit_different_keys_independent() {
        let store = RateLimiterStore::new();
        store.check("key1", 1, 0, 1, RateLimitScope::Token);
        assert!(!store.check("key1", 1, 0, 1, RateLimitScope::Token).allowed);
        assert!(store.check("key2", 1, 0, 1, RateLimitScope::Token).allowed);
    }

    #[test]
    fn test_tpm_check_n_blocks_after_limit() {
        let store = RateLimiterStore::new();
        // TPM=5, each request consumes 3 tokens → second request blocked
        assert!(store.check("tpm-key", 0, 5, 3, RateLimitScope::Token).allowed);
        assert!(!store.check("tpm-key", 0, 5, 3, RateLimitScope::Token).allowed);
    }

    #[test]
    fn test_tpm_check_n_allows_within_limit() {
        let store = RateLimiterStore::new();
        // TPM=10, each consumes 2 → 5 fit
        for _ in 0..5 {
            assert!(store.check("tpm-key", 0, 10, 2, RateLimitScope::Token).allowed);
        }
        assert!(!store.check("tpm-key", 0, 10, 2, RateLimitScope::Token).allowed);
    }
}

impl RateLimiterStore {
    /// Периодическая очистка неактивных записей (вызывать в фоне)
    pub fn start_cleanup(store: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                let before = store.limiters.len();
                store.limiters.retain(|_, pair| {
                    pair.rpm.len() > 0 || pair.tpm.len() > 0
                });
                if store.limiters.len() < before {
                    tracing::debug!(before, after = store.limiters.len(), "Rate limiter cleanup");
                }
            }
        });
    }
}
// TPM test at the end (outside the test module, runnable via cargo test)
