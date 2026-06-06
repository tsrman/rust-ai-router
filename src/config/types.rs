use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Root application configuration
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
    /// Path prefix (sub-URL), e.g. "/rustrouter".
    /// All API endpoints will be available under this prefix:
    ///   /rustrouter/v1/models, /rustrouter/health, ...
    /// Works without prefix too (backward compatibility).
    #[serde(default)]
    pub base_path: String,
    #[serde(default)]
    pub timeouts: TimeoutConfig,
    /// Maximum request body size in bytes (default 10MB)
    #[serde(default = "default_max_body_size")]
    pub max_body_size: usize,
}

fn default_max_body_size() -> usize { 10 * 1024 * 1024 }  // 10MB

fn default_listen() -> String {
    "0.0.0.0:8080".into()
}

/// Timeout settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutConfig {
    /// Client idle timeout (keep-alive), seconds. 0 = unlimited.
    #[serde(default = "default_client_idle")]
    pub client_idle_secs: u64,
    /// Client request body read timeout, seconds. 0 = unlimited.
    #[serde(default = "default_client_read")]
    pub client_read_secs: u64,
    /// Upstream connection timeout, seconds.
    #[serde(default = "default_upstream_connect")]
    pub upstream_connect_secs: u64,
    /// Upstream response read timeout, seconds. Important for streaming!
    #[serde(default = "default_upstream_read")]
    pub upstream_read_secs: u64,
    /// Upstream request write timeout, seconds.
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

/// Team (user group)
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

/// Access token (client API key)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenConfig {
    pub key: String,
    pub team: String,
    #[serde(default)]
    pub models: Option<Vec<String>>,
    #[serde(default)]
    pub limits: Option<LimitsConfig>,
}

/// RPM/TPM limits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    #[serde(default)]
    pub rpm: u32,
    #[serde(default)]
    pub tpm: u64,
}

/// Model with multiple endpoints
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub endpoints: Vec<EndpointConfig>,
    /// Model type: chat (default), audio, embedding, completions, rerank
    #[serde(default = "default_model_type")]
    pub model_type: String,
}

fn default_model_type() -> String { "chat".into() }

/// Endpoint (upstream API)
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

/// Token cost (per 1M tokens)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostConfig {
    pub prompt: f64,
    pub completion: f64,
}

/// Fail2ban settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fail2banConfig {
    #[serde(default = "default_max_failures")]
    pub max_failures: u32,
    #[serde(default = "default_ban_duration")]
    pub ban_duration_secs: u64,
    #[serde(default = "default_error_threshold")]
    pub error_threshold_pct: f64,
    /// HTTP status codes from upstream that count as errors.
    /// Format: exact code ("500", "429") or mask ("5xx" = all 5xx).
    /// Default: ["5xx", "401", "403", "429"].
    /// Network errors (connection refused, timeout) are always counted.
    #[serde(default = "default_fail_codes")]
    pub fail_status_codes: Vec<String>,
    /// Background health check interval for banned endpoints, seconds.
    /// 0 = disable check. Default: 30.
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval_secs: u64,
    /// Retry request on another endpoint upon error (5xx, 429, network).
    /// true = on error try the next endpoint, don't return 5xx to client.
    /// Default: true.
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
        "5xx".into(),   // All 5xx
        "401".into(),   // Invalid API key
        "403".into(),   // Access denied
        "429".into(),   // Upstream rate limit
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

/// PostgreSQL statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub postgres_url: Option<String>,
    /// Keep records for N days (default 30, 0 = don't clean)
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    /// Cleanup interval in seconds (default 3600 = 1 hour, 0 = disabled)
    #[serde(default = "default_cleanup_interval")]
    pub cleanup_interval_secs: u64,
    /// Batch aggregation interval in seconds (default 300 = 5 min, 0 = disabled)
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

/// Session-sticky routing settings
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

/// Effective token limits (with inheritance from team)
#[derive(Debug, Clone)]
pub struct EffectiveLimits {
    pub rpm: u32,
    pub tpm: u64,
    pub models: Vec<String>,
    pub cost_multiplier: f64,
}

/// Inter-instance synchronization via Redis/Valkey
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Connection mode: "standalone" (default) or "sentinel"
    #[serde(default = "default_sync_mode")]
    pub mode: String,
    /// URL for standalone mode (backward compatibility)
    #[serde(default)]
    pub redis_url: Option<String>,
    /// Sentinel nodes list for sentinel mode
    #[serde(default)]
    pub sentinel_nodes: Vec<String>,
    /// Master name in Sentinel
    #[serde(default)]
    pub sentinel_master_name: Option<String>,
    /// Server type: "master" (default) or "replica"
    #[serde(default = "default_sentinel_server_type")]
    pub sentinel_server_type: String,
    #[serde(default = "default_sync_prefix")]
    pub key_prefix: String,
    /// If Redis is unavailable: true = allow requests without limits, false = reject
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
    /// Get effective token limits considering team inheritance
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

    /// Check if a token has access to a model
    pub fn token_has_model_access(&self, token_key: &str, model: &str) -> bool {
        if let Some(eff) = self.resolve_token(token_key) {
            // wildcard — access to all
            if eff.models.iter().any(|m| m == "*") {
                return true;
            }
            return eff.models.iter().any(|m| m == model);
        }
        false
    }

    /// Get model config by name (with alias resolution)
    pub fn find_model(&self, model_name: &str) -> Option<&ModelConfig> {
        self.models
            .iter()
            .find(|m| m.name == model_name || m.aliases.iter().any(|a| a == model_name))
    }

    /// Get canonical model name (resolve alias)
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

