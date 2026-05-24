# ── Build stage ─────────────────────────────────────────────────────────
FROM rust:1.95-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release

# ── Runtime stage ──────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/openai-router /app/openai-router
COPY docker/config_docker.yaml /app/config.example.yaml

EXPOSE 8080

ENV OPENAI_ROUTER_CONFIG=/app/config.yaml

ENTRYPOINT ["/app/openai-router"]
CMD ["/app/config.yaml"]
