//! Трансляция Anthropic API ↔ OpenAI API.
//!
//! Клиенты отправляют запросы в формате Anthropic (`/v1/messages`),
//! роутер преобразует их в OpenAI формат, проксирует к upstream,
//! и преобразует ответ обратно в Anthropic формат.

use serde_json::{json, Value};

/// Перевести тело запроса из Anthropic Messages API в OpenAI Chat Completions.
/// Возвращает изменённый JSON и извлечённое имя модели.
pub fn translate_request(body: &Value) -> (Value, String) {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let mut openai = json!({
        "model": model,
        "messages": [],
    });

    // Перенос system в messages
    if let Some(system) = body.get("system").and_then(|v| v.as_str()) {
        if let Some(arr) = openai["messages"].as_array_mut() {
            arr.push(json!({"role": "system", "content": system}));
        }
    }

    // Перенос messages
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            // Anthropic использует content как строку или массив блоков
            let content = msg.get("content");
            if let Some(arr) = openai["messages"].as_array_mut() {
                arr.push(json!({"role": role, "content": content}));
            }
        }
    }

    // Параметры, совпадающие 1:1
    for field in &["max_tokens", "temperature", "top_p", "stream"] {
        if let Some(val) = body.get(field) {
            openai[field] = val.clone();
        }
    }

    // stop_sequences → stop
    if let Some(stop) = body.get("stop_sequences") {
        openai["stop"] = stop.clone();
    }

    // Сохраняем оригинальный запрос для обратной трансляции
    openai["__anthropic_request"] = body.clone();

    (openai, model)
}

/// Перевести ответ из OpenAI Chat Completions в Anthropic Messages API.
pub fn translate_response(openai_resp: &Value) -> Value {
    // Извлекаем сохранённый оригинальный запрос
    let _original = openai_resp.get("__anthropic_request");

    let choice = openai_resp
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first());

    let message = choice.and_then(|c| c.get("message"));
    let content = message
        .and_then(|m| m.get("content"))
        .cloned()
        .unwrap_or(Value::Null);

    let finish_reason = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|v| v.as_str())
        .unwrap_or("stop");

    // OpenAI finish_reason → Anthropic stop_reason
    let stop_reason = match finish_reason {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "content_filter" => "content_filter",
        _ => "end_turn",
    };

    let model = openai_resp
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let usage = openai_resp.get("usage");

    let mut anthropic = json!({
        "id": format!("msg_{}", uuid::Uuid::new_v4()),
        "type": "message",
        "role": "assistant",
        "content": [{
            "type": "text",
            "text": content
        }],
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
    });

    if let Some(u) = usage {
        anthropic["usage"] = json!({
            "input_tokens": u.get("prompt_tokens").cloned().unwrap_or(json!(0)),
            "output_tokens": u.get("completion_tokens").cloned().unwrap_or(json!(0)),
        });
    }

    anthropic
}

/// Определить, является ли запрос Anthropic-форматом (по URL пути).
pub fn is_anthropic_request(path: &str) -> bool {
    path.ends_with("/v1/messages")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_basic_request() {
        let input = json!({
            "model": "claude-3-opus",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 100
        });
        let (openai, model) = translate_request(&input);
        assert_eq!(model, "claude-3-opus");
        assert_eq!(openai["model"], "claude-3-opus");
        assert_eq!(openai["max_tokens"], 100);
        let msgs = openai["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn test_translate_request_with_system() {
        let input = json!({
            "model": "claude-3",
            "system": "You are helpful",
            "messages": [
                {"role": "user", "content": "Hi"}
            ],
            "max_tokens": 50
        });
        let (openai, _) = translate_request(&input);
        let msgs = openai["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2); // system + user
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are helpful");
    }

    #[test]
    fn test_translate_request_stop_sequences() {
        let input = json!({
            "model": "claude-3",
            "messages": [],
            "stop_sequences": ["END", "STOP"]
        });
        let (openai, _) = translate_request(&input);
        assert_eq!(openai["stop"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_translate_response() {
        let openai_resp = json!({
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        let anthropic = translate_response(&openai_resp);
        assert_eq!(anthropic["type"], "message");
        assert_eq!(anthropic["role"], "assistant");
        assert_eq!(anthropic["stop_reason"], "end_turn");
        assert_eq!(anthropic["content"][0]["text"], "Hello!");
        assert_eq!(anthropic["usage"]["input_tokens"], 10);
        assert_eq!(anthropic["usage"]["output_tokens"], 5);
    }

    #[test]
    fn test_translate_response_max_tokens_stop() {
        let openai_resp = json!({
            "choices": [{
                "message": {"content": "truncated..."},
                "finish_reason": "length"
            }]
        });
        let anthropic = translate_response(&openai_resp);
        assert_eq!(anthropic["stop_reason"], "max_tokens");
    }

    #[test]
    fn test_is_anthropic_request() {
        assert!(is_anthropic_request("/v1/messages"));
        assert!(is_anthropic_request("/api/v1/messages"));
        assert!(!is_anthropic_request("/v1/chat/completions"));
        assert!(!is_anthropic_request("/health"));
    }
}
