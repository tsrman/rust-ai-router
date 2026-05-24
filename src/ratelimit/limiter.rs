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

        // TPM проверка
        if tpm > 0 && estimated_tokens > 0 {
            let pair = self.get_or_create(key, rpm, tpm);
            // Для TPM проверяем по одному токену; при первом же отказе — возвращаем ошибку
            for _i in 0..estimated_tokens {
                match pair.tpm.check_key(&key.to_string()) {
                    Err(not_until) => {
                        // «Возвращаем» уже потреблённые токены обратно (упрощение)
                        // В реальности нужно было бы сделать atomic check, но для MVP норм
                        let wait = not_until.wait_time_from(governor::clock::DefaultClock::default().now());
                        return RateLimitResult {
                            allowed: false,
                            limit: tpm,
                            reset_after_secs: wait.as_secs_f64(),
                            scope,
                        };
                    }
                    Ok(_) => {}
                }
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
        // Exhaust key1
        store.check("key1", 1, 0, 1, RateLimitScope::Token);
        assert!(!store.check("key1", 1, 0, 1, RateLimitScope::Token).allowed);
        // key2 should still work
        assert!(store.check("key2", 1, 0, 1, RateLimitScope::Token).allowed);
    }
}
