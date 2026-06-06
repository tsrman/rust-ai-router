use arc_swap::ArcSwap;
use dashmap::DashMap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::config::{AppConfig, EndpointConfig};

/// Endpoint selection result
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SelectedEndpoint {
    pub url: String,
    pub api_key: String,
    pub index: usize,
    pub total_endpoints: usize,
    pub cost_prompt: f64,
    pub cost_completion: f64,
    pub endpoint_limits_rpm: u32,
    pub endpoint_limits_tpm: u64,
}

/// Round-robin counters per model
pub struct RoundRobinCounters {
    counters: DashMap<String, AtomicU64>,
}

impl RoundRobinCounters {
    fn new() -> Self {
        Self { counters: DashMap::new() }
    }

    fn next(&self, model: &str, total: usize) -> usize {
        let counter = self.counters.entry(model.to_string()).or_insert_with(|| AtomicU64::new(0));
        (counter.fetch_add(1, Ordering::Relaxed) as usize) % total
    }
}

/// Model router: weighted round-robin + session sticky
pub struct ModelRouter {
    config: Arc<ArcSwap<AppConfig>>,
    round_robin: RoundRobinCounters,
}

impl ModelRouter {
    pub fn new(config: Arc<ArcSwap<AppConfig>>) -> Self {
        Self {
            config,
            round_robin: RoundRobinCounters::new(),
        }
    }

    /// Select an endpoint via weighted round-robin
    #[allow(dead_code)]
    pub fn select_endpoint(&self, model_name: &str) -> Option<SelectedEndpoint> {
        let cfg = self.config.load();
        let model = cfg.find_model(model_name)?;

        if model.endpoints.is_empty() {
            return None;
        }

        self.pick_weighted(&model.name, &model.endpoints)
    }

    /// Select an available endpoint, skipping banned and already tried ones.
    pub fn select_available(
        &self,
        model_name: &str,
        banned_keys: &HashSet<String>,
        tried_keys: &HashSet<String>,
    ) -> Option<SelectedEndpoint> {
        let cfg = self.config.load();
        let model = cfg.find_model(model_name)?;

        let endpoints: Vec<&EndpointConfig> = model.endpoints.iter().collect();
        let total = endpoints.len();
        if total == 0 {
            return None;
        }

        // Try up to total times — one round-robin pass
        let start = self.round_robin.next(&model.name, total);
        for offset in 0..total {
            let idx = (start + offset) % total;
            let ep = endpoints[idx];
            let key = format!("{}:{}", ep.url, mask_key(&ep.key));

            if banned_keys.contains(&key) || tried_keys.contains(&key) {
                continue;
            }

            return Some(SelectedEndpoint {
                url: ep.url.clone(),
                api_key: ep.key.clone(),
                index: idx,
                total_endpoints: total,
                cost_prompt: ep.cost.as_ref().map(|c| c.prompt).unwrap_or(0.0),
                cost_completion: ep.cost.as_ref().map(|c| c.completion).unwrap_or(0.0),
                endpoint_limits_rpm: ep.limits.as_ref().map(|l| l.rpm).unwrap_or(0),
                endpoint_limits_tpm: ep.limits.as_ref().map(|l| l.tpm).unwrap_or(0),
            });
        }

        None
    }

    /// Select an endpoint with session-sticky affinity (considering weights)
    #[allow(dead_code)]
    pub fn select_sticky(&self, model_name: &str, session_id: &str) -> Option<SelectedEndpoint> {
        let cfg = self.config.load();
        let model = cfg.find_model(model_name)?;

        // Use session_id hash for deterministic selection
        let hash = session_id.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));

        self.pick_weighted_with_offset(&model.name, &model.endpoints, hash as usize)
    }

    /// Weighted round-robin
    fn pick_weighted(&self, model_name: &str, endpoints: &[EndpointConfig]) -> Option<SelectedEndpoint> {
        let total = endpoints.len();
        let idx = self.round_robin.next(model_name, total);
        self.build_selected(endpoints, idx, total)
    }

    /// Weighted selection with offset (for sticky)
    fn pick_weighted_with_offset(&self, _model_name: &str, endpoints: &[EndpointConfig], offset: usize) -> Option<SelectedEndpoint> {
        let total = endpoints.len();
        if total == 0 {
            return None;
        }

        // Consider weights: skip endpoints with weight=0
        let weights: Vec<usize> = endpoints.iter()
            .map(|ep| if ep.weight == 0 { 1 } else { ep.weight } as usize)
            .collect();

        let total_weight: usize = weights.iter().sum();
        if total_weight == 0 {
            return None;
        }

        let pick = offset % total_weight;
        let mut cumulative = 0usize;
        for (i, w) in weights.iter().enumerate() {
            cumulative += w;
            if pick < cumulative {
                return self.build_selected(endpoints, i, total);
            }
        }

        self.build_selected(endpoints, 0, total)
    }

    fn build_selected(&self, endpoints: &[EndpointConfig], index: usize, total: usize) -> Option<SelectedEndpoint> {
        let ep = endpoints.get(index)?;
        Some(SelectedEndpoint {
            url: ep.url.clone(),
            api_key: ep.key.clone(),
            index,
            total_endpoints: total,
            cost_prompt: ep.cost.as_ref().map(|c| c.prompt).unwrap_or(0.0),
            cost_completion: ep.cost.as_ref().map(|c| c.completion).unwrap_or(0.0),
            endpoint_limits_rpm: ep.limits.as_ref().map(|l| l.rpm).unwrap_or(0),
            endpoint_limits_tpm: ep.limits.as_ref().map(|l| l.tpm).unwrap_or(0),
        })
    }
}

fn mask_key(key: &str) -> String {
    if key.len() <= 6 {
        return "***".to_string();
    }
    format!("{}...{}", &key[..3], &key[key.len()-3..])
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(yaml: &str) -> Arc<ArcSwap<AppConfig>> {
        let cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
        Arc::new(ArcSwap::from_pointee(cfg))
    }

    #[test]
    fn test_select_endpoint_single() {
        let yaml = r#"
server:
  listen: "0.0.0.0:8080"
models:
  - name: "gpt-4"
    endpoints:
      - url: "https://api.openai.com"
        key: "sk-test"
"#;
        let config = make_config(yaml);
        let router = ModelRouter::new(config);
        let ep = router.select_endpoint("gpt-4").unwrap();
        assert_eq!(ep.url, "https://api.openai.com");
        assert_eq!(ep.index, 0);
    }

    #[test]
    fn test_select_endpoint_unknown_model() {
        let config = make_config(r#"server: {listen: "0.0.0.0:8080"}"#);
        let router = ModelRouter::new(config);
        assert!(router.select_endpoint("nonexistent").is_none());
    }

    #[test]
    fn test_select_available_skips_banned() {
        let yaml = r#"
server:
  listen: "0.0.0.0:8080"
models:
  - name: "gpt-4"
    endpoints:
      - url: "https://ep1.example.com"
        key: "sk-111"
      - url: "https://ep2.example.com"
        key: "sk-222"
"#;
        let config = make_config(yaml);
        let router = ModelRouter::new(config);

        let mut banned = HashSet::new();
        let ep1_key = format!("{}:{}", "https://ep1.example.com", mask_key("sk-111"));
        banned.insert(ep1_key);

        let tried = HashSet::new();
        let ep = router.select_available("gpt-4", &banned, &tried).unwrap();
        assert_eq!(ep.url, "https://ep2.example.com");
    }

    #[test]
    fn test_select_available_all_banned() {
        let yaml = r#"
server:
  listen: "0.0.0.0:8080"
models:
  - name: "gpt-4"
    endpoints:
      - url: "https://ep1.example.com"
        key: "sk-111"
"#;
        let config = make_config(yaml);
        let router = ModelRouter::new(config);

        let mut banned = HashSet::new();
        banned.insert(format!("{}:{}", "https://ep1.example.com", mask_key("sk-111")));

        let tried = HashSet::new();
        assert!(router.select_available("gpt-4", &banned, &tried).is_none());
    }

    #[test]
    fn test_round_robin_cycles() {
        let yaml = r#"
server:
  listen: "0.0.0.0:8080"
models:
  - name: "gpt-4"
    endpoints:
      - url: "https://ep1.example.com"
        key: "sk-1"
      - url: "https://ep2.example.com"
        key: "sk-2"
      - url: "https://ep3.example.com"
        key: "sk-3"
"#;
        let config = make_config(yaml);
        let router = ModelRouter::new(config);

        let mut seen = HashSet::new();
        for _ in 0..6 {
            let ep = router.select_endpoint("gpt-4").unwrap();
            seen.insert(ep.index);
        }
        // All 3 endpoints should have been picked at least once
        assert_eq!(seen.len(), 3);
    }
}
