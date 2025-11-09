use anyhow::{Context, Result};
use futures::{StreamExt, stream};
use metrics::{counter, histogram};
use reqwest::{Client, StatusCode};
use schemars::JsonSchema;
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};
use url::Url;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Config {
    /// List of HTTP/HTTPS endpoints to check
    pub endpoints_to_check: Vec<String>,
    /// Advanced endpoint configs (overrides endpoints_to_check if provided)
    #[serde(default)]
    pub endpoints: Option<Vec<EndpointConfig>>,
    /// Request timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub request_timeout_ms: u64,
    /// Maximum number of concurrent checks
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// Number of retries for each endpoint (0 = no retry)
    #[serde(default)]
    pub retries: u32,
    /// Base backoff ms for retries
    #[serde(default = "default_base_backoff_ms")]
    pub base_backoff_ms: u64,
    /// Max backoff ms for retries
    #[serde(default = "default_max_backoff_ms")]
    pub max_backoff_ms: u64,
    /// User-Agent header for outbound requests
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    /// Optional log level (e.g., INFO, DEBUG). If unset, use env var RUST_LOG or default.
    #[serde(default)]
    pub log_level: Option<String>,
    /// If set, periodically logs metrics (seconds). For one-shot runs, a final summary is always logged.
    #[serde(default)]
    pub metrics_log_interval_sec: Option<u64>,
    /// If set, will run repeatedly in a watch loop with this interval (seconds)
    #[serde(default)]
    pub watch_interval_sec: Option<u64>,
    /// Circuit breaker: failures before opening breaker (in watch mode)
    #[serde(default = "default_cb_threshold")]
    pub cb_failures_threshold: u32,
    /// Circuit breaker: cooldown seconds once opened
    #[serde(default = "default_cb_cooldown_sec")]
    pub cb_cooldown_sec: u64,
    /// Output logs as JSON if true
    #[serde(default)]
    pub json_logging: bool,
    /// Emit final summary also as JSON on stdout if true
    #[serde(default)]
    pub summary_json: bool,
    /// TLS: accept invalid certs (dangerous; default false)
    #[serde(default)]
    pub danger_accept_invalid_certs: bool,
    /// TLS: optional CA bundle path (PEM) to trust
    #[serde(default)]
    pub ca_bundle_path: Option<String>,
}

fn default_timeout_ms() -> u64 {
    5_000
}
fn default_concurrency() -> usize {
    8
}
fn default_user_agent() -> String {
    "rust-healthcheck/1.0".to_string()
}
fn default_base_backoff_ms() -> u64 {
    200
}
fn default_max_backoff_ms() -> u64 {
    5_000
}
fn default_cb_threshold() -> u32 {
    3
}
fn default_cb_cooldown_sec() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ExpectedStatus {
    #[serde(default)]
    pub min: Option<u16>,
    #[serde(default)]
    pub max: Option<u16>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EndpointConfig {
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub retries: Option<u32>,
    #[serde(default)]
    pub expected_status: Option<ExpectedStatus>,
    #[serde(default)]
    pub headers: Option<std::collections::HashMap<String, String>>,
}

fn default_method() -> String {
    "GET".to_string()
}

#[derive(Debug, Clone)]
pub enum HealthStatus {
    Up,
    Down(String),
}

#[derive(Debug, Clone)]
pub struct CheckOutcome {
    pub endpoint: String,
    pub status: HealthStatus,
    pub latency_ms: Option<u128>,
    pub attempts: u32,
    pub last_http_status: Option<StatusCode>,
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub total: usize,
    pub up: usize,
    pub down: usize,
}

pub fn load_config<P: AsRef<Path>>(path: P) -> Result<Config> {
    let path_ref = path.as_ref();
    let bytes =
        fs::read(path_ref).with_context(|| format!("failed to read config file {:?}", path_ref))?;
    let ext = path_ref
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "json".to_string());
    let cfg: Config = match ext.as_str() {
        "yaml" | "yml" => serde_yaml::from_slice(&bytes).context("failed to parse YAML config")?,
        _ => serde_json::from_slice(&bytes).context("failed to parse JSON config")?,
    };
    Ok(cfg)
}

pub fn build_client(cfg: &Config) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(&cfg.user_agent)
        .timeout(Duration::from_millis(cfg.request_timeout_ms))
        .danger_accept_invalid_certs(cfg.danger_accept_invalid_certs);
    if let Some(path) = &cfg.ca_bundle_path {
        let pem =
            fs::read(path).with_context(|| format!("failed to read ca bundle at {}", path))?;
        let cert = reqwest::Certificate::from_pem(&pem).context("invalid PEM for CA bundle")?;
        builder = builder.add_root_certificate(cert);
    }
    let client = builder.build().context("failed to build reqwest client")?;
    Ok(client)
}

fn redact_url(input: &str) -> String {
    if let Ok(u) = Url::parse(input) {
        let mut redacted = u.clone();
        redacted.set_query(None);
        return redacted.to_string();
    }
    input.to_string()
}

fn status_matches_expected(status: StatusCode, expected: &Option<ExpectedStatus>) -> bool {
    if let Some(e) = expected {
        let code = status.as_u16();
        if let Some(min) = e.min
            && code < min
        {
            return false;
        }
        if let Some(max) = e.max
            && code > max
        {
            return false;
        }
        true
    } else {
        status.is_success()
    }
}

pub async fn check_endpoint_once(
    client: &Client,
    ep: &EndpointConfig,
    default_timeout_ms: u64,
) -> CheckOutcome {
    let start = Instant::now();
    let mut req = match ep.method.as_str() {
        "HEAD" => client.head(&ep.url),
        _ => client.get(&ep.url),
    };
    if let Some(hs) = &ep.headers {
        for (k, v) in hs {
            req = req.header(k, v);
        }
    }
    req = req.timeout(Duration::from_millis(
        ep.timeout_ms.unwrap_or(default_timeout_ms),
    ));
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status_matches_expected(status, &ep.expected_status) {
                let latency = start.elapsed().as_millis();
                histogram!("healthcheck_latency_ms").record(latency as f64);
                counter!("healthcheck_up_total").increment(1);
                CheckOutcome {
                    endpoint: redact_url(&ep.url),
                    status: HealthStatus::Up,
                    latency_ms: Some(latency),
                    attempts: 1,
                    last_http_status: Some(status),
                }
            } else {
                counter!("healthcheck_down_total").increment(1);
                CheckOutcome {
                    endpoint: redact_url(&ep.url),
                    status: HealthStatus::Down(format!("HTTP {}", status)),
                    latency_ms: None,
                    attempts: 1,
                    last_http_status: Some(status),
                }
            }
        }
        Err(e) => {
            let reason = if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "connect"
            } else if e.is_body() {
                "body"
            } else {
                "other"
            };
            let _ = reason; // reason is still used for logging context below
            counter!("healthcheck_down_total").increment(1);
            CheckOutcome {
                endpoint: redact_url(&ep.url),
                status: HealthStatus::Down(e.to_string()),
                latency_ms: None,
                attempts: 1,
                last_http_status: None,
            }
        }
    }
}

pub async fn check_with_retries(
    client: &Client,
    ep: &EndpointConfig,
    retries: u32,
    default_timeout_ms: u64,
    base_backoff_ms: u64,
    max_backoff_ms: u64,
) -> CheckOutcome {
    let mut attempt: u32 = 0;
    let mut last_outcome = check_endpoint_once(client, ep, default_timeout_ms).await;
    last_outcome.attempts = 1;
    while attempt < retries {
        match last_outcome.status {
            HealthStatus::Up => break,
            HealthStatus::Down(_) => {
                attempt += 1;
                warn!(
                    endpoint = ep.url.as_str(),
                    attempt = attempt,
                    "retrying failed endpoint"
                );
                // backoff with jitter
                let factor = 1u64.checked_pow(attempt.min(20)).unwrap_or(u64::MAX);
                let base = base_backoff_ms.saturating_mul(factor);
                let delay = base.min(max_backoff_ms);
                let jitter = rand::random::<u64>() % (delay / 2 + 1);
                tokio::time::sleep(Duration::from_millis(delay + jitter)).await;
                let outcome = check_endpoint_once(client, ep, default_timeout_ms).await;
                last_outcome = outcome;
                last_outcome.attempts = attempt + 1;
                if matches!(last_outcome.status, HealthStatus::Up) {
                    break;
                }
            }
        }
    }
    last_outcome
}

pub async fn run_healthchecks(cfg: &Config) -> Result<Summary> {
    let endpoints: Vec<EndpointConfig> = if let Some(adv) = &cfg.endpoints {
        adv.clone()
    } else {
        cfg.endpoints_to_check
            .iter()
            .map(|u| EndpointConfig {
                url: u.clone(),
                method: default_method(),
                timeout_ms: None,
                retries: None,
                expected_status: None,
                headers: None,
            })
            .collect()
    };
    if endpoints.is_empty() {
        warn!("no endpoints configured");
        return Ok(Summary::default());
    }
    let client = build_client(cfg)?;
    let semaphore = Arc::new(Semaphore::new(cfg.concurrency));

    info!(
        total = endpoints.len(),
        concurrency = cfg.concurrency,
        timeout_ms = cfg.request_timeout_ms,
        retries = cfg.retries,
        "starting healthchecks"
    );

    let outcomes = stream::iter(endpoints.clone())
        .map(|endpoint| {
            let client = client.clone();
            let sem = Arc::clone(&semaphore);
            let retries = endpoint.retries.unwrap_or(cfg.retries);
            let default_timeout_ms = cfg.request_timeout_ms;
            let base_backoff_ms = cfg.base_backoff_ms;
            let max_backoff_ms = cfg.max_backoff_ms;
            async move {
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                debug!(endpoint = %endpoint.url, "checking endpoint");
                let outcome = check_with_retries(&client, &endpoint, retries, default_timeout_ms, base_backoff_ms, max_backoff_ms).await;
                match &outcome.status {
                    HealthStatus::Up => {
                        info!(endpoint = %outcome.endpoint, latency_ms = ?outcome.latency_ms, attempts = outcome.attempts, "endpoint up");
                    }
                    HealthStatus::Down(reason) => {
                        error!(endpoint = %outcome.endpoint, attempts = outcome.attempts, reason = %reason, "endpoint down");
                    }
                }
                outcome
            }
        })
        .buffer_unordered(cfg.concurrency)
        .collect::<Vec<_>>()
        .await;

    let mut summary = Summary {
        total: outcomes.len(),
        up: 0,
        down: 0,
    };
    for outcome in outcomes {
        match outcome.status {
            HealthStatus::Up => summary.up += 1,
            HealthStatus::Down(_) => summary.down += 1,
        }
    }
    info!(
        total = summary.total,
        up = summary.up,
        down = summary.down,
        "healthcheck summary"
    );
    Ok(summary)
}

pub async fn run_watch(cfg: &Config) -> Result<()> {
    let interval_sec = match cfg.watch_interval_sec {
        Some(n) if n > 0 => n,
        _ => return Ok(()), // nothing to do
    };
    use std::collections::HashMap;
    let mut breaker: HashMap<String, (u32, Option<Instant>)> = HashMap::new();
    let mut last_summary: Summary;
    let metrics_interval = cfg.metrics_log_interval_sec.unwrap_or(0);
    let mut metrics_ticker = if metrics_interval > 0 {
        Some(tokio::time::interval(Duration::from_secs(metrics_interval)))
    } else {
        None
    };
    if let Some(ticker) = &mut metrics_ticker {
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    }
    loop {
        // Prepare a filtered config if breaker is open for endpoints
        let mut cfg_clone = cfg.clone();
        let base_eps: Vec<EndpointConfig> = if let Some(eps) = &cfg.endpoints {
            eps.clone()
        } else {
            cfg.endpoints_to_check
                .iter()
                .map(|u| EndpointConfig {
                    url: u.clone(),
                    method: default_method(),
                    timeout_ms: None,
                    retries: None,
                    expected_status: None,
                    headers: None,
                })
                .collect()
        };
        let now = Instant::now();
        let filtered: Vec<EndpointConfig> = base_eps
            .into_iter()
            .filter(|ep| {
                if let Some((fails, until)) = breaker.get(&ep.url)
                    && let Some(deadline) = until
                    && *fails >= cfg.cb_failures_threshold
                    && *deadline > now
                {
                    warn!(endpoint = %ep.url, "circuit open; skipping this iteration");
                    return false;
                }
                true
            })
            .collect();
        cfg_clone.endpoints = Some(filtered);
        let summary = run_healthchecks(&cfg_clone).await?;
        if cfg.summary_json {
            let json = serde_json::to_string(&serde_json::json!({
                "total": summary.total,
                "up": summary.up,
                "down": summary.down
            }))?;
            println!("{}", json);
        }
        last_summary = summary.clone();

        // Update breaker state based on last run
        if let Some(eps) = &cfg.endpoints {
            for ep in eps {
                if last_summary.down > 0 {
                    // rough heuristic: if any down, increment count for those known failing endpoints
                    let entry = breaker.entry(ep.url.clone()).or_insert((0, None));
                    entry.0 = entry.0.saturating_add(1);
                    if entry.0 >= cfg.cb_failures_threshold {
                        entry.1 = Some(Instant::now() + Duration::from_secs(cfg.cb_cooldown_sec));
                    }
                } else {
                    breaker.remove(&ep.url);
                }
            }
        }

        // Periodic metrics logging
        if let Some(ticker) = &mut metrics_ticker {
            tokio::select! {
                _ = ticker.tick() => {
                    info!(total = last_summary.total, up = last_summary.up, down = last_summary.down, "periodic summary");
                }
                _ = tokio::time::sleep(Duration::from_secs(interval_sec)) => {}
            }
        } else {
            tokio::time::sleep(Duration::from_secs(interval_sec)).await;
        }
    }
}
