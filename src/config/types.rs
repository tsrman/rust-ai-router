use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Корневая конфигурация приложения
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    #[serde(default)]
    pub teams: Vec<TeamConfig>,
    #[serde(default)]
    pub tokens: Vec<TokenConfig>,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    #[serde(default)]
    pub fail2ban: Fail2banConfig,
    #[serde(default)]
    pub stats: StatsConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub sync: SyncConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Префикс пути (sub-URL), например "/rustrouter".
    /// Все API-эндпоинты будут доступны по этому префиксу:
    ///   /rustrouter/v1/models, /rustrouter/health, ...
    /// Без префикса тоже работают (для обратной совместимости).
    #[serde(default)]
    pub base_path: String,
    #[serde(default)]
    pub timeouts: TimeoutConfig,
    /// Максимальный размер тела запроса в байтах (по умолчанию 10MB)
    #[serde(default = "default_max_body_size")]
    pub max_body_size: usize,
}

fn default_max_body_size() -> usize { 10 * 1024 * 1024 }  // 10MB

fn default_listen() -> String {
    "0.0.0.0:8080".into()
}

/// Настройки таймаутов
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutConfig {
    /// Таймаут ожидания от клиента (keep-alive / idle), секунд. 0 = безлимит.
    #[serde(default = "default_client_idle")]
    pub client_idle_secs: u64,
    /// Таймаут чтения тела запроса от клиента, секунд. 0 = безлимит.
    #[serde(default = "default_client_read")]
    pub client_read_secs: u64,
    /// Таймаут подключения к upstream, секунд.
    #[serde(default = "default_upstream_connect")]
    pub upstream_connect_secs: u64,
    /// Таймаут чтения ответа от upstream, секунд. Важно для streaming!
    #[serde(default = "default_upstream_read")]
    pub upstream_read_secs: u64,
    /// Таймаут отправки запроса в upstream, секунд.
    #[serde(default = "default_upstream_write")]
    pub upstream_write_secs: u64,
}

fn default_client_idle() -> u64 { 60 }
fn default_client_read() -> u64 { 30 }
fn default_upstream_connect() -> u64 { 10 }
fn default_upstream_read() -> u64 { 300 }
fn default_upstream_write() -> u64 { 60 }

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            client_idle_secs: 60,
            client_read_secs: 30,
            upstream_connect_secs: 10,
            upstream_read_secs: 300,
            upstream_write_secs: 60,
        }
    }
}

/// Команда (группа пользователей)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub name: String,
    #[serde(default = "default_team_models")]
    pub models: Vec<String>,
    #[serde(default)]
    pub limits: Option<LimitsConfig>,
    #[serde(default)]
    pub cost_multiplier: Option<f64>,
}

fn default_team_models() -> Vec<String> {
    vec!["*".into()]
}

/// Токен доступа (API ключ клиента)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenConfig {
    pub key: String,
    pub team: String,
    #[serde(default)]
    pub models: Option<Vec<String>>,
    #[serde(default)]
    pub limits: Option<LimitsConfig>,
}

/// Лимиты RPM/TPM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    #[serde(default)]
    pub rpm: u32,
    #[serde(default)]
    pub tpm: u64,
}

/// Модель с несколькими эндпоинтами
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub endpoints: Vec<EndpointConfig>,
    /// Тип модели: chat (по умолчанию), audio, embedding, completions, rerank
    #[serde(default = "default_model_type")]
    pub model_type: String,
}

fn default_model_type() -> String { "chat".into() }

/// Эндпоинт (upstream API)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointConfig {
    pub url: String,
    pub key: String,
    #[serde(default)]
    pub limits: Option<LimitsConfig>,
    #[serde(default)]
    pub cost: Option<CostConfig>,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

fn default_weight() -> u32 {
    1
}

/// Стоимость токенов (за 1M токенов)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostConfig {
    pub prompt: f64,
    pub completion: f64,
}

/// Настройки Fail2ban
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fail2banConfig {
    #[serde(default = "default_max_failures")]
    pub max_failures: u32,
    #[serde(default = "default_ban_duration")]
    pub ban_duration_secs: u64,
    #[serde(default = "default_error_threshold")]
    pub error_threshold_pct: f64,
    /// HTTP-статусы от upstream, которые считаются ошибкой.
    /// Формат: точный код ("500", "429") или маска ("5xx" = все 5xx).
    /// По умолчанию: ["5xx", "401", "403", "429"].
    /// Сетевые ошибки (connection refused, timeout) учитываются всегда.
    #[serde(default = "default_fail_codes")]
    pub fail_status_codes: Vec<String>,
    /// Интервал фоновой проверки забаненных эндпоинтов, секунд.
    /// 0 = отключить проверку. По умолчанию: 30.
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval_secs: u64,
    /// Ретраить запрос на другие эндпоинты при ошибке (5xx, 429, сетевые).
    /// true = при ошибке пробуем следующий эндпоинт, не возвращая 5xx клиенту.
    /// По умолчанию: true.
    #[serde(default = "default_retry_on_failure")]
    pub retry_on_failure: bool,
}

fn default_retry_on_failure() -> bool {
    true
}

fn default_health_check_interval() -> u64 {
    30
}

fn default_fail_codes() -> Vec<String> {
    vec![
        "5xx".into(),   // Все 5xx
        "401".into(),   // Невалидный API ключ
        "403".into(),   // Доступ запрещён
        "429".into(),   // Rate limit upstream
    ]
}

fn default_max_failures() -> u32 {
    5
}
fn default_ban_duration() -> u64 {
    60
}
fn default_error_threshold() -> f64 {
    0.5
}

impl Default for Fail2banConfig {
    fn default() -> Self {
        Self {
            max_failures: 5,
            ban_duration_secs: 60,
            error_threshold_pct: 0.5,
            fail_status_codes: default_fail_codes(),
            health_check_interval_secs: 30,
            retry_on_failure: true,
        }
    }
}

/// Статистика в PostgreSQL
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub postgres_url: Option<String>,
    /// Хранить записи N дней (по умолчанию 30, 0 = не чистить)
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    /// Интервал очистки в секундах (по умолчанию 3600 = 1 час, 0 = отключено)
    #[serde(default = "default_cleanup_interval")]
    pub cleanup_interval_secs: u64,
    /// Интервал batch-агрегации в секундах (по умолчанию 300 = 5 мин, 0 = отключено)
    #[serde(default = "default_aggregation_interval")]
    pub aggregation_interval_secs: u64,
}

fn default_retention_days() -> u32 { 30 }
fn default_cleanup_interval() -> u64 { 3600 }
fn default_aggregation_interval() -> u64 { 300 }

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            postgres_url: None,
            retention_days: default_retention_days(),
            cleanup_interval_secs: default_cleanup_interval(),
            aggregation_interval_secs: default_aggregation_interval(),
        }
    }
}

/// Настройки session-sticky routing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_sticky_ttl")]
    pub sticky_ttl_secs: u64,
}

fn default_sticky_ttl() -> u64 {
    300
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            sticky_ttl_secs: 300,
        }
    }
}

/// Разрешённые лимиты для токена (с учётом наследования от команды)
#[derive(Debug, Clone)]
pub struct EffectiveLimits {
    pub rpm: u32,
    pub tpm: u64,
    pub models: Vec<String>,
    pub cost_multiplier: f64,
}

/// Настройки синхронизации между экземплярами через Redis/Valkey
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Режим подключения: "standalone" (по умолчанию) или "sentinel"
    #[serde(default = "default_sync_mode")]
    pub mode: String,
    /// URL для standalone режима (обратная совместимость)
    #[serde(default)]
    pub redis_url: Option<String>,
    /// Список Sentinel-нод для режима sentinel
    #[serde(default)]
    pub sentinel_nodes: Vec<String>,
    /// Имя мастера в Sentinel
    #[serde(default)]
    pub sentinel_master_name: Option<String>,
    /// Тип сервера: "master" (по умолчанию) или "replica"
    #[serde(default = "default_sentinel_server_type")]
    pub sentinel_server_type: String,
    #[serde(default = "default_sync_prefix")]
    pub key_prefix: String,
    /// Если Redis недоступен: true = пропускать запросы без лимитов, false = отклонять
    #[serde(default = "default_true")]
    pub fail_open: bool,
}

fn default_true() -> bool { true }

fn default_sync_mode() -> String { "standalone".into() }

fn default_sync_prefix() -> String { "oar".into() }

fn default_sentinel_server_type() -> String { "master".into() }

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "standalone".into(),
            redis_url: None,
            sentinel_nodes: Vec::new(),
            sentinel_master_name: None,
            sentinel_server_type: "master".into(),
            key_prefix: "oar".into(),
            fail_open: true,
        }
    }
}

impl AppConfig {
    /// Получить эффективные лимиты токена с учётом наследования от команды
    pub fn resolve_token(&self, token_key: &str) -> Option<EffectiveLimits> {
        let token = self.tokens.iter().find(|t| t.key == token_key)?;
        let team = self.teams.iter().find(|t| t.name == token.team);

        let models = token
            .models
            .clone()
            .or_else(|| team.and_then(|t| Some(t.models.clone())))
            .unwrap_or_else(|| vec!["*".into()]);

        let team_limits = team.and_then(|t| t.limits.as_ref());
        let token_limits = token.limits.as_ref();

        let rpm = token_limits
            .map(|l| l.rpm)
            .or_else(|| team_limits.map(|l| l.rpm))
            .unwrap_or(0);

        let tpm = token_limits
            .map(|l| l.tpm)
            .or_else(|| team_limits.map(|l| l.tpm))
            .unwrap_or(0);

        let cost_multiplier = team.and_then(|t| t.cost_multiplier).unwrap_or(1.0);

        Some(EffectiveLimits {
            rpm,
            tpm,
            models,
            cost_multiplier,
        })
    }

    /// Проверить, имеет ли токен доступ к модели
    pub fn token_has_model_access(&self, token_key: &str, model: &str) -> bool {
        if let Some(eff) = self.resolve_token(token_key) {
            // wildcard — доступ ко всем
            if eff.models.iter().any(|m| m == "*") {
                return true;
            }
            return eff.models.iter().any(|m| m == model);
        }
        false
    }

    /// Получить конфигурацию модели по имени (с учётом алиасов)
    pub fn find_model(&self, model_name: &str) -> Option<&ModelConfig> {
        self.models
            .iter()
            .find(|m| m.name == model_name || m.aliases.iter().any(|a| a == model_name))
    }

    /// Получить каноническое имя модели (разрешить алиас)
    pub fn canonical_model_name(&self, model_name: &str) -> String {
        self.find_model(model_name)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| model_name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> AppConfig {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn test_basic_parsing() {
        let cfg = parse(r#"
server:
  listen: "0.0.0.0:8080"
tokens:
  - key: "sk-test"
    team: "default"
teams:
  - name: "default"
    models: ["gpt-4"]
models:
  - name: "gpt-4"
    endpoints:
      - url: "https://api.openai.com"
        key: "sk-upstream"
"#);
        assert_eq!(cfg.tokens.len(), 1);
        assert_eq!(cfg.teams.len(), 1);
        assert_eq!(cfg.models.len(), 1);
    }

    #[test]
    fn test_resolve_token_with_limits() {
        let cfg = parse(r#"
server:
  listen: "0.0.0.0:8080"
teams:
  - name: "default"
tokens:
  - key: "sk-test"
    team: "default"
    limits: { rpm: 100, tpm: 50000 }
"#);
        let eff = cfg.resolve_token("sk-test").unwrap();
        assert_eq!(eff.rpm, 100);
        assert_eq!(eff.tpm, 50000);
    }

    #[test]
    fn test_resolve_token_inherits_from_team() {
        let cfg = parse(r#"
server:
  listen: "0.0.0.0:8080"
teams:
  - name: "default"
    models: ["gpt-4", "gpt-3.5"]
    limits: { rpm: 50 }
tokens:
  - key: "sk-test"
    team: "default"
    # token doesn't specify models or limits — inherits from team
"#);
        let eff = cfg.resolve_token("sk-test").unwrap();
        assert_eq!(eff.rpm, 50);           // inherited from team
        assert_eq!(eff.tpm, 0);            // team has no tpm
        assert!(eff.models.contains(&"gpt-4".to_string()));
    }

    #[test]
    fn test_resolve_token_overrides_team() {
        let cfg = parse(r#"
server:
  listen: "0.0.0.0:8080"
teams:
  - name: "default"
    limits: { rpm: 10 }
tokens:
  - key: "sk-test"
    team: "default"
    limits: { rpm: 500 }      # token overrides team
"#);
        let eff = cfg.resolve_token("sk-test").unwrap();
        assert_eq!(eff.rpm, 500);  // token's value, not team's 10
    }

    #[test]
    fn test_resolve_token_unknown() {
        let cfg = parse(r#"server: {listen: "0.0.0.0:8080"}"#);
        assert!(cfg.resolve_token("nonexistent").is_none());
    }

    #[test]
    fn test_token_has_model_access_wildcard() {
        let cfg = parse(r#"
server:
  listen: "0.0.0.0:8080"
teams:
  - name: "default"
    models: ["*"]
tokens:
  - key: "sk-test"
    team: "default"
"#);
        assert!(cfg.token_has_model_access("sk-test", "gpt-4"));
        assert!(cfg.token_has_model_access("sk-test", "any-model"));
    }

    #[test]
    fn test_token_has_model_access_restricted() {
        let cfg = parse(r#"
server:
  listen: "0.0.0.0:8080"
tokens:
  - key: "sk-test"
    team: "default"
    models: ["gpt-4"]
teams:
  - name: "default"
"#);
        assert!(cfg.token_has_model_access("sk-test", "gpt-4"));
        assert!(!cfg.token_has_model_access("sk-test", "gpt-3.5"));
    }

    #[test]
    fn test_canonical_model_name_alias() {
        let cfg = parse(r#"
server:
  listen: "0.0.0.0:8080"
models:
  - name: "deepseek-chat"
    aliases: ["deepseek", "ds"]
    endpoints:
      - url: "https://api.example.com"
        key: "sk-test"
"#);
        assert_eq!(cfg.canonical_model_name("deepseek"), "deepseek-chat");
        assert_eq!(cfg.canonical_model_name("ds"), "deepseek-chat");
        assert_eq!(cfg.canonical_model_name("unknown"), "unknown");
    }

    #[test]
    fn test_validation_unknown_team() {
        let result: Result<AppConfig, _> = serde_yaml::from_str(r#"
server:
  listen: "0.0.0.0:8080"
tokens:
  - key: "sk-test"
    team: "nonexistent"
teams:
  - name: "default"
"#);
        // serde_yaml succeeds, but our validate step would fail.
        // This test verifies serde deserializes without error.
        assert!(result.is_ok());
    }
}

