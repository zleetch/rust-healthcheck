# ---- Builder stage ----
FROM rust:1.91-bookworm AS builder
WORKDIR /src
# Create a dummy project to leverage Docker layer caching for dependencies
RUN USER=root cargo new --bin app
WORKDIR /src/app
COPY Cargo.toml Cargo.toml
COPY src ./src
COPY config ./config
RUN cargo build --release

# ---- Runtime stage ----
FROM debian:bookworm-slim AS runtime
ENV RUST_LOG=info \
    APP_HOME=/app
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* && update-ca-certificates
RUN useradd -u 10001 -r -s /sbin/nologin appuser && mkdir -p "${APP_HOME}/config"
WORKDIR ${APP_HOME}
COPY --from=builder /src/app/target/release/rust-healthcheck ${APP_HOME}/rust-healthcheck
COPY --from=builder /src/app/config/example.config.json ${APP_HOME}/config/config.json
USER 10001:10001
ENTRYPOINT ["./rust-healthcheck","--config","/app/config/config.json"]

