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
        /// Directory of dashboards (.ndjson Kibana exports, .yaml); overrides the config.
        #[arg(long)]
        dashboards: Option<PathBuf>,
        #[command(flatten)]
        connection: Connection,
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
    /// Check dashboard files: the format, references between columns, KQL,
    /// and with --online the indexes and fields against Elasticsearch. Exits 1
    /// on errors, so it can gate CI.
    Lint {
        /// Teleplot YAML dashboards and/or Kibana exports.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Also check indexes and field types against Elasticsearch.
        #[arg(long)]
        online: bool,
        #[command(flatten)]
        connection: Connection,
    },
    /// Print the JSON Schema of Teleplot's YAML dashboard format (for editors).
    Schema,
    /// Serve the MCP endpoint over stdio, for agents that launch Teleplot
    /// themselves (`teleplot serve` also serves it at /mcp).
    Mcp {
        /// Directory of dashboards; agents save into its `agents/` folder.
        #[arg(long)]
        dashboards: Option<PathBuf>,
        /// Where Teleplot's UI runs, for links in results.
        #[arg(long, default_value = "http://127.0.0.1:3000")]
        url: String,
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
        dashboards: None,
        connection: Connection {
            config: "teleplot.yaml".into(),
            es: None,
            time_zone: None,
        },
    });
    let result = match command {
        Command::Serve {
            addr,
            dashboards,
            connection,
        } => teleplot::server::serve(
            addr,
            teleplot::server::Teleplot::new(connection.config(), dashboards),
        )
        .await
        .map(|()| ExitCode::SUCCESS)
        .map_err(|e| e.to_string()),
        Command::Check { exports } => check(&exports),
        Command::Convert { export, out } => convert(&export, out.as_deref()),
        Command::Lint {
            files,
            online,
            connection,
        } => lint(&files, online.then_some(&connection)).await,
        Command::Schema => {
            println!(
                "{}",
                serde_json::to_string_pretty(&teleplot::mcp::dashboard_schema_json())
                    .unwrap_or_default()
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Mcp {
            dashboards,
            url,
            connection,
        } => mcp_stdio(dashboards, url, &connection).await,
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
        ..Default::default()
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
            for a in &panel_data.annotations {
                match &a.error {
                    Some(e) => println!("  annotation {}: error: {e}", a.label),
                    None => println!("  annotation {}: {} events", a.label, a.events.len()),
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

async fn lint(files: &[PathBuf], online: Option<&Connection>) -> Result<ExitCode, String> {
    use teleplot::lint;
    let engine = online
        .map(|c| c.config().and_then(|c| Engine::new(&c)))
        .transpose()?;
    let mut errors = 0;
    for file in files {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let is_yaml = file.extension().is_some_and(|e| e == "yaml" || e == "yml");
        // YAML is parsed from raw values so problems carry exact paths.
        let parsed: Vec<Result<Dashboard, lint::Problem>> = if is_yaml {
            serde_yaml_ng::Deserializer::from_str(&text)
                .map(|doc| {
                    let value: serde_json::Value =
                        serde::Deserialize::deserialize(doc).map_err(|e| lint::Problem {
                            path: String::new(),
                            severity: lint::Severity::Error,
                            message: e.to_string(),
                        })?;
                    lint::parse(value)
                })
                .collect()
        } else {
            teleplot::kibana::read(&text)
                .map_err(|e| format!("{}: {e}", file.display()))?
                .into_iter()
                .map(Ok)
                .collect()
        };
        for result in parsed {
            let (name, problems) = match result {
                Err(p) => ("(unparsed)".to_string(), vec![p]),
                Ok(d) => {
                    let problems = match &engine {
                        Some(engine) => {
                            let indexes: Vec<String> = d
                                .panels
                                .iter()
                                .filter_map(|p| match &p.content {
                                    PanelContent::Chart(c) => {
                                        Some(c.layers.iter().map(|l| l.data_view.index.clone()))
                                    }
                                    _ => None,
                                })
                                .flatten()
                                .chain(d.controls.iter().map(|c| c.data_view.index.clone()))
                                .collect();
                            lint::check(&d, Some(&engine.fields_for(&indexes).await))
                        }
                        None => lint::check(&d, None),
                    };
                    (d.id.clone(), problems)
                }
            };
            for p in &problems {
                let level = if p.severity == lint::Severity::Error {
                    "error"
                } else {
                    "warning"
                };
                println!(
                    "{}: {name}: {level}: {}: {}",
                    file.display(),
                    if p.path.is_empty() {
                        "(dashboard)"
                    } else {
                        &p.path
                    },
                    p.message
                );
            }
            errors += problems
                .iter()
                .filter(|p| p.severity == lint::Severity::Error)
                .count();
        }
    }
    if errors > 0 {
        eprintln!("{errors} error(s)");
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

async fn mcp_stdio(
    dashboards: Option<PathBuf>,
    url: String,
    connection: &Connection,
) -> Result<ExitCode, String> {
    use rmcp::ServiceExt;
    let app = std::sync::Arc::new(teleplot::server::Teleplot::new(
        connection.config(),
        dashboards,
    ));
    // A process the agent launched is local, so saving is allowed.
    let server = teleplot::mcp::TeleplotMcp::new(app, true, url)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| e.to_string())?;
    server.waiting().await.map_err(|e| e.to_string())?;
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
