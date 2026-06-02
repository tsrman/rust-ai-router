# openai-router

[![Rust](https://img.shields.io/badge/rust-1.80+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

High-performance OpenAI-compatible API router written in Rust. A lightweight alternative to [litellm](https://github.com/BerriAI/litellm) — load balance, rate limit, and control access to multiple OpenAI-compatible backends through a single endpoint.

## Features

- **Multi-provider routing** — round-robin load balancing across multiple credentials per model
- **Anthropic API translation** — accepts `/v1/messages` requests, translates to OpenAI, proxies, translates response back
- **Session-sticky routing** — same session → same backend, maximises prompt cache hits
- **Fail2ban + retry** — automatic ban on repeated errors, retry on other endpoints when 5xx/429/401/403
- **Rate limiting** — per-token, per-team, and per-credential RPM/TPM controls (token bucket)
- **Access control** — token → team inheritance: models, limits, cost multiplier
- **Model aliases** — map `gpt-4`, `gpt-4-vision`, `gpt-4-turbo` → `gpt-4o`
- **Hot reload** — edit `config.yaml` and changes apply instantly; invalid config does NOT break the service
- **Health dashboard** — JSON API (`/health`) + HTML dashboard (`/vhealth`) with live per-endpoint status
- **Prometheus metrics** — request counts, latency histograms, banned endpoints, rate limit hits
- **Background health checks** — periodic probes of banned endpoints, auto-unban on recovery
- **Streaming** — SSE pass-through for `stream: true` requests
- **Cost tracking** — per-token cost headers (`x-cost-prompt-per-1m`, `x-cost-completion-per-1m`)
- **PostgreSQL stats** — per-request, per-token, and per-team hourly aggregation
- **OpenAI-compatible** — drop-in replacement for `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models`;
  also accepts **Anthropic** `/v1/messages` format

## Quick Start

```bash
# Build
cargo build --release

# Create config
cp config.example.yaml config.yaml
# Edit config.yaml — add your API keys and tokens

# Run
./target/release/openai-router config.yaml

# Or via environment variable
OPENAI_ROUTER_CONFIG=/etc/openai-router/config.yaml ./target/release/openai-router
```

Use it as a drop-in OpenAI replacement:

```bash
# List models
curl -H "Authorization: Bearer sk-admin-xxx" http://localhost:8080/v1/models

# Chat completion
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer sk-admin-xxx" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"Hello!"}]}'

# Health check
curl http://localhost:8080/health

# HTML dashboard
open http://localhost:8080/vhealth
```

## Architecture

```
                    ┌──────────────────────────────────┐
                    │        openai-router              │
                    │                                   │
  Client ──────────►│  Auth Middleware (Bearer token)   │
  (OpenAI SDK)     │    ├─ Token → Team lookup          │
                    │    └─ Access control              │
                    │                                   │
                    │  Rate Limiter (token bucket)      │
                    │    ├─ Per-token RPM/TPM           │
                    │    ├─ Per-team RPM/TPM            │
                    │    └─ Per-credential RPM/TPM      │
                    │                                   │
                    │  Router + Load Balancer           │
                    │    ├─ Weighted round-robin         │
                    │    ├─ Session-sticky routing       │
                    │    └─ Model alias resolution       │
                    │                                   │
                    │  Fail2ban (circuit breaker)       │
                    │    ├─ Consecutive failure counter  │
                    │    └─ Error rate threshold         │
                    │                                   │
                    │  Reverse Proxy                    │
                    │    ├─ /v1/chat/completions         │
                    │    ├─ /v1/completions              │
                    │    ├─ /v1/embeddings               │
                    │    └─ SSE streaming passthrough    │
                    │                                   │
                    │  Metrics & Health                 │
                    │    ├─ Prometheus /metrics          │
                    │    ├─ /health (JSON)               │
                    │    └─ /vhealth (HTML dashboard)    │
                    │                                   │
                    │  Config (YAML, hot-reload)         │
                    │  Optional: PostgreSQL stats        │
                    └──────────────────────────────────┘
```

## Configuration Reference

Configuration is stored in a single YAML file. See [`config.example.yaml`](config.example.yaml) for a full annotated example.

### Top-level structure

```yaml
server:        # Server settings
teams:         # Team definitions (access groups)
tokens:        # API tokens (client keys)
models:        # Model definitions with endpoints
fail2ban:      # Circuit breaker settings
session:       # Session sticky routing settings
stats:         # PostgreSQL statistics (optional)
```

### Server

```yaml
server:
  listen: "0.0.0.0:8080"     # Bind address (default: 0.0.0.0:8080)
  base_path: ""              # Sub-URL prefix, e.g. "/rustrouter"
  # base_path: "/rustrouter" # → /rustrouter/v1/models, /rustrouter/health, ...
  max_body_size: 10485760    # Max request body in bytes (default 10MB)
  timeouts:
    client_idle_secs: 60         # Keep-alive idle timeout, 0 = no limit
    client_read_secs: 30         # Full request cycle timeout, 0 = no limit
    upstream_connect_secs: 10    # Upstream connection timeout
    upstream_read_secs: 300      # Upstream response read timeout (streaming!)
    upstream_write_secs: 60      # Upstream request write timeout
```

**Timeout behavior:**

| Timeout | Scope | What happens on expiry |
|---------|-------|----------------------|
| `client_idle_secs` | Keep-alive connections | reqwest drops idle pool connections |
| `client_read_secs` | Full request cycle | Router returns 504 Gateway Timeout |
| `upstream_connect_secs` | TCP connect to upstream | Request fails, fail2ban records error |
| `upstream_read_secs` | Response from upstream | Request fails (critical for streaming) |
| `upstream_write_secs` | Sending body to upstream | Request fails |

> **Note:** For production, client-facing timeouts are best handled by a reverse proxy (nginx/Caddy).
> See the [Reverse Proxy Deployment](#reverse-proxy-deployment) section below.

### Teams

Teams define groups of users with shared model access and rate limits.

**Team limits are a SHARED budget** — all tokens in the team draw from the same
RPM/TPM pool. Each token can also have its own personal limit on top.

```yaml
teams:
  - name: "admin"            # Team name (used by tokens)
    models: ["*"]            # Allowed models — "*" = all models
    limits:
      rpm: 10000             # Shared budget across ALL admin tokens
      tpm: 5000000           # Shared token budget
    cost_multiplier: 1.0     # Cost multiplier for billing (default: 1.0)

  - name: "developers"
    models: ["gpt-4o", "gpt-4o-mini"]
    limits:
      rpm: 500               # All developer tokens share 500 RPM
      tpm: 200000
```

Rate limit order: **team shared budget → token personal limit**

| Level | Key | Scope |
|-------|-----|-------|
| Team | `team:<name>` | All tokens in the team share one counter |
| Token | `<token_key>` | Per-token limit (inherits from team if not set) |

### Tokens

Tokens are API keys that clients use. Each token belongs to a team and inherits its settings by default.

```yaml
tokens:
  - key: "sk-admin-secret-key"     # The Bearer token value
    team: "admin"                   # Team name (required)
    # models: [...arg]              # Override team models (optional)
    # limits:                       # Override team limits (optional)
    #   rpm: 1000
    #   tpm: 500000

  - key: "sk-dev-key"
    team: "developers"
    limits:
      rpm: 1000                    # Override: higher RPM than team default
    # models: inherited from developers team
    # tpm: inherited from developers team (200000)
```

**Inheritance rules**: token inherits from its team. If a field is set on the token, it overrides the team value. If neither is set, default is 0 (unlimited) for limits and `["*"]` for models.

### Models

Define each model with one or more upstream endpoints. The router load-balances across them.

```yaml
models:
  - name: "gpt-4o"                   # Canonical model name
    model_type: chat                 # chat (default) | audio | embedding | completions | rerank
    aliases: ["gpt-4", "gpt-4-turbo", "gpt-4-vision"]
    endpoints:
      - url: "https://api.openai.com"     # Upstream base URL
        key: "sk-openai-key-1"            # Upstream API key
        limits:                           # Per-credential limits (optional)
          rpm: 500
          tpm: 200000
        cost:                             # Cost per 1M tokens (optional)
          prompt: 2.50                    # $ per 1M prompt tokens
          completion: 10.00               # $ per 1M completion tokens
        weight: 1                         # Load balancing weight (default: 1)
        headers:                          # Extra headers (optional)
          x-custom: "value"

      - url: "https://api.openai.com"
        key: "sk-openai-key-2"
        limits:
          rpm: 500
          tpm: 200000
        cost:
          prompt: 2.50
          completion: 10.00
        weight: 2                         # 2x more traffic than weight:1
```

**Cost fields** are in USD per 1 million tokens. These are exposed in response headers (`x-cost-prompt-per-1m`, `x-cost-completion-per-1m`) for downstream billing.

**Weight** controls load distribution. Endpoint with weight 2 gets roughly 2x traffic compared to weight 1. Default weight is 1.

**model_type** maps endpoints to API routes:

| model_type | Routes |
|-----------|--------|
| `chat` (default) | `/v1/chat/completions`, `/v1/messages`, `/v1/models` |
| `audio` | `/v1/audio/transcriptions`, `/v1/audio/translations` |
| `embedding` | `/v1/embeddings` |
| `completions` | `/v1/completions` |
| `rerank` | `/v1/rerank` |

All other `/v1/{*path}` endpoints are handled by the generic proxy — auth + rate-limit + round-robin + forward as-is.

```yaml
# Audio model example — multipart/form-data, model from form field
  - name: "whisper-1"
    model_type: audio
    aliases: ["openai/whisper-large-v3-turbo"]
    endpoints:
      - url: "https://api.openai.com"
        key: "sk-openai-key-2"
```

### Fail2ban

Circuit breaker that temporarily bans failing endpoints.

```yaml
fail2ban:
  max_failures: 5            # Consecutive failures before ban (default: 5)
  ban_duration_secs: 60      # Ban duration in seconds (default: 60)
  error_threshold_pct: 0.5   # Error rate threshold (0.5 = 50%, default: 0.5)
```

Two ban triggers:
1. **Consecutive failures**: N failures in a row → instant ban
2. **Error rate**: >threshold% errors over 10+ requests → ban

### Session Sticky Routing

```yaml
session:
  sticky_ttl_secs: 300       # How long to keep session→endpoint binding (default: 300)
```

When a request includes `x-sticky-session-id` in the body, the router hashes it to pick a fixed endpoint. Subsequent requests with the same session ID go to the same endpoint — maximising prompt cache hits and reducing costs.

TTL resets on each request. After TTL seconds of inactivity, the binding expires and a new endpoint is chosen.

### PostgreSQL Statistics (optional)

```yaml
stats:
  enabled: false                              # Enable PostgreSQL stats (default: false)
  postgres_url: "postgres://user:***@localhost/openai_router"
  retention_days: 30                          # Auto-delete stats older than N days (0 = never)
  cleanup_interval_secs: 3600                 # Cleanup run interval in seconds (0 = disabled)
  aggregation_interval_secs: 300              # Batch aggregation interval (0 = disabled, default 5min)
```

Requires building with the `postgres` feature:

```bash
cargo build --release --features postgres
```

Tables are **auto-created** on startup. Schema:

```sql
-- Raw per-request log
CREATE TABLE requests (
    id                BIGSERIAL PRIMARY KEY,
    timestamp         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    model             VARCHAR(255) NOT NULL,
    endpoint          VARCHAR(512) NOT NULL,
    team              VARCHAR(128) NOT NULL DEFAULT '',
    tokens_prompt     BIGINT NOT NULL DEFAULT 0,
    tokens_completion BIGINT NOT NULL DEFAULT 0,
    latency_ms        BIGINT NOT NULL DEFAULT 0,
    status            SMALLINT NOT NULL DEFAULT 200,
    token_key_hash    VARCHAR(64)
);

-- Hourly aggregation per token (upsert — counters increment)
CREATE TABLE token_usage_hourly (
    hour              TIMESTAMPTZ NOT NULL,
    token_hash        VARCHAR(64) NOT NULL,
    team              VARCHAR(128) NOT NULL DEFAULT '',
    requests          BIGINT NOT NULL DEFAULT 0,
    tokens_prompt     BIGINT NOT NULL DEFAULT 0,
    tokens_completion BIGINT NOT NULL DEFAULT 0,
    cost_approx       DOUBLE PRECISION NOT NULL DEFAULT 0,
    PRIMARY KEY (hour, token_hash)
);

-- Hourly aggregation per team (upsert — counters increment)
CREATE TABLE team_usage_hourly (
    hour              TIMESTAMPTZ NOT NULL,
    team              VARCHAR(128) NOT NULL,
    requests          BIGINT NOT NULL DEFAULT 0,
    tokens_prompt     BIGINT NOT NULL DEFAULT 0,
    tokens_completion BIGINT NOT NULL DEFAULT 0,
    active_tokens     BIGINT NOT NULL DEFAULT 0,
    cost_approx       DOUBLE PRECISION NOT NULL DEFAULT 0,
    PRIMARY KEY (hour, team)
);
```

#### Indexes (auto-created)

```sql
-- Time-range scan (most common query pattern)
CREATE INDEX idx_requests_ts ON requests (timestamp DESC);

-- Lookup by token hash
CREATE INDEX idx_requests_token ON requests (token_key_hash);

-- Lookup by team
CREATE INDEX idx_requests_team ON requests (team);

-- Composite: token + time (fast filter by token in date range)
CREATE INDEX idx_requests_token_ts ON requests (token_key_hash, timestamp DESC);

-- Composite: team + time (fast filter by team in date range)
CREATE INDEX idx_requests_team_ts ON requests (team, timestamp DESC);

-- Composite: model + time
CREATE INDEX idx_requests_model_ts ON requests (model, timestamp DESC);

-- Composite: status + time (fast error lookup)
CREATE INDEX idx_requests_status_ts ON requests (status, timestamp DESC);
```

#### Background tasks

Two background tasks run periodically inside `tokio::spawn`:

| Task | Interval | What it does |
|------|----------|--------------|
| **Aggregation** | `aggregation_interval_secs` (default 300s) | Runs `INSERT INTO ... SELECT ... GROUP BY` from `requests` into `token_usage_hourly` and `team_usage_hourly`. Processes only **completed hours** (excludes current partial hour). Uses `_aggregation_cursor` table to track last-processed timestamp — crash-safe, no double-counting. |
| **Cleanup** | `cleanup_interval_secs` (default 3600s) | `DELETE` rows older than `retention_days` from all three tables. |

First aggregation starts 90s after server start, cleanup at 60s.

`requests` table is **write-only** — `record_request()` does a single fast `INSERT`, no upserts. Hourly tables get populated later by the aggregation task.

#### Useful queries

```sql
-- Token usage today (by cost)
SELECT token_hash, team, requests,
       tokens_prompt + tokens_completion AS total_tokens,
       ROUND(cost_approx::numeric, 4) AS cost_usd
FROM token_usage_hourly
WHERE hour >= date_trunc('day', NOW())
ORDER BY cost_approx DESC;

-- Team usage this month
SELECT team, SUM(requests) AS reqs,
       SUM(tokens_prompt + tokens_completion) AS tokens,
       ROUND(SUM(cost_approx)::numeric, 2) AS cost_usd
FROM team_usage_hourly
WHERE hour >= date_trunc('month', NOW())
GROUP BY team
ORDER BY cost_usd DESC;

-- Top tokens by RPM (requests per minute peak)
SELECT token_hash, team, requests,
       EXTRACT(epoch FROM NOW() - hour) / 60 AS minutes_ago,
       ROUND(requests / GREATEST(EXTRACT(epoch FROM NOW() - hour) / 60, 1)) AS avg_rpm
FROM token_usage_hourly
WHERE hour >= NOW() - INTERVAL '1 hour'
ORDER BY requests DESC
LIMIT 20;
```

## API Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/health` | No | JSON health status of all endpoints |
| `GET` | `/vhealth` | No | HTML health dashboard with auto-refresh |
| `GET` | `/metrics` | No | Prometheus metrics (text format) |
| `GET` | `/api/version` | No | Ollama-compatible version endpoint |
| `GET` | `/api/tags` | Yes | Ollama-compatible model list |
| `GET` | `/stats` | Yes | Live request statistics per token/team/endpoint |
| `GET` | `/v1/models` | Yes | List available models (OpenAI-compatible) |
| `POST` | `/v1/chat/completions` | Yes | Chat completions (proxied) |
| `POST` | `/v1/messages` | Yes | Anthropic Messages API (translated to OpenAI, proxied, translated back) |
| `POST` | `/v1/completions` | Yes | Text completions (proxied) |
| `POST` | `/v1/embeddings` | Yes | Embeddings (proxied) |

### Response Headers

The router adds these headers to proxied responses:

| Header | Description |
|--------|-------------|
| `x-endpoint-used` | URL of the upstream endpoint that served the request |
| `x-endpoint-index` | Index of the endpoint (0-based) within the model's endpoint list |
| `x-latency-ms` | Router-to-upstream latency in milliseconds |
| `x-cost-prompt-per-1m` | Prompt token cost per 1M (if configured) |
| `x-cost-completion-per-1m` | Completion token cost per 1M (if configured) |

### Response Normalization

The router normalizes upstream responses to the strict OpenAI schema, removing non-standard extensions injected by inference engines (vLLM, TGI, etc.). This ensures compatibility with strict clients such as **Hermes agent**, **LangChain**, and **OpenAI Python SDK**.

What is normalized:

| Transformation | Why |
|----------------|-----|
| `model` field replaced with **requested model name** | Upstream may return internal model IDs (`qwencoder`) while the client expects the alias (`coder36`) |
| `choices[].stop_reason` removed | Non-standard vLLM field; breaks Pydantic validators |
| `choices[].token_ids` removed | Non-standard vLLM field |
| `choices[].provider_specific_fields` removed | Non-standard wrapper field |
| `prompt_logprobs`, `prompt_token_ids`, `kv_transfer_params` removed | Non-standard root-level fields |

LiteLLM performs the same sanitization; the router now matches that behavior out of the box.

### Error Responses

All errors are returned in OpenAI-compatible JSON format:

```json
{
  "error": {
    "message": "Invalid API token provided",
    "type": "authentication_error",
    "param": null,
    "code": "invalid_api_token"
  }
}
```

Common error codes:

| HTTP Status | `code` | When |
|-------------|--------|------|
| `401` | `missing_bearer_token` | No `Authorization: Bearer ...` header |
| `401` | `invalid_api_token` | Unknown token |
| `403` | `model_not_allowed` | Token has no access to requested model |
| `429` | `rate_limit_exceeded` | Team or token RPM/TPM limit hit |
| `503` | `all_endpoints_banned` | Every endpoint for the model is banned |
| `503` | `no_available_endpoint` | No healthy endpoint for the model |
| `502` | `upstream_network_error` | Network error from upstream |

### Rate Limit Headers

Standard HTTP 429 is returned when limits are exceeded. Response body indicates the scope (token, team, or endpoint).

## Live Statistics

`GET /stats` returns real-time request statistics. Regular tokens see only their own scope; **admin tokens** (`models: ["*"]`) see aggregated data for **all tokens and teams**.

```bash
curl -H "Authorization: Bearer sk-admin-xxx" http://localhost:8080/stats
```

**Regular token response:**
```json
{
  "token_key": "sk-basic",
  "team": "basic",
  "is_admin": false,
  "token_requests": 42,
  "token_errors": 3,
  "team_requests": 150,
  "team_errors": 10,
  "endpoints": [
    {
      "model": "gpt-4o-mini",
      "url": "https://api.openai.com",
      "requests": 25,
      "errors": 2,
      "banned": false,
      "healthy": true
    }
  ]
}
```

**Admin token response** additionally includes:
```json
{
  "is_admin": true,
  "all_tokens": {
    "sk-admin": { "requests": 100, "errors": 5 },
    "sk-basic": { "requests": 42, "errors": 3 }
  },
  "all_teams": {
    "admin": { "requests": 100, "errors": 5 },
    "basic": { "requests": 42, "errors": 3 }
  }
}
```

## Prometheus Metrics

Available at `GET /metrics`:

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `openai_router_requests_total` | Counter | `model`, `endpoint`, `status` | Total proxied requests |
| `openai_router_request_latency_seconds` | Histogram | `model`, `endpoint` | Request latency distribution |
| `openai_router_banned_endpoints` | Gauge | — | Currently banned endpoint count |
| `openai_router_rate_limit_hits_total` | Counter | `scope`, `key` | Rate limit rejections |
| `openai_router_tokens_consumed_total` | Counter | `model`, `type` | Tokens consumed (prompt/completion) |

## Session Sticky Routing

To use session stickiness, include `x-sticky-session-id` in your request body:

```json
{
  "model": "gpt-4o",
  "messages": [{"role": "user", "content": "Hello"}],
  "x-sticky-session-id": "conversation-abc-123"
}
```

The router hashes the session ID to pick a stable endpoint. This maximises the chance of hitting the upstream provider's prompt cache, reducing costs and latency.

## Model Aliases

Define aliases in the model config to support multiple model names:

```yaml
models:
  - name: "gpt-4o"
    aliases: ["gpt-4", "gpt-4-turbo", "gpt-4-vision"]
```

Requests for `gpt-4`, `gpt-4-turbo`, or `gpt-4-vision` are all routed to the `gpt-4o` endpoints.

## Anthropic API Translation

The router accepts **Anthropic Messages API** requests at `POST /v1/messages` and transparently translates them to OpenAI format, proxies to upstream, and translates the response back.

### Request translation (Anthropic → OpenAI)

| Anthropic field | OpenAI field |
|----------------|-------------|
| `system` (string) | `messages[0]` = `{"role":"system","content":"..."}` |
| `messages[].role` | `messages[].role` (user/assistant — preserved) |
| `messages[].content` | `messages[].content` |
| `max_tokens` | `max_tokens` |
| `temperature` | `temperature` |
| `top_p` | `top_p` |
| `stop_sequences` | `stop` |
| `stream` | `stream` |

### Response translation (OpenAI → Anthropic)

| OpenAI field | Anthropic field |
|-------------|----------------|
| `choices[0].message.content` | `content[0].text` |
| `choices[0].finish_reason` | `stop_reason` (`stop`→`end_turn`, `length`→`max_tokens`) |
| `usage.prompt_tokens` | `usage.input_tokens` |
| `usage.completion_tokens` | `usage.output_tokens` |
| `model` | `model` |

### Example

```bash
curl -X POST http://localhost:8080/v1/messages \
  -H "Authorization: Bearer sk-admin-xxx" \
  -H "Content-Type: application/json" \
  -H "anthropic-version: 2023-06-01" \
  -d '{
    "model": "deepseek-chat",
    "system": "Reply in Russian",
    "messages": [{"role": "user", "content": "Say hello"}],
    "max_tokens": 50
  }'
```

Response (Anthropic format):
```json
{
  "id": "msg_...",
  "type": "message",
  "role": "assistant",
  "model": "deepseek-v4-flash",
  "stop_reason": "end_turn",
  "content": [{"type": "text", "text": "Здравствуйте"}],
  "usage": {"input_tokens": 13, "output_tokens": 1}
}
```

## Ollama Compatibility

The router exposes a subset of the Ollama API so that Ollama-native clients (e.g. OpenWebUI) can discover and use models without explicit OpenAI configuration.

| Ollama Endpoint | Status | Notes |
|-----------------|--------|-------|
| `GET /api/tags` | ✅ Supported | Returns model list in Ollama format (requires Bearer token) |
| `GET /api/version` | ✅ Supported | Returns router version (no auth required) |
| `POST /api/generate` | ❌ Not supported | Use `/v1/chat/completions` instead |
| `POST /api/chat` | ❌ Not supported | Use `/v1/chat/completions` instead |

### Example

```bash
curl http://localhost:8080/api/tags \
  -H "Authorization: Bearer sk-admin-xxx"
```

Response:
```json
{
  "models": [
    {
      "name": "gpt-4o",
      "model": "gpt-4o",
      "modified_at": "2024-01-01T00:00:00Z",
      "size": 0,
      "digest": "openai-router",
      "details": {
        "family": "openai",
        "format": "gguf",
        "families": [],
        "parameter_size": "",
        "quantization_level": ""
      }
    }
  ]
}
```

## Token Inheritance Example

```yaml
teams:
  - name: "basic"
    models: ["gpt-4o-mini"]
    limits: { rpm: 100, tpm: 50000 }

tokens:
  # Full inheritance — all settings from "basic" team
  - key: "sk-user-1"
    team: "basic"

  # Override RPM only — models and TPM from team
  - key: "sk-power-user"
    team: "basic"
    limits: { rpm: 500 }    # tpm = 50000 from team
```

## Multi-Instance Sync (Redis/Valkey)

When running multiple instances behind a load balancer, enable Redis/Valkey sync to share state:

- **Rate limits** — counters in Redis (`INCR` + `EXPIRE`), shared across all instances
- **Fail2ban** — ban/unban via `SETEX` + Pub/Sub, all instances see bans instantly
- **Sticky sessions** — stored in Redis with TTL, survives instance restart

### Configuration

#### Standalone Redis/Valkey

```yaml
sync:
  enabled: true
  mode: "standalone"
  redis_url: "redis://valkey:6379/0"
  key_prefix: "oar"
  fail_open: true               # If Redis is down: true=allow without limits, false=reject
```

#### Redis Sentinel (HA)

```yaml
sync:
  enabled: true
  mode: "sentinel"
  sentinel_nodes:
    - "redis://sentinel-1:26379"
    - "redis://sentinel-2:26379"
    - "redis://sentinel-3:26379"
  sentinel_master_name: "mymaster"
  sentinel_server_type: "master"   # "master" or "replica"
  key_prefix: "oar"
  fail_open: true
```

`fail_open` determines behavior when Redis/Valkey is unreachable:
- `true` (default) — requests pass without shared rate limits
- `false` — requests are rejected until Redis recovers

```bash
cargo build --release --features redis-sync
```

### Architecture

```
                   ┌──────────────┐
                   │    nginx     │  (load balancer)
                   └──┬───┬───┬──┘
                      │   │   │
              ┌───────┘   │   └───────┐
              ▼           ▼           ▼
         ┌─────────┐ ┌─────────┐ ┌─────────┐
         │router #1│ │router #2│ │router #3│
         └────┬────┘ └────┬────┘ └────┬────┘
              │           │           │
              └───────────┼───────────┘
                          │
              ┌───────────▼───────────┐
              │    Redis / Valkey    │
              │  (rate limits, bans, │
              │   sticky sessions)   │
              └──────────────────────┘
```

### Docker Compose

```bash
# Start 3 router instances + Valkey + nginx
docker compose -f docker/docker-compose.yml up -d --scale router=3

# With PostgreSQL stats
docker compose -f docker/docker-compose.yml --profile stats up -d --scale router=3
```

Full `docker-compose.yml`, `Dockerfile` and `nginx.conf` are in the `docker/` directory.

## Reverse Proxy Deployment

For production, run openai-router behind nginx or Caddy for TLS termination, client timeouts, compression, and sub-path routing.

### nginx

```nginx
# /etc/nginx/sites-available/openai-router
upstream openai_router {
    server 127.0.0.1:8080;
    keepalive 32;
}

server {
    listen 443 ssl http2;
    server_name api.example.com;

    ssl_certificate     /etc/letsencrypt/live/api.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/api.example.com/privkey.pem;

    # Client timeouts
    client_body_timeout     30s;
    client_header_timeout   10s;
    keepalive_timeout       60s;
    send_timeout            60s;

    # Upstream timeouts
    proxy_connect_timeout   10s;
    proxy_read_timeout     300s;   # Long timeout for streaming!
    proxy_send_timeout      60s;

    gzip on;
    gzip_types application/json text/event-stream;

    # Sub-path: /api/* → openai-router (strip /api prefix)
    location /api/ {
        proxy_pass http://openai_router/;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_buffering off;    # Required for SSE streaming
        proxy_cache off;
    }

    # Direct proxy (no sub-path)
    # location / {
    #     proxy_pass http://openai_router;
    #     proxy_http_version 1.1;
    #     proxy_set_header Connection "";
    #     proxy_set_header Host $host;
    #     proxy_buffering off;
    # }
}

server {
    listen 80;
    server_name api.example.com;
    return 301 https://$host$request_uri;
}
```

### Caddy

```caddy
# /etc/caddy/Caddyfile (or ./Caddyfile)
api.example.com {
    servers {
        timeouts {
            read_body   30s
            read_header 10s
            write       60s
            idle        60s
        }
    }

    # Sub-path: /api/* → openai-router (strip /api prefix)
    handle_path /api/* {
        reverse_proxy localhost:8080 {
            header_up Host {host}
            header_up X-Real-IP {remote_host}
            header_up X-Forwarded-For {remote_host}
            header_up X-Forwarded-Proto {scheme}

            transport http {
                dial_timeout            10s
                response_header_timeout 300s
                write_timeout           60s
            }

            flush_interval -1     # Required for SSE streaming
        }
    }

    # Direct proxy (no sub-path)
    # reverse_proxy localhost:8080
}
```

> **Caddy automatically obtains TLS certificates** from Let's Encrypt. Just replace `api.example.com` with your actual domain. For local development use `tls internal`.

## Logging & Debugging

The router uses `tracing` for structured logging. Set the log level via the `RUST_LOG` environment variable:

```bash
RUST_LOG=debug ./openai-router config.yaml
```

**What gets logged:**

| Level | What is logged |
|-------|----------------|
| `INFO` | Startup, config reload, requests (method, path, token, latency) |
| `WARN` | Invalid tokens, forbidden model access, upstream errors, endpoint bans |
| `DEBUG` | Missing/malformed Bearer tokens, rate-limit hits, endpoint selection, retries |
| `ERROR` | Upstream network failures, PostgreSQL connection issues |

All rejected requests (invalid token, rate limit, missing auth) are logged with enough detail to debug access issues without exposing full keys (tokens are masked as `sk-ab...cd`).

## Building from Source

Requirements:
- Rust 1.80+
- Optional: PostgreSQL 14+ (for stats feature)

```bash
# Clone
git clone https://github.com/your-org/openai-router
cd openai-router

# Build (debug)
cargo build

# Build (release, optimized)
cargo build --release

# Build with PostgreSQL support
cargo build --release --features postgres

# Run
./target/release/openai-router config.yaml
```

## Kubernetes (Helm)

```bash
helm install openai-router ./k8s \
  --set config.tokens[0].key=sk-your-key \
  --set config.models[0].endpoints[0].key=sk-upstream-key
```

Chart includes:
- Router deployment (3 replicas by default)
- Valkey sidecar (sync)
- PostgreSQL sidecar (stats, optional)
- ConfigMap for hot-reloadable `config.yaml`
- Ingress template (optional)

## Systemd

```bash
sudo cp systemd/openai-router.service /etc/systemd/system/
sudo useradd -r -s /bin/false openai-router
sudo mkdir -p /opt/openai-router
sudo cp target/release/openai-router /opt/openai-router/
sudo cp config.yaml /opt/openai-router/
sudo chown -R openai-router:openai-router /opt/openai-router
sudo systemctl daemon-reload
sudo systemctl enable --now openai-router
```

## Tests

```bash
cargo test                          # all tests
cargo test router                   # balancer tests
cargo test fail2ban                 # fail2ban tests
cargo test --no-default-features    # test without pg/redis
```

## Project Structure

```
src/
├── main.rs                 # Entry point, server setup
├── config/
│   ├── types.rs            # Configuration structs (YAML mapping)
│   ├── loader.rs           # YAML parser + validation
│   └── watcher.rs          # Hot-reload via inotify
├── auth/
│   ├── middleware.rs       # Bearer token authentication
│   └── timeout.rs          # Request timeout middleware
├── router/
│   ├── balancer.rs         # Weighted round-robin + banned-skip + retry
│   └── sticky.rs           # Session-sticky routing store
├── proxy/
│   ├── handler.rs          # Request proxy + retry loop + fail2ban
│   ├── sse.rs              # SSE streaming passthrough
│   └── anthropic.rs        # Anthropic ↔ OpenAI translation
├── ratelimit/
│   └── limiter.rs          # Token bucket RPM/TPM limiter
├── fail2ban/
│   └── breaker.rs          # Circuit breaker + per-endpoint ban
├── metrics/
│   └── prometheus.rs       # Prometheus metric definitions
├── health/
│   ├── dashboard.rs        # /health JSON + /vhealth HTML + /stats live statistics
│   └── checker.rs          # Background endpoint health probes
├── stats/
│   └── pg.rs               # PostgreSQL: requests + token/team hourly aggregation
├── sync/
│   ├── mod.rs              # SyncStore (no-op or Redis-backed)
│   └── redis_sync.rs       # Redis/Valkey: rate limits, bans, sticky sessions
└── utils.rs                # Shared helpers (mask_key, mask_url, json_error)
```

## License

MIT
