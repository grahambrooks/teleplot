use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use teleplot::config::Config;
use teleplot::engine::{Engine, Request};
use teleplot::model::{Cell, Dashboard, PanelContent, TimeRange};
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
    /// Run a dashboard's queries and print each panel's data.
    Query {
        /// Kibana export (.ndjson) or Teleplot dashboard (.yaml).
        file: PathBuf,
        /// Dashboard id or a substring of its title; all dashboards if omitted.
        #[arg(long)]
        dashboard: Option<String>,
        /// Start of the time range (Kibana date math or ISO date); the dashboard's default if omitted.
        #[arg(long, requires = "to")]
        from: Option<String>,
        #[arg(long, requires = "from")]
        to: Option<String>,
        #[command(flatten)]
        connection: Connection,
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

/// Where Elasticsearch is: a config file, or a URL for quick local use.
#[derive(clap::Args)]
struct Connection {
    /// teleplot.yaml
    #[arg(long, default_value = "teleplot.yaml", conflicts_with = "es")]
    config: PathBuf,
    /// Elasticsearch URL (no auth), instead of a config file.
    #[arg(long)]
    es: Option<String>,
    /// IANA time zone, with --es.
    #[arg(long, requires = "es")]
    time_zone: Option<String>,
}

impl Connection {
    fn config(&self) -> Result<Config, String> {
        match &self.es {
            Some(url) => Config::parse(
                &format!(
                    "elasticsearch:\n  url: {url}\ntime_zone: {}\n",
                    self.time_zone.as_deref().unwrap_or("UTC")
                ),
                |_| None,
            ),
            None => Config::load(&self.config),
        }
    }
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
        Command::Query {
            file,
            dashboard,
            from,
            to,
            connection,
        } => query(&file, dashboard.as_deref(), from.zip(to), &connection).await,
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

async fn query(
    file: &Path,
    which: Option<&str>,
    time: Option<(String, String)>,
    connection: &Connection,
) -> Result<ExitCode, String> {
    let engine = Engine::new(&connection.config()?)?;
    let request = Request {
        time: time.map(|(from, to)| TimeRange { from, to }),
        selections: Vec::new(),
    };
    let dashboards = teleplot::dashboards::load_file(file)?;
    let chosen: Vec<&Dashboard> = dashboards
        .iter()
        .filter(|d| {
            which.is_none_or(|w| d.id == w || d.title.to_lowercase().contains(&w.to_lowercase()))
        })
        .collect();
    if chosen.is_empty() {
        return Err("no matching dashboard".into());
    }
    for d in chosen {
        println!("== {}", d.title);
        let data = engine
            .dashboard_data(d, &request, jiff::Timestamp::now())
            .await?;
        for panel in &d.panels {
            let PanelContent::Chart(chart) = &panel.content else {
                continue;
            };
            println!("\n# {}", panel.title.as_deref().unwrap_or(&panel.id));
            let Some(panel_data) = data.get(&panel.id) else {
                continue;
            };
            for w in &panel_data.warnings {
                println!("  ! {w}");
            }
            for (layer, layer_data) in chart.layers.iter().zip(&panel_data.layers) {
                if let Some(e) = &layer_data.error {
                    println!("  error: {e}");
                    continue;
                }
                let headers: Vec<String> = layer_data
                    .columns
                    .iter()
                    .map(|s| {
                        layer
                            .column(&s.id)
                            .and_then(|c| c.label.clone())
                            .unwrap_or_else(|| s.id.clone())
                    })
                    .collect();
                println!("  {}", headers.join(" | "));
                for row in 0..layer_data.rows.min(25) {
                    let cells: Vec<String> = layer_data
                        .columns
                        .iter()
                        .map(|s| cell(&s.values[row]))
                        .collect();
                    println!("  {}", cells.join(" | "));
                }
                if layer_data.rows > 25 {
                    println!("  … {} rows", layer_data.rows);
                }
            }
        }
        println!();
    }
    Ok(ExitCode::SUCCESS)
}

fn cell(c: &Cell) -> String {
    match c {
        Cell::Number(n) if n.fract() == 0.0 && n.abs() < 1e15 => format!("{n:.0}"),
        Cell::Number(n) => format!("{n:.4}"),
        Cell::Text(t) => t.clone(),
        Cell::Null => "-".into(),
    }
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
