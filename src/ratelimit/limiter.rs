use dashmap::DashMap;
use governor::clock::Clock;
use governor::{Quota, RateLimiter as GovernorLimiter};
use parking_lot::Mutex;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// Кастомный token-bucket для TPM с поддержкой пост-фактного списания.
struct TpmLimiter {
    capacity: u64,
    state: Mutex<TpmState>,
}

struct TpmState {
    tokens: i64,
    last_update: Instant,
    /// Accumulated consumed tokens in the current 60s window (for accurate stats reporting).
    /// Stored as (window_start_instant, total_consumed).
    window_start: Instant,
    window_consumed: u64,
}

impl TpmLimiter {
    fn new(capacity: u64) -> Self {
        let cap = capacity.max(1) as i64;
        let now = Instant::now();
        Self {
            capacity,
            state: Mutex::new(TpmState {
                tokens: cap,
                last_update: now,
                window_start: now,
                window_consumed: 0,
            }),
        }
    }

    /// Пополнить бакет пропорционально прошедшему времени (1 minute window).
    fn refill(state: &mut TpmState, capacity: u64) {
        let now = Instant::now();
        let elapsed_ms = now.duration_since(state.last_update).as_millis() as u64;
        if elapsed_ms > 0 {
            let add = (elapsed_ms as u128 * capacity as u128 / 60_000) as i64;
            if add > 0 {
                state.tokens = (state.tokens + add).min(capacity as i64);
                state.last_update = now;
            }
        }
    }

    /// Проверить и атомарно списать n токенов.
    fn check_n(&self, n: u64) -> Result<(), RateLimitWait> {
        let mut s = self.state.lock();
        Self::refill(&mut s, self.capacity);
        let n_i64 = n as i64;
        if s.tokens >= n_i64 {
            s.tokens -= n_i64;
            // Track in window for accurate stats
            let now = Instant::now();
            if now.duration_since(s.window_start) > Duration::from_secs(60) {
                s.window_start = now;
                s.window_consumed = n;
            } else {
                s.window_consumed = s.window_consumed.saturating_add(n);
            }
            Ok(())
        } else {
            let deficit = n_i64 - s.tokens;
            let wait_secs = if self.capacity > 0 {
                deficit as f64 * 60.0 / self.capacity as f64
            } else {
                60.0
            };
            Err(RateLimitWait { wait_secs })
        }
    }

    /// Пост-фактное списание: пополняем бакет и принудительно уменьшаем на n.
    /// Баланс может уйти в минус — следующие запросы будут заблокированы до пополнения.
    fn consume_n(&self, n: u64) {
        let mut s = self.state.lock();
        Self::refill(&mut s, self.capacity);
        s.tokens -= n as i64;
        // Track consumed in the current 60s window for accurate stats
        let now = Instant::now();
        if now.duration_since(s.window_start) > Duration::from_secs(60) {
            s.window_start = now;
            s.window_consumed = n;
        } else {
            s.window_consumed = s.window_consumed.saturating_add(n);
        }
    }

    /// Текущий остаток токенов (может быть отрицательным при овердрафте).
    fn remaining(&self) -> i64 {
        let mut s = self.state.lock();
        Self::refill(&mut s, self.capacity);
        s.tokens
    }

    /// Консервативный remaining для stats: capacity минус consumed за последние 60 секунд.
    /// Не зависит от refill — показывает реальное потребление за окно.
    fn window_remaining(&self) -> i64 {
        let mut s = self.state.lock();
        let now = Instant::now();
        // Expire old window
        if now.duration_since(s.window_start) > Duration::from_secs(60) {
            s.window_start = now;
            s.window_consumed = 0;
        }
        (self.capacity as i64).saturating_sub(s.window_consumed as i64)
    }
}

struct RateLimitWait {
    wait_secs: f64,
}

/// Два rate limiter'а: RPM и TPM
struct RateLimitPair {
    rpm: GovernorLimiter<String, dashmap::DashMap<String, governor::state::InMemoryState>, governor::clock::DefaultClock, governor::middleware::NoOpMiddleware>,
    tpm: TpmLimiter,
    last_used: AtomicU64,
}

impl RateLimitPair {
    fn new(rpm: u32, tpm: u64) -> Self {
        let rpm_q = if rpm > 0 {
            Quota::per_minute(NonZeroU32::new(rpm).unwrap_or(NonZeroU32::MIN))
        } else {
            Quota::per_minute(NonZeroU32::new(1).unwrap())
        };

        Self {
            rpm: GovernorLimiter::keyed(rpm_q),
            tpm: TpmLimiter::new(tpm),
            last_used: AtomicU64::new(unix_now()),
        }
    }

    fn touch(&self) {
        self.last_used.store(unix_now(), Ordering::Relaxed);
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
            pair.touch();
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
            pair.touch();
            let n = estimated_tokens;
            match pair.tpm.check_n(n) {
                Err(wait) => {
                    return RateLimitResult {
                        allowed: false,
                        limit: tpm,
                        reset_after_secs: wait.wait_secs,
                        scope,
                    };
                }
                Ok(_) => {}
            }
        }

        RateLimitResult {
            allowed: true,
            limit: if rpm > 0 { rpm as u64 } else { tpm },
            reset_after_secs: 0.0,
            scope,
        }
    }

    /// Пост-фактное списание TPM на основе реального usage из upstream-ответа.
    /// Вызывать после успешного проксирования.
    pub fn consume_tpm(&self, key: &str, tpm: u64, tokens: u64) {
        if tpm == 0 || tokens == 0 {
            return;
        }
        let pair = self.get_or_create(key, 0, tpm);
        pair.touch();
        pair.tpm.consume_n(tokens);
    }

    /// Получить текущее состояние TPM: (limit, remaining).
    /// Возвращает (0, 0) если лимит не задан.
    /// Использует window_remaining — capacity минус consumed за последние 60 секунд,
    /// не зависит от bucket refill.
    pub fn get_tpm_state(&self, key: &str, tpm: u64) -> (u64, i64) {
        if tpm == 0 {
            return (0, 0);
        }
        let pair = self.get_or_create(key, 0, tpm);
        let remaining = pair.tpm.window_remaining();
        (tpm, remaining)
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

    #[test]
    fn test_tpm_consume_reduces_future_budget() {
        let store = RateLimiterStore::new();
        // TPM=10. Предварительная проверка на 1 токен.
        assert!(store.check("tpm-consume", 0, 10, 1, RateLimitScope::Token).allowed);
        // Пост-факт списание ещё 8 токенов (итого 9)
        store.consume_tpm("tpm-consume", 10, 8);
        // Остался 1 токен — проходит
        assert!(store.check("tpm-consume", 0, 10, 1, RateLimitScope::Token).allowed);
        // 11-й токен — блок
        assert!(!store.check("tpm-consume", 0, 10, 1, RateLimitScope::Token).allowed);
    }

    #[test]
    fn test_tpm_consume_can_overdraft() {
        let store = RateLimiterStore::new();
        // TPM=5. Предварительно списываем 1.
        assert!(store.check("tpm-od", 0, 5, 1, RateLimitScope::Token).allowed);
        // Пост-факт списание 10 токенов → уходим в овердрафт
        store.consume_tpm("tpm-od", 5, 10);
        // Следующий запрос сразу блокируется, пока бакет не пополнится
        assert!(!store.check("tpm-od", 0, 5, 1, RateLimitScope::Token).allowed);
    }

    #[test]
    fn test_tpm_consume_zero_is_noop() {
        let store = RateLimiterStore::new();
        assert!(store.check("tpm-zero", 0, 5, 1, RateLimitScope::Token).allowed);
        store.consume_tpm("tpm-zero", 5, 0);
        // Бюджет не изменился (осталось 4)
        assert!(store.check("tpm-zero", 0, 5, 1, RateLimitScope::Token).allowed);
        assert!(store.check("tpm-zero", 0, 5, 1, RateLimitScope::Token).allowed);
        assert!(store.check("tpm-zero", 0, 5, 1, RateLimitScope::Token).allowed);
        assert!(store.check("tpm-zero", 0, 5, 1, RateLimitScope::Token).allowed);
        // 5-й запрос — блок
        assert!(!store.check("tpm-zero", 0, 5, 1, RateLimitScope::Token).allowed);
    }

    #[test]
    fn test_tpm_consume_isolated_per_key() {
        let store = RateLimiterStore::new();
        // key1: check 1 + consume 5 = 6 из 10
        assert!(store.check("key1", 0, 10, 1, RateLimitScope::Token).allowed);
        store.consume_tpm("key1", 10, 5);
        // key2: независимый бюджет 10
        assert!(store.check("key2", 0, 10, 1, RateLimitScope::Token).allowed);
        store.consume_tpm("key2", 10, 5);
        // key1 осталось 4 — 4 проходят
        for _ in 0..4 {
            assert!(store.check("key1", 0, 10, 1, RateLimitScope::Token).allowed);
        }
        assert!(!store.check("key1", 0, 10, 1, RateLimitScope::Token).allowed);
        // key2 тоже осталось 4
        for _ in 0..4 {
            assert!(store.check("key2", 0, 10, 1, RateLimitScope::Token).allowed);
        }
        assert!(!store.check("key2", 0, 10, 1, RateLimitScope::Token).allowed);
    }

    #[test]
    fn test_rpm_plus_tpm_with_consume() {
        let store = RateLimiterStore::new();
        // RPM=2, TPM=10. Первый запрос проходит.
        assert!(store.check("mixed", 2, 10, 1, RateLimitScope::Token).allowed);
        // Пост-факт consume 5 → TPM осталось 4
        store.consume_tpm("mixed", 10, 5);
        // Второй запрос проходит по RPM и TPM
        assert!(store.check("mixed", 2, 10, 1, RateLimitScope::Token).allowed);
        // consume ещё 3 → TPM осталось 1 (10 - 1 - 5 - 1 - 3 = 0)
        store.consume_tpm("mixed", 10, 3);
        // RPM исчерпан (2/2), TPM тоже 0 — блок
        assert!(!store.check("mixed", 2, 10, 1, RateLimitScope::Token).allowed);
    }

    #[test]
    fn test_cleanup_retains_recently_used() {
        use std::sync::atomic::Ordering;
        let store = RateLimiterStore::new();
        let pair = store.get_or_create("recent", 1, 10);
        pair.touch();
        // last_used должен быть свежим (в пределах пары секунд от now)
        let now = unix_now();
        let last = pair.last_used.load(Ordering::Relaxed);
        assert!(now >= last && now - last < 5, "last_used should be recent");
    }

    #[test]
    fn test_tpm_large_consume_blocks_subsequent() {
        let store = RateLimiterStore::new();
        // TPM=100. check(1) → осталось 99
        assert!(store.check("bulk", 0, 100, 1, RateLimitScope::Token).allowed);
        // consume 99 → осталось 0
        store.consume_tpm("bulk", 100, 99);
        // Следующий check(1) блокируется
        assert!(!store.check("bulk", 0, 100, 1, RateLimitScope::Token).allowed);
    }

    #[test]
    fn test_tpm_check_n_with_estimated_greater_than_one() {
        let store = RateLimiterStore::new();
        // TPM=10, estimated_tokens=4 → проходит (осталось 6)
        assert!(store.check("est", 0, 10, 4, RateLimitScope::Token).allowed);
        // estimated_tokens=4 → проходит (осталось 2)
        assert!(store.check("est", 0, 10, 4, RateLimitScope::Token).allowed);
        // estimated_tokens=4 → блок (нужно 4, осталось 2)
        assert!(!store.check("est", 0, 10, 4, RateLimitScope::Token).allowed);
        // consume не меняет ключ
        store.consume_tpm("est", 10, 1);
        // estimated_tokens=1 → проходит (осталось 1)
        assert!(store.check("est", 0, 10, 1, RateLimitScope::Token).allowed);
        // Больше ничего
        assert!(!store.check("est", 0, 10, 1, RateLimitScope::Token).allowed);
    }
}

impl RateLimiterStore {
    /// Периодическая очистка неактивных записей (вызывать в фоне)
    pub fn start_cleanup(store: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(300)).await;
                let now = unix_now();
                let before = store.limiters.len();
                store.limiters.retain(|_, pair| {
                    let idle = now.saturating_sub(pair.last_used.load(Ordering::Relaxed));
                    idle < 600
                });
                if store.limiters.len() < before {
                    tracing::debug!(before, after = store.limiters.len(), "Rate limiter cleanup");
                }
            }
        });
    }
}
