# ── Build stage ─────────────────────────────────────────────────────────
FROM rust:1.95-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

# --mount=type=cache сохраняет cargo registry между сборками (быстро после первого раза)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release && \
    cp target/release/openai-router /app/openai-router

# ── Runtime stage ──────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/openai-router /app/openai-router
COPY docker/config_docker.yaml /app/config.example.yaml

EXPOSE 8080

ENV OPENAI_ROUTER_CONFIG=/app/config.yaml

ENTRYPOINT ["/app/openai-router"]
CMD ["/app/config.yaml"]
