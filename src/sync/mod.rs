//! Synchronization between instances via Redis/Valkey (optional).

#[cfg(feature = "redis-sync")]
mod redis_sync;
#[cfg(feature = "redis-sync")]
pub use redis_sync::*;

/// No-op stub when redis-sync is not enabled
#[cfg(not(feature = "redis-sync"))]
pub struct SyncStore;

#[cfg(not(feature = "redis-sync"))]
impl SyncStore {
    pub fn new() -> Self { Self }
    pub async fn connect(_cfg: &crate::config::SyncConfig) -> Result<Self, String> {
        Ok(Self)
    }
    pub async fn check_rate_limit(
        &self, _s: &str, _k: &str, _limit: u64,
    ) -> crate::ratelimit::RateLimitResult {
        crate::ratelimit::RateLimitResult {
            allowed: true,
            limit: 0,
            reset_after_secs: 0.0,
            scope: crate::ratelimit::RateLimitScope::Token,
        }
    }
    pub async fn publish_ban(&self, _ep: &str, _d: u64) {}
    pub async fn is_banned(&self, _ep: &str) -> bool { false }
    pub async fn set_ban(&self, _ep: &str, _d: u64) {}
    pub async fn get_sticky(&self, _sid: &str) -> Option<usize> { None }
    pub async fn set_sticky(&self, _sid: &str, _idx: usize, _ttl: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_noop_check_rate_limit_always_allowed() {
        let store = SyncStore::new();
        for _ in 0..100 {
            let r = store.check_rate_limit("token", "key", 2).await;
            assert!(r.allowed);
        }
    }

    #[tokio::test]
    async fn test_noop_is_not_banned() {
        let store = SyncStore::new();
        assert!(!store.is_banned("any-endpoint").await);
    }

    #[tokio::test]
    async fn test_noop_sticky_returns_none() {
        let store = SyncStore::new();
        assert_eq!(store.get_sticky("session-1").await, None);
    }

    #[tokio::test]
    async fn test_noop_methods_dont_panic() {
        let store = SyncStore::new();
        store.set_ban("ep", 60).await;
        store.set_sticky("sid", 0, 60).await;
        store.publish_ban("ep", 60).await;
        // just checking they don't panic
    }
}
