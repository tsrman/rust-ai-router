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

/// Сформировать OpenAI-совместимый JSON-ответ с ошибкой
pub fn json_error(
    status: axum::http::StatusCode,
    message: &str,
    err_type: &str,
    code: &str,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": err_type,
            "param": null,
            "code": code
        }
    });
    (status, axum::Json(body)).into_response()
}
