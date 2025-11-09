## rust-healthcheck

Concurrent HTTP healthcheck worker written in Rust.

- Reads all configuration from a file (JSON or YAML). Example at `config/example.config.json`.
- Logs structured output using `tracing`.
- Collects basic metrics internally and logs a final summary. No metrics web server is exposed.
- Ships with multi-stage Dockerfile running as non-root.
- Helm chart for Kubernetes with ConfigMap-mounted config (CronJob-focused).
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

- `endpoints_to_check`: array of URLs to probe (basic mode).
- `request_timeout_ms`: per-request timeout.
- `concurrency`: max in-flight checks.
- `retries`: number of retries per endpoint.
- `user_agent`: User-Agent header for outgoing requests.
- `log_level`: `trace|debug|info|warn|error`.
- `json_logging`: output logs in JSON format if `true`.
- `summary_json`: also print summary as JSON.
- `watch_interval_sec`: run continuously with this interval (seconds).
- `metrics_log_interval_sec`: in watch mode, log periodic summaries.
- TLS: `danger_accept_invalid_certs`, `ca_bundle_path` (PEM).

Advanced endpoints (override `endpoints_to_check`):

```json
{
  "endpoints": [
    {
      "url": "https://example.com/health",
      "method": "GET",
      "timeout_ms": 3000,
      "retries": 2,
      "expected_status": { "min": 200, "max": 399 },
      "headers": { "X-Health": "check" }
    }
  ],
  "concurrency": 8,
  "base_backoff_ms": 200,
  "max_backoff_ms": 5000,
  "cb_failures_threshold": 3,
  "cb_cooldown_sec": 60
}
```

YAML is also supported (use `.yml`/`.yaml` extension).

### Running locally

```bash
cargo run -- --config ./config/example.config.json
# print config schema
cargo run -- --print-schema | jq .
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

### Helm (CronJob)

Update `chart/values.yaml` or provide your own values.

```bash
helm install rh ./chart --namespace default --create-namespace
helm upgrade rh ./chart -f your-values.yaml
helm uninstall rh
```

The config is sourced from a ConfigMap named `<release>-rust-healthcheck-config` and mounted at `/app/config/config.json`.

CronJob mode:
- Set `values.cron.enabled=true` and `values.cron.schedule` (e.g. "*/5 * * * *").

NetworkPolicy:
- Enabled via `values.networkPolicy.enabled`. Defaults allow egress 80/443 only.

### Kustomize (dev)

Render the Helm chart via Kustomize for the `dev` environment (CronJob):

```bash
kustomize build deploy/dev
```

### GitHub Actions CI

Workflow at `.github/workflows/ci.yml` runs:
- `cargo fmt --check`
- `cargo clippy -D warnings`
- `cargo test`
- docker build (no push)

Release pipeline:
- Release Please opens a release PR; on tag push or release publish, the `Publish Image` workflow builds, signs (Cosign), scans (Grype), and generates SBOM (Syft), then pushes `ghcr.io/<owner>/rust-healthcheck` with semver and `latest` tags.
