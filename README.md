## rust-healthcheck

Concurrent HTTP healthcheck worker written in Rust.

- Reads all configuration from a file (JSON or YAML). Example at `config/example.config.json`.
- Logs structured output using `tracing`.
- Collects basic metrics internally and logs a final summary. No metrics web server is exposed.
- Ships with multi-stage Dockerfile running as non-root.
- Helm chart for Kubernetes with ConfigMap-mounted config.
- Kustomize overlays for `dev`, `stg`, and `prd` rendering the Helm chart with environment-specific values.
- GitHub Actions CI for fmt, clippy, tests, and Docker build.

### Config file

Minimal JSON example:

```json
{
  "endpoints_to_check": ["https://www.rust-lang.org","https://httpstat.us/503"],
  "request_timeout_ms": 5000,
  "concurrency": 8,
  "retries": 0,
  "user_agent": "rust-healthcheck/1.0",
  "log_level": "info",
  "metrics_log_interval_sec": null
}
```

- `endpoints_to_check`: array of URLs to probe.
- `request_timeout_ms`: per-request timeout.
- `concurrency`: max in-flight checks.
- `retries`: number of retries per endpoint.
- `user_agent`: User-Agent header for outgoing requests.
- `log_level`: `trace|debug|info|warn|error`.

YAML is also supported (use `.yml`/`.yaml` extension).

### Running locally

```bash
cargo run -- --config ./config/example.config.json
```

### Tests and lints

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

### Docker

Build and run:

```bash
docker build -t rust-healthcheck:local .
docker run --rm -e RUST_LOG=info rust-healthcheck:local
```

Provide a custom config:

```bash
docker run --rm -v $(pwd)/config:/app/config rust-healthcheck:local ./rust-healthcheck --config /app/config/config.json
```

### Helm

Update `chart/values.yaml` or provide your own values.

```bash
helm install rh ./chart --namespace default --create-namespace
helm upgrade rh ./chart -f your-values.yaml
helm uninstall rh
```

The config is sourced from a ConfigMap named `<release>-rust-healthcheck-config` and mounted at `/app/config/config.json`.

### Kustomize (dev/stg/prd)

Each environment renders the Helm chart with its own `values.yaml`:

```bash
kustomize build deploy/dev
kustomize build deploy/stg
kustomize build deploy/prd
```

### GitHub Actions CI

Workflow at `.github/workflows/ci.yml` runs:
- `cargo fmt --check`
- `cargo clippy -D warnings`
- `cargo test`
- docker build (no push)


