use lazy_static::lazy_static;
use prometheus::{
    register_histogram_vec, register_int_counter_vec, register_int_gauge, HistogramVec,
    IntCounterVec, IntGauge,
};

lazy_static! {
    /// Счётчик запросов по модели, эндпоинту, статусу
    pub static ref REQUEST_COUNT: IntCounterVec = register_int_counter_vec!(
        "openai_router_requests_total",
        "Total requests",
        &["model", "endpoint", "status"]
    )
    .unwrap();

    /// Гистограмма latency
    pub static ref REQUEST_LATENCY: HistogramVec = register_histogram_vec!(
        "openai_router_request_latency_seconds",
        "Request latency in seconds",
        &["model", "endpoint"],
        vec![0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
    )
    .unwrap();

    /// Количество забаненных эндпоинтов
    pub static ref BANNED_ENDPOINTS: IntGauge = register_int_gauge!(
        "openai_router_banned_endpoints",
        "Number of currently banned endpoints"
    )
    .unwrap();

    /// Количество rate limit hits
    pub static ref RATE_LIMIT_HITS: IntCounterVec = register_int_counter_vec!(
        "openai_router_rate_limit_hits_total",
        "Rate limit hits",
        &["scope", "key"]
    )
    .unwrap();

    /// Потреблённые токены
    pub static ref TOKENS_CONSUMED: IntCounterVec = register_int_counter_vec!(
        "openai_router_tokens_consumed_total",
        "Tokens consumed",
        &["model", "type"]
    )
    .unwrap();
}
