use anyhow::{Context, Result};
use std::path::Path;

use super::types::AppConfig;

/// Load configuration from a YAML file with env variable substitution.
/// Supports:
///   - ${VAR} — required variable (error if not set)
///   - ${VAR:-default} — with default value
///   - Loading .env file (if present)
pub fn load_config(path: &Path) -> Result<AppConfig> {
    // Load .env if present (non-critical)
    let _ = dotenvy::dotenv();

    let content =
        std::fs::read_to_string(path).with_context(|| format!("Cannot read {:?}", path))?;

    let expanded = expand_env_vars(&content)?;

    let config: AppConfig =
        serde_yaml::from_str(&expanded).with_context(|| "Failed to parse YAML config")?;
    validate_config(&config)?;
    Ok(config)
}

/// Substitute env variables in a string.
/// ${VAR} — required, ${VAR:-default} — with default.
fn expand_env_vars(input: &str) -> Result<String> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_name = String::new();
            let mut default_value = None;
            let mut in_default = false;

            while let Some(c) = chars.next() {
                if c == '}' {
                    break;
                }
                if c == ':' && chars.peek() == Some(&'-') && !in_default {
                    chars.next(); // consume '-'
                    in_default = true;
                    continue;
                }
                if in_default {
                    default_value.get_or_insert_with(String::new).push(c);
                } else {
                    var_name.push(c);
                }
            }

            let var_name = var_name.trim();
            if var_name.is_empty() {
                anyhow::bail!("Empty variable name in ${{}}");
            }

            match std::env::var(var_name) {
                Ok(val) => result.push_str(&val),
                Err(_) => {
                    if let Some(def) = default_value {
                        result.push_str(def.trim());
                    } else {
                        anyhow::bail!(
                            "Environment variable '{}' is not set (and no default provided)",
                            var_name
                        );
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

/// Basic config validation
fn validate_config(config: &AppConfig) -> Result<()> {
    // Verify all tokens reference existing teams
    let team_names: Vec<&str> = config.teams.iter().map(|t| t.name.as_str()).collect();

    for token in &config.tokens {
        if !team_names.contains(&token.team.as_str()) {
            anyhow::bail!(
                "Token '{}' references unknown team '{}'",
                mask_key(&token.key),
                token.team
            );
        }
    }

    // Verify every model has at least one endpoint
    for model in &config.models {
        if model.endpoints.is_empty() {
            anyhow::bail!("Model '{}' has no endpoints configured", model.name);
        }
    }

    Ok(())
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".into();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_var_expansion() {
        std::env::set_var("TEST_LISTEN", "0.0.0.0:9090");
        std::env::set_var("TEST_KEY", "sk-from-env");
        let yaml = r#"
server:
  listen: "${TEST_LISTEN}"
tokens:
  - key: "${TEST_KEY}"
    team: "default"
teams:
  - name: "default"
models:
  - name: "gpt-4"
    endpoints:
      - url: "https://api.openai.com"
        key: "sk-upstream"
"#;
        let expanded = expand_env_vars(yaml).unwrap();
        assert!(expanded.contains("0.0.0.0:9090"));
        assert!(expanded.contains("sk-from-env"));
        let cfg: AppConfig = serde_yaml::from_str(&expanded).unwrap();
        assert_eq!(cfg.server.listen, "0.0.0.0:9090");
        assert_eq!(cfg.tokens[0].key, "sk-from-env");
    }

    #[test]
    fn test_env_var_with_default() {
        let yaml = "server:\n  listen: \"${UNDEFINED_VAR:-0.0.0.0:7777}\"\n";
        let expanded = expand_env_vars(yaml).unwrap();
        assert!(expanded.contains("0.0.0.0:7777"));
    }

    #[test]
    fn test_env_var_missing_no_default() {
        let yaml = "server:\n  listen: \"${TOTALLY_MISSING_VAR}\"\n";
        assert!(expand_env_vars(yaml).is_err());
    }
}
