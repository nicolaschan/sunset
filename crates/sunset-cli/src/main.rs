//! sunset-cli binary entry.

use std::path::PathBuf;

use clap::Parser;
use sunset_cli::client::Client;
use sunset_cli::identity::{default_path, load_or_generate};
use sunset_cli::ui::run_app;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "sunset-cli", about = "sunset.chat native ratatui client")]
struct Args {
    /// Relay to connect to. Accepts wss://host:port,
    /// ws://host:port, wts://host[:port], wt://host[:port], or a
    /// hostname (resolved via the relay's identity descriptor).
    #[arg(long, env = "SUNSET_RELAY")]
    relay: Option<String>,

    /// Identity file path. Defaults to <config_dir>/sunset/identity.bin
    /// (or $SUNSET_IDENTITY_PATH).
    #[arg(long, env = "SUNSET_IDENTITY_PATH")]
    identity: Option<PathBuf>,

    /// Display name to publish in presence heartbeats.
    #[arg(long, env = "SUNSET_NAME")]
    name: Option<String>,

    /// Room to auto-join on startup.
    #[arg(long)]
    join: Option<String>,
}

fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let args = Args::parse();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();

    runtime.block_on(local.run_until(async move {
        let id_path = args.identity.unwrap_or_else(default_path);
        let identity = match load_or_generate(&id_path).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("sunset-cli: identity error: {e}");
                std::process::exit(2);
            }
        };

        let client = Client::start(identity);
        if let Some(name) = args.name {
            client.set_self_name(&name);
        }
        if let Some(url) = args.relay {
            if let Err(e) = client.add_relay(url).await {
                client.append_system(format!("relay add failed at startup: {e}"));
            }
        }
        if let Some(room) = args.join {
            if let Err(e) = client.join_room(&room).await {
                client.append_system(format!("auto-join failed: {e}"));
            }
        }

        if let Err(e) = run_app(client).await {
            eprintln!("sunset-cli: ui error: {e}");
            std::process::exit(1);
        }
    }));
    Ok(())
}
