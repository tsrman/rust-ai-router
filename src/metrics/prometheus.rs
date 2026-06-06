use lazy_static::lazy_static;
use prometheus::{
    register_histogram_vec, register_int_counter_vec, register_int_gauge, HistogramVec,
    IntCounterVec, IntGauge,
};

lazy_static! {
    /// Request counter by model, endpoint, status
    pub static ref REQUEST_COUNT: IntCounterVec = register_int_counter_vec!(
        "openai_router_requests_total",
        "Total requests",
        &["model", "endpoint", "status"]
    )
    .unwrap();

    /// Latency histogram
    pub static ref REQUEST_LATENCY: HistogramVec = register_histogram_vec!(
        "openai_router_request_latency_seconds",
        "Request latency in seconds",
        &["model", "endpoint"],
        vec![0.01, 0.05, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
    )
    .unwrap();

    /// Number of banned endpoints
    pub static ref BANNED_ENDPOINTS: IntGauge = register_int_gauge!(
        "openai_router_banned_endpoints",
        "Number of currently banned endpoints"
    )
    .unwrap();

    /// Number of rate limit hits
    pub static ref RATE_LIMIT_HITS: IntCounterVec = register_int_counter_vec!(
        "openai_router_rate_limit_hits_total",
        "Rate limit hits",
        &["scope", "key"]
    )
    .unwrap();

    /// Consumed tokens
    pub static ref TOKENS_CONSUMED: IntCounterVec = register_int_counter_vec!(
        "openai_router_tokens_consumed_total",
        "Tokens consumed",
        &["model", "type"]
    )
    .unwrap();

    /// Redis connection status: 1 = connected, 0 = disconnected
    pub static ref REDIS_CONNECTED: IntGauge = register_int_gauge!(
        "openai_router_redis_connected",
        "Redis connection status (1=connected, 0=disconnected)"
    )
    .unwrap();

    /// PostgreSQL connection status: 1 = connected, 0 = disconnected
    pub static ref POSTGRES_CONNECTED: IntGauge = register_int_gauge!(
        "openai_router_postgres_connected",
        "PostgreSQL connection status (1=connected, 0=disconnected)"
    )
    .unwrap();
}

/// Force-initialize all metrics (so /metrics is not empty)
pub fn init() {
    let _ = &*REQUEST_COUNT;
    let _ = &*REQUEST_LATENCY;
    let _ = &*BANNED_ENDPOINTS;
    let _ = &*RATE_LIMIT_HITS;
    let _ = &*TOKENS_CONSUMED;
    let _ = &*REDIS_CONNECTED;
    let _ = &*POSTGRES_CONNECTED;
}
