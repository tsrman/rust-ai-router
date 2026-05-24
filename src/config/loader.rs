use anyhow::{Context, Result};
use std::path::Path;

use super::types::AppConfig;

/// Загрузить конфигурацию из YAML-файла
pub fn load_config(path: &Path) -> Result<AppConfig> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("Cannot read {:?}", path))?;
    let config: AppConfig =
        serde_yaml::from_str(&content).with_context(|| "Failed to parse YAML config")?;
    validate_config(&config)?;
    Ok(config)
}

/// Базовая валидация конфига
fn validate_config(config: &AppConfig) -> Result<()> {
    // Проверяем, что все токены ссылаются на существующие команды
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

    // Проверяем, что у каждой модели есть хотя бы один эндпоинт
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
