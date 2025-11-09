use anyhow::Result;
use clap::Parser;
use rust_healthcheck::{Config, load_config, run_healthchecks, run_watch};
use schemars::schema_for;
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "rust-healthcheck",
    version,
    about = "Concurrent HTTP healthchecker with file-based config"
)]
struct Cli {
    /// Path to config file (json|yaml). Falls back to $CONFIG_PATH or ./config/config.json
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Print JSON schema for the config and exit
    #[arg(long)]
    print_schema: bool,
}

fn init_logging(cfg: &Config) {
    let env_filter = if let Some(level) = &cfg.log_level {
        EnvFilter::new(level)
    } else if let Ok(level) = std::env::var("RUST_LOG") {
        EnvFilter::new(level)
    } else {
        EnvFilter::new("info")
    };
    if cfg.json_logging {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .json()
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("failed to set global subscriber");
    } else {
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("failed to set global subscriber");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli
        .config
        .or_else(|| std::env::var_os("CONFIG_PATH").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("./config/config.json"));
    if cli.print_schema {
        let schema = schema_for!(Config);
        println!("{}", serde_json::to_string_pretty(&schema)?);
        return Ok(());
    }
    let mut cfg: Config = load_config(&config_path)?;
    // Basic env overrides
    if let Ok(v) = std::env::var("CONCURRENCY")
        && let Ok(n) = v.parse::<usize>()
    {
        cfg.concurrency = n;
    }
    if let Ok(v) = std::env::var("REQUEST_TIMEOUT_MS")
        && let Ok(n) = v.parse::<u64>()
    {
        cfg.request_timeout_ms = n;
    }
    if let Ok(v) = std::env::var("RETRIES")
        && let Ok(n) = v.parse::<u32>()
    {
        cfg.retries = n;
    }
    init_logging(&cfg);

    info!(?config_path, "loaded configuration");
    if cfg.watch_interval_sec.unwrap_or(0) > 0 {
        run_watch(&cfg).await?;
        Ok(())
    } else {
        let summary = run_healthchecks(&cfg).await?;
        if cfg.summary_json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "total": summary.total,
                    "up": summary.up,
                    "down": summary.down
                }))?
            );
        }
        if summary.down > 0 {
            std::process::exit(1);
        }
        Ok(())
    }
}
