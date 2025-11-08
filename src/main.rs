use anyhow::Result;
use clap::Parser;
use rust_healthcheck::{Config, load_config, run_healthchecks};
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
}

fn init_logging(cfg: &Config) {
    let env_filter = if let Some(level) = &cfg.log_level {
        EnvFilter::new(level)
    } else if let Ok(level) = std::env::var("RUST_LOG") {
        EnvFilter::new(level)
    } else {
        EnvFilter::new("info")
    };
    tracing_subscriber::fmt().with_env_filter(env_filter).init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli
        .config
        .or_else(|| std::env::var_os("CONFIG_PATH").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("./config/config.json"));
    let cfg: Config = load_config(&config_path)?;
    init_logging(&cfg);

    info!(?config_path, "loaded configuration");
    let _summary = run_healthchecks(&cfg).await?;
    // Summary is already logged by the library
    Ok(())
}
