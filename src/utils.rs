/// Mask API key for logs: shows first 4 and last 4 characters
pub fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".into();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}

/// Mask Redis URL for logs: hides password after @
pub fn mask_redis_url(url: &str) -> String {
    if let Some(at) = url.find('@') {
        format!("redis://***@{}", &url[at + 1..])
    } else {
        url.to_string()
    }
}

/// Partially mask an endpoint URL: hides subdomain / IP and each
/// path segment (first 4 + last 4 characters, like API key).
pub fn mask_endpoint_url(url: &str) -> String {
    use std::net::IpAddr;

    fn mask_segment(seg: &str) -> String {
        if seg.len() <= 8 {
            seg.to_string()
        } else {
            format!("{}...{}", &seg[..4], &seg[seg.len() - 4..])
        }
    }

    if let Ok(parsed) = reqwest::Url::parse(url) {
        let host = parsed.host_str().unwrap_or("");

        let masked_host = if host.parse::<IpAddr>().is_ok() {
            if let Some(pos) = host.find('.') {
                format!("{}.***", &host[..pos])
            } else {
                "***".into()
            }
        } else {
            let parts: Vec<&str> = host.split('.').collect();
            if parts.len() >= 2 {
                let first = parts[0];
                let prefix = if first.len() > 3 { &first[..3] } else { first };
                let rest = parts[1..].join(".");
                format!("{}***.{}", prefix, rest)
            } else {
                host.to_string()
            }
        };

        let mut result = format!("{}://{}", parsed.scheme(), masked_host);
        if let Some(port) = parsed.port() {
            result.push_str(&format!(":{}", port));
        }

        let path = parsed.path();
        let masked_path = path
            .split('/')
            .map(|seg| if seg.is_empty() { String::new() } else { mask_segment(seg) })
            .collect::<Vec<_>>()
            .join("/");
        result.push_str(&masked_path);

        result
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::mask_endpoint_url;

    #[test]
    fn test_mask_endpoint_url_domain() {
        assert_eq!(
            mask_endpoint_url("https://api.openai.com/v1/chat/completions"),
            "https://api***.openai.com/v1/chat/comp...ions"
        );
    }

    #[test]
    fn test_mask_endpoint_url_ip() {
        assert_eq!(
            mask_endpoint_url("http://10.0.0.1:8081/v1/models"),
            "http://10.***:8081/v1/models"
        );
    }

    #[test]
    fn test_mask_endpoint_url_long_path_segment() {
        assert_eq!(
            mask_endpoint_url("https://example.com/zsddf-34343-sfdfdfdf-343545/"),
            "https://exa***.com/zsdd...3545/"
        );
    }

    #[test]
    fn test_mask_endpoint_url_short_path_segments() {
        assert_eq!(
            mask_endpoint_url("https://example.com/v1/models"),
            "https://exa***.com/v1/models"
        );
    }

    #[test]
    fn test_mask_endpoint_url_two_part_domain() {
        assert_eq!(
            mask_endpoint_url("https://openai.com/v1"),
            "https://ope***.com/v1"
        );
    }
}

/// Build an OpenAI-compatible JSON error response
pub fn json_error(
    status: axum::http::StatusCode,
    message: &str,
    err_type: &str,
    code: &str,
) -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": err_type,
            "param": null,
            "code": code
        }
    });
    let body_str = serde_json::to_string(&body).unwrap_or_default();
    let mut resp = axum::response::Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body_str))
        .unwrap_or_else(|_| (status, "internal error").into_response());

    // Add WWW-Authenticate header for 401 responses (HTTP spec requirement)
    if status == axum::http::StatusCode::UNAUTHORIZED {
        resp.headers_mut()
            .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
    }

    resp
}
