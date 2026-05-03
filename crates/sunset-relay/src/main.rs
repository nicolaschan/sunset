//! sunset-relay binary entrypoint.

use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use sunset_relay::{Config, Relay, Result};

#[derive(Parser, Debug)]
#[command(version, about = "sunset.chat relay")]
struct Cli {
    /// Path to the TOML config file. If omitted, runs with defaults
    /// (listen 0.0.0.0:8443, data ./data, no federated peers).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("sunset_relay=info,sunset_sync=info")),
        )
        .init();

    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let config = match cli.config {
                    Some(path) => {
                        let text = std::fs::read_to_string(&path).map_err(|e| {
                            sunset_relay::Error::Config(format!("read {}: {e}", path.display(),))
                        })?;
                        Config::from_toml(&text)?
                    }
                    None => Config::defaults()?,
                };
                let handle = Relay::new(config).await?;
                handle.run().await
            })
            .await
    })
}
