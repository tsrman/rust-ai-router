use std::sync::Arc;

#[cfg(feature = "postgres")]
use sqlx::postgres::PgPoolOptions;
#[cfg(feature = "postgres")]
use sqlx::PgPool;
#[cfg(feature = "postgres")]
use chrono::Timelike;

#[cfg(feature = "postgres")]
pub struct StatsWriter {
    pool: Option<PgPool>,
    retention_days: u32,
    cleanup_interval_secs: u64,
    aggregation_interval_secs: u64,
}

#[cfg(feature = "postgres")]
impl StatsWriter {
    pub fn new(
        database_url: &str,
        retention_days: u32,
        cleanup_interval_secs: u64,
        aggregation_interval_secs: u64,
    ) -> Self {
        if database_url.is_empty() {
            tracing::info!("PostgreSQL stats disabled (empty URL)");
            return Self {
                pool: None,
                retention_days,
                cleanup_interval_secs,
                aggregation_interval_secs,
            };
        }

        let db_url = database_url.to_string();
        let pool = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let pool = PgPoolOptions::new()
                    .max_connections(5)
                    .connect(&db_url)
                    .await
                    .expect("Failed to connect to PostgreSQL");

                // --- Tables ---

                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS requests (\
                     id BIGSERIAL PRIMARY KEY, timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),\
                     model VARCHAR(255) NOT NULL, endpoint VARCHAR(512) NOT NULL,\
                     team VARCHAR(128) NOT NULL DEFAULT '',\
                     tokens_prompt BIGINT NOT NULL DEFAULT 0,\
                     tokens_completion BIGINT NOT NULL DEFAULT 0,\
                     latency_ms BIGINT NOT NULL DEFAULT 0,\
                     status SMALLINT NOT NULL DEFAULT 200,\
                     token_key_hash VARCHAR(64),\
                     cost_approx DOUBLE PRECISION NOT NULL DEFAULT 0)"
                ).execute(&pool).await.expect("CREATE TABLE requests");

                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS token_usage_hourly (\
                     hour TIMESTAMPTZ NOT NULL, token_hash VARCHAR(64) NOT NULL,\
                     team VARCHAR(128) NOT NULL DEFAULT '',\
                     requests BIGINT NOT NULL DEFAULT 0,\
                     tokens_prompt BIGINT NOT NULL DEFAULT 0,\
                     tokens_completion BIGINT NOT NULL DEFAULT 0,\
                     cost_approx DOUBLE PRECISION NOT NULL DEFAULT 0,\
                     PRIMARY KEY (hour, token_hash))"
                ).execute(&pool).await.expect("CREATE TABLE token_usage_hourly");

                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS team_usage_hourly (\
                     hour TIMESTAMPTZ NOT NULL, team VARCHAR(128) NOT NULL,\
                     requests BIGINT NOT NULL DEFAULT 0,\
                     tokens_prompt BIGINT NOT NULL DEFAULT 0,\
                     tokens_completion BIGINT NOT NULL DEFAULT 0,\
                     active_tokens BIGINT NOT NULL DEFAULT 0,\
                     cost_approx DOUBLE PRECISION NOT NULL DEFAULT 0,\
                     PRIMARY KEY (hour, team))"
                ).execute(&pool).await.expect("CREATE TABLE team_usage_hourly");

                // Курсор агрегации: хранит timestamp последней обработанной записи
                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS _aggregation_cursor (\
                     id INT PRIMARY KEY DEFAULT 1,\
                     last_ts TIMESTAMPTZ NOT NULL DEFAULT NOW())"
                ).execute(&pool).await.ok();
                // Инициализируем курсор, если его ещё нет
                sqlx::query(
                    "INSERT INTO _aggregation_cursor (id, last_ts) \
                     VALUES (1, NOW() - INTERVAL '1 hour') \
                     ON CONFLICT DO NOTHING"
                ).execute(&pool).await.ok();

                // Миграция: добавить cost_approx в существующую таблицу requests
                sqlx::query(
                    "ALTER TABLE requests ADD COLUMN IF NOT EXISTS cost_approx DOUBLE PRECISION NOT NULL DEFAULT 0"
                ).execute(&pool).await.ok();

                // --- Indexes ---

                sqlx::query("CREATE INDEX IF NOT EXISTS idx_requests_ts ON requests (timestamp DESC)")
                    .execute(&pool).await.ok();
                sqlx::query("CREATE INDEX IF NOT EXISTS idx_requests_token ON requests (token_key_hash)")
                    .execute(&pool).await.ok();
                sqlx::query("CREATE INDEX IF NOT EXISTS idx_requests_team ON requests (team)")
                    .execute(&pool).await.ok();
                sqlx::query("CREATE INDEX IF NOT EXISTS idx_requests_token_ts ON requests (token_key_hash, timestamp DESC)")
                    .execute(&pool).await.ok();
                sqlx::query("CREATE INDEX IF NOT EXISTS idx_requests_team_ts ON requests (team, timestamp DESC)")
                    .execute(&pool).await.ok();
                sqlx::query("CREATE INDEX IF NOT EXISTS idx_requests_model_ts ON requests (model, timestamp DESC)")
                    .execute(&pool).await.ok();
                sqlx::query("CREATE INDEX IF NOT EXISTS idx_requests_status_ts ON requests (status, timestamp DESC)")
                    .execute(&pool).await.ok();
                // Индекс для batch-агрегации: быстрый range scan по timestamp
                sqlx::query("CREATE INDEX IF NOT EXISTS idx_requests_ts_cost ON requests (timestamp, cost_approx)")
                    .execute(&pool).await.ok();

                tracing::info!("PostgreSQL stats connected, tables created");
                pool
            })
        }).join().unwrap();

        Self {
            pool: Some(pool),
            retention_days,
            cleanup_interval_secs,
            aggregation_interval_secs,
        }
    }

    /// Только вставка в requests (без агрегации на лету — её делает фоновая задача)
    pub async fn record_request(
        &self,
        model: &str,
        endpoint: &str,
        team: &str,
        tokens_prompt: u64,
        tokens_completion: u64,
        latency_ms: u64,
        status: u16,
        token_hash: Option<&str>,
        cost_prompt_per_1m: f64,
        cost_completion_per_1m: f64,
    ) {
        let pool = match &self.pool {
            Some(p) => p,
            None => return,
        };
        let th = token_hash.unwrap_or("");

        let cost = (tokens_prompt as f64 / 1_000_000.0) * cost_prompt_per_1m
            + (tokens_completion as f64 / 1_000_000.0) * cost_completion_per_1m;

        let _ = sqlx::query(
            "INSERT INTO requests \
             (model, endpoint, team, tokens_prompt, tokens_completion, latency_ms, status, token_key_hash, cost_approx) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(model)
        .bind(endpoint)
        .bind(team)
        .bind(tokens_prompt as i64)
        .bind(tokens_completion as i64)
        .bind(latency_ms as i64)
        .bind(status as i16)
        .bind(th)
        .bind(cost)
        .execute(pool)
        .await;
    }

    /// Запустить фоновые задачи: очистка + batch-агрегация
    pub fn start_background_tasks(self: &Arc<Self>) {
        // --- Cleanup ---
        if self.cleanup_interval_secs > 0 && self.retention_days > 0 && self.pool.is_some() {
            let this = self.clone();
            let retention = self.retention_days;
            let interval = self.cleanup_interval_secs;

            tracing::info!("Stats cleanup enabled: retention={retention}d, interval={interval}s");

            tokio::spawn(async move {
                // Первый запуск через 60 секунд
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                loop {
                    this.run_cleanup(retention).await;
                    tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                }
            });
        } else {
            tracing::info!(
                "Stats cleanup disabled (interval={}, retention={}d)",
                self.cleanup_interval_secs, self.retention_days
            );
        }

        // --- Aggregation ---
        if self.aggregation_interval_secs > 0 && self.pool.is_some() {
            let this = self.clone();
            let interval = self.aggregation_interval_secs;

            tracing::info!("Stats batch aggregation enabled: interval={interval}s");

            tokio::spawn(async move {
                // Первый запуск через 90 секунд (после cleanup, чтобы не пересекались)
                tokio::time::sleep(std::time::Duration::from_secs(90)).await;
                loop {
                    this.run_aggregation().await;
                    tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                }
            });
        } else {
            tracing::info!(
                "Stats aggregation disabled (interval={})",
                self.aggregation_interval_secs
            );
        }
    }

    /// Удалить записи старше retention_days из всех таблиц
    async fn run_cleanup(&self, retention_days: u32) {
        let pool = match &self.pool {
            Some(p) => p,
            None => return,
        };

        let result = sqlx::query(
            "DELETE FROM requests WHERE timestamp < NOW() - ($1::integer || ' days')::interval",
        )
        .bind(retention_days as i32)
        .execute(pool)
        .await;

        match result {
            Ok(r) => tracing::info!(
                "Stats cleanup: deleted {} rows from requests (retention={}d)",
                r.rows_affected(),
                retention_days
            ),
            Err(e) => tracing::error!("Stats cleanup requests error: {e}"),
        }

        let result = sqlx::query(
            "DELETE FROM token_usage_hourly WHERE hour < date_trunc('hour', NOW()) - ($1::integer || ' days')::interval",
        )
        .bind(retention_days as i32)
        .execute(pool)
        .await;

        match result {
            Ok(r) => tracing::info!("Stats cleanup: deleted {} rows from token_usage_hourly", r.rows_affected()),
            Err(e) => tracing::error!("Stats cleanup token_usage_hourly error: {e}"),
        }

        let result = sqlx::query(
            "DELETE FROM team_usage_hourly WHERE hour < date_trunc('hour', NOW()) - ($1::integer || ' days')::interval",
        )
        .bind(retention_days as i32)
        .execute(pool)
        .await;

        match result {
            Ok(r) => tracing::info!("Stats cleanup: deleted {} rows from team_usage_hourly", r.rows_affected()),
            Err(e) => tracing::error!("Stats cleanup team_usage_hourly error: {e}"),
        }
    }

    /// Batch-агрегация: собрать completed-часы из requests в _hourly таблицы.
    /// Использует _aggregation_cursor для отслеживания обработанного диапазона.
    async fn run_aggregation(&self) {
        let pool = match &self.pool {
            Some(p) => p,
            None => return,
        };

        // Начало транзакции
        let mut tx = match pool.begin().await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("Stats aggregation: cannot begin transaction: {e}");
                return;
            }
        };

        // Читаем курсор
        let last_ts: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> = match sqlx::query_scalar(
            "SELECT last_ts FROM _aggregation_cursor WHERE id = 1",
        )
        .fetch_one(&mut *tx)
        .await
        {
            Ok(ts) => ts,
            Err(e) => {
                tracing::error!("Stats aggregation: cannot read cursor: {e}");
                return;
            }
        };

        // Агрегируем только завершённые часы (текущий час исключаем)
        let end_ts = chrono::Utc::now()
            .with_minute(0).unwrap()
            .with_second(0).unwrap()
            .with_nanosecond(0).unwrap();

        if last_ts >= end_ts {
            // Нет завершённых часов для агрегации
            tracing::debug!(
                "Stats aggregation: nothing to aggregate (last_ts={last_ts}, end_ts={end_ts})"
            );
            return;
        }

        // --- token_usage_hourly ---
        let token_result = sqlx::query(
            "INSERT INTO token_usage_hourly (hour, token_hash, team, requests, tokens_prompt, tokens_completion, cost_approx) \
             SELECT \
               date_trunc('hour', timestamp) AS hour, \
               COALESCE(token_key_hash, '') AS token_hash, \
               team, \
               COUNT(*) AS requests, \
               SUM(tokens_prompt) AS tokens_prompt, \
               SUM(tokens_completion) AS tokens_completion, \
               SUM(cost_approx) AS cost_approx \
             FROM requests \
             WHERE timestamp >= $1 AND timestamp < $2 \
             GROUP BY 1, 2, 3 \
             ON CONFLICT (hour, token_hash) DO UPDATE SET \
               requests = token_usage_hourly.requests + EXCLUDED.requests, \
               tokens_prompt = token_usage_hourly.tokens_prompt + EXCLUDED.tokens_prompt, \
               tokens_completion = token_usage_hourly.tokens_completion + EXCLUDED.tokens_completion, \
               cost_approx = token_usage_hourly.cost_approx + EXCLUDED.cost_approx",
        )
        .bind(last_ts)
        .bind(end_ts)
        .execute(&mut *tx)
        .await;

        match token_result {
            Ok(r) => {
                if r.rows_affected() > 0 {
                    tracing::info!(
                        "Stats aggregation: token_usage_hourly upserted {} rows ({last_ts}..{end_ts})",
                        r.rows_affected()
                    );
                }
            }
            Err(e) => tracing::error!("Stats aggregation token_usage_hourly error: {e}"),
        }

        // --- team_usage_hourly ---
        let team_result = sqlx::query(
            "INSERT INTO team_usage_hourly (hour, team, requests, tokens_prompt, tokens_completion, active_tokens, cost_approx) \
             SELECT \
               date_trunc('hour', timestamp) AS hour, \
               team, \
               COUNT(*) AS requests, \
               SUM(tokens_prompt) AS tokens_prompt, \
               SUM(tokens_completion) AS tokens_completion, \
               COUNT(DISTINCT token_key_hash) AS active_tokens, \
               SUM(cost_approx) AS cost_approx \
             FROM requests \
             WHERE timestamp >= $1 AND timestamp < $2 \
             GROUP BY 1, 2 \
             ON CONFLICT (hour, team) DO UPDATE SET \
               requests = team_usage_hourly.requests + EXCLUDED.requests, \
               tokens_prompt = team_usage_hourly.tokens_prompt + EXCLUDED.tokens_prompt, \
               tokens_completion = team_usage_hourly.tokens_completion + EXCLUDED.tokens_completion, \
               active_tokens = team_usage_hourly.active_tokens + EXCLUDED.active_tokens, \
               cost_approx = team_usage_hourly.cost_approx + EXCLUDED.cost_approx",
        )
        .bind(last_ts)
        .bind(end_ts)
        .execute(&mut *tx)
        .await;

        match team_result {
            Ok(r) => {
                if r.rows_affected() > 0 {
                    tracing::info!(
                        "Stats aggregation: team_usage_hourly upserted {} rows",
                        r.rows_affected()
                    );
                }
            }
            Err(e) => tracing::error!("Stats aggregation team_usage_hourly error: {e}"),
        }

        // Обновляем курсор
        let _ = sqlx::query("UPDATE _aggregation_cursor SET last_ts = $1 WHERE id = 1")
            .bind(end_ts)
            .execute(&mut *tx)
            .await;

        // Коммит
        if let Err(e) = tx.commit().await {
            tracing::error!("Stats aggregation: commit failed: {e}");
        } else {
            tracing::debug!("Stats aggregation: committed range {last_ts}..{end_ts}");
        }
    }
}

#[cfg(not(feature = "postgres"))]
pub struct StatsWriter;

#[cfg(not(feature = "postgres"))]
impl StatsWriter {
    pub fn new(
        _url: &str,
        _retention_days: u32,
        _cleanup_interval_secs: u64,
        _aggregation_interval_secs: u64,
    ) -> Self {
        Self
    }
    pub async fn record_request(
        &self,
        _m: &str,
        _e: &str,
        _t: &str,
        _tp: u64,
        _tc: u64,
        _l: u64,
        _s: u16,
        _h: Option<&str>,
        _cp: f64,
        _cc: f64,
    ) {
    }
    pub fn start_background_tasks(self: &Arc<Self>) {}
}
