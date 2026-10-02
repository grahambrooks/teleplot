use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

/// Localhost only by default: Teleplot has no auth.
const DEFAULT_ADDR: &str = "127.0.0.1:3000";

#[derive(Parser)]
#[command(name = "teleplot", version, about)]
struct Cli {
    /// Defaults to `serve`.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the dashboard UI.
    Serve {
        /// Address to listen on.
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let command = Cli::parse().command.unwrap_or(Command::Serve {
        addr: DEFAULT_ADDR.parse().expect("valid default address"),
    });
    match command {
        Command::Serve { addr } => teleplot::server::serve(addr).await,
    }
}
