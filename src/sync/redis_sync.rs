use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;

use crate::ratelimit::RateLimitResult;
use crate::ratelimit::RateLimitScope;

#[allow(dead_code)]
pub struct SyncStore {
    conn: Option<MultiplexedConnection>,
    prefix: String,
}

#[allow(dead_code)]
impl SyncStore {
    /// No-op экземпляр (sync не подключён)
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self { conn: None, prefix: String::new() }
    }

    /// Подключиться к Redis/Valkey
    pub async fn connect(url: &str, prefix: &str) -> Result<Self, String> {
        let client = redis::Client::open(url)
            .map_err(|e| format!("Redis client error: {e}"))?;

        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| format!("Redis connection failed: {e}"))?;

        let _: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|e| format!("Redis PING failed: {e}"))?;

        tracing::info!(url = %crate::utils::mask_url(url), prefix, "Redis sync connected");
        Ok(Self { conn: Some(conn), prefix: prefix.to_string() })
    }

    fn conn(&self) -> Option<MultiplexedConnection> {
        self.conn.clone()
    }

    pub async fn check_rate_limit(
        &self, scope: &str, key: &str, limit: u64,
    ) -> RateLimitResult {
        let Some(mut conn) = self.conn() else {
            return RateLimitResult { allowed: true, limit: 0, reset_after_secs: 0.0, scope: RateLimitScope::Token };
        };
        if limit == 0 {
            return RateLimitResult { allowed: true, limit: 0, reset_after_secs: 0.0, scope: RateLimitScope::Token };
        }

        // Скользящее окно: ключ без привязки к минуте.
        // INCR при первом запросе → EXPIRE 60s. Сброс через 60s после первого запроса.
        let rk = format!("{}:rl:{}:{}", self.prefix, scope, key);
        let window_secs: i64 = 60;

        let count: u64 = match conn.incr(&rk, 1).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Redis rate limit error: {e}");
                return RateLimitResult { allowed: true, limit: 0, reset_after_secs: 0.0, scope: RateLimitScope::Token };
            }
        };
        if count == 1 {
            let _: Result<bool, redis::RedisError> = conn.expire(&rk, window_secs).await;
        }

        let allowed = count <= limit;
        RateLimitResult {
            allowed,
            limit,
            reset_after_secs: if allowed { 0.0 } else { window_secs as f64 },
            scope: RateLimitScope::Token,
        }
    }

    pub async fn set_ban(&self, endpoint_key: &str, duration_secs: u64) {
        let Some(mut conn) = self.conn() else { return };
        let bk = format!("{}:banned:{}", self.prefix, endpoint_key);
        let _: Result<(), redis::RedisError> = conn.set_ex(&bk, "1", duration_secs).await;
    }

    pub async fn is_banned(&self, endpoint_key: &str) -> bool {
        let Some(mut conn) = self.conn() else { return false };
        let bk = format!("{}:banned:{}", self.prefix, endpoint_key);
        conn.exists(&bk).await.unwrap_or(false)
    }

    pub async fn set_sticky(&self, session_id: &str, endpoint_index: usize, ttl_secs: u64) {
        let Some(mut conn) = self.conn() else { return };
        let sk = format!("{}:sticky:{}", self.prefix, session_id);
        let _: Result<(), redis::RedisError> = conn.set_ex(&sk, endpoint_index.to_string(), ttl_secs).await;
    }

    pub async fn get_sticky(&self, session_id: &str) -> Option<usize> {
        let mut conn = self.conn()?;
        let sk = format!("{}:sticky:{}", self.prefix, session_id);
        let result: Result<Option<String>, redis::RedisError> = conn.get(&sk).await;
        if let Ok(Some(val)) = result {
            let _: Result<bool, redis::RedisError> = conn.expire(&sk, 300).await;
            return val.parse().ok();
        }
        None
    }

    pub async fn publish_ban(&self, _endpoint_key: &str, _duration_secs: u64) {}
}
