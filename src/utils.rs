/// Маскировать API-ключ для логов: показывает первые 4 и последние 4 символа
pub fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".into();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}

/// Маскировать URL для логов: скрывает пароль после @
pub fn mask_url(url: &str) -> String {
    if let Some(at) = url.find('@') {
        format!("redis://***@{}", &url[at + 1..])
    } else {
        url.to_string()
    }
}
