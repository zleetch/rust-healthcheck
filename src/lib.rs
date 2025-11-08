use anyhow::{Context, Result};
use futures::{StreamExt, stream};
use metrics::{counter, histogram};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// List of HTTP/HTTPS endpoints to check
    pub endpoints_to_check: Vec<String>,
    /// Request timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub request_timeout_ms: u64,
    /// Maximum number of concurrent checks
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// Number of retries for each endpoint (0 = no retry)
    #[serde(default)]
    pub retries: u32,
    /// User-Agent header for outbound requests
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    /// Optional log level (e.g., INFO, DEBUG). If unset, use env var RUST_LOG or default.
    #[serde(default)]
    pub log_level: Option<String>,
    /// If set, periodically logs metrics (seconds). For one-shot runs, a final summary is always logged.
    #[serde(default)]
    pub metrics_log_interval_sec: Option<u64>,
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
    let client = Client::builder()
        .user_agent(&cfg.user_agent)
        .timeout(Duration::from_millis(cfg.request_timeout_ms))
        .build()
        .context("failed to build reqwest client")?;
    Ok(client)
}

pub async fn check_endpoint_once(client: &Client, endpoint: &str) -> CheckOutcome {
    let start = Instant::now();
    match client.get(endpoint).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                let latency = start.elapsed().as_millis();
                histogram!("healthcheck_latency_ms").record(latency as f64);
                counter!("healthcheck_up_total").increment(1);
                CheckOutcome {
                    endpoint: endpoint.to_string(),
                    status: HealthStatus::Up,
                    latency_ms: Some(latency),
                    attempts: 1,
                    last_http_status: Some(status),
                }
            } else {
                counter!("healthcheck_down_total").increment(1);
                CheckOutcome {
                    endpoint: endpoint.to_string(),
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
                endpoint: endpoint.to_string(),
                status: HealthStatus::Down(e.to_string()),
                latency_ms: None,
                attempts: 1,
                last_http_status: None,
            }
        }
    }
}

pub async fn check_with_retries(client: &Client, endpoint: &str, retries: u32) -> CheckOutcome {
    let mut attempt: u32 = 0;
    let mut last_outcome = check_endpoint_once(client, endpoint).await;
    last_outcome.attempts = 1;
    while attempt < retries {
        match last_outcome.status {
            HealthStatus::Up => break,
            HealthStatus::Down(_) => {
                attempt += 1;
                warn!(
                    endpoint = endpoint,
                    attempt = attempt,
                    "retrying failed endpoint"
                );
                let outcome = check_endpoint_once(client, endpoint).await;
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
    if cfg.endpoints_to_check.is_empty() {
        warn!("no endpoints configured");
        return Ok(Summary::default());
    }
    let client = build_client(cfg)?;
    let semaphore = Arc::new(Semaphore::new(cfg.concurrency));

    info!(
        total = cfg.endpoints_to_check.len(),
        concurrency = cfg.concurrency,
        timeout_ms = cfg.request_timeout_ms,
        retries = cfg.retries,
        "starting healthchecks"
    );

    let outcomes = stream::iter(cfg.endpoints_to_check.clone())
        .map(|endpoint| {
            let client = client.clone();
            let sem = Arc::clone(&semaphore);
            let retries = cfg.retries;
            async move {
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                debug!(endpoint = %endpoint, "checking endpoint");
                let outcome = check_with_retries(&client, &endpoint, retries).await;
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
