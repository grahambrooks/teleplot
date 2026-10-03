use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use teleplot::model::Dashboard;
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
    /// Report which panels of Kibana exports Teleplot can render. Exits
    /// non-zero if any supported panel could not be converted.
    Check {
        /// Kibana saved-object exports (.ndjson).
        #[arg(required = true)]
        exports: Vec<PathBuf>,
    },
    /// Convert a Kibana export to Teleplot YAML dashboards.
    Convert {
        /// Kibana saved-object export (.ndjson).
        export: PathBuf,
        /// Write one `<title>.yaml` per dashboard here, instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();

    let command = Cli::parse().command.unwrap_or(Command::Serve {
        addr: DEFAULT_ADDR.parse().expect("valid default address"),
    });
    let result = match command {
        Command::Serve { addr } => teleplot::server::serve(addr)
            .await
            .map(|()| ExitCode::SUCCESS)
            .map_err(|e| e.to_string()),
        Command::Check { exports } => check(&exports),
        Command::Convert { export, out } => convert(&export, out.as_deref()),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("teleplot: {message}");
            ExitCode::FAILURE
        }
    }
}

fn read_export(path: &Path) -> Result<Vec<Dashboard>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    teleplot::kibana::read(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn check(exports: &[PathBuf]) -> Result<ExitCode, String> {
    let mut failed = false;
    for path in exports {
        let dashboards = read_export(path)?;
        let (report, total) = teleplot::report::check(&dashboards);
        println!("{} — {} dashboards", path.display(), dashboards.len());
        print!("{report}");
        failed |= total.unparseable > 0;
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn convert(export: &Path, out: Option<&Path>) -> Result<ExitCode, String> {
    let dashboards = read_export(export)?;
    match out {
        None => {
            for d in &dashboards {
                println!("---\n{}", to_yaml(d)?);
            }
        }
        Some(dir) => {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            for d in &dashboards {
                let path = dir.join(format!("{}.yaml", slug(&d.title)));
                std::fs::write(&path, to_yaml(d)?)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                eprintln!("wrote {}", path.display());
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn to_yaml(d: &Dashboard) -> Result<String, String> {
    serde_yaml_ng::to_string(d).map_err(|e| format!("{}: {e}", d.title))
}

/// `[Delivery] PR flow` → `delivery-pr-flow`.
fn slug(title: &str) -> String {
    let lowered: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    lowered
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
