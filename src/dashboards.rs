//! Loading dashboards from disk: Kibana exports (`.ndjson`) and Teleplot
//! YAML (`.yaml`/`.yml`, one dashboard per file or several as YAML documents).
//!
//! YAML dashboards' metric references (`op: metric`) are expanded here,
//! from the dashboard's own `metrics` and the `metrics.yaml` beside it; see
//! [`crate::metrics`]. [`read_file`] leaves them as written.

use std::path::Path;

use crate::model::Dashboard;

/// Every dashboard in a file, ready to run: metric references expanded, and
/// a panel whose references can't be expanded shown as a placeholder.
pub fn load_file(path: &Path) -> Result<Vec<Dashboard>, String> {
    let dashboards = read_file(path)?;
    if !is_yaml(path) || !dashboards.iter().any(uses_metrics) {
        return Ok(dashboards);
    }
    let library = crate::metrics::load(&crate::metrics::library_dir(path));
    Ok(dashboards
        .iter()
        .map(|d| crate::metrics::resolve_for_display(d, library.as_ref().map_err(String::as_str)))
        .collect())
}

/// Every dashboard in a file, as written.
pub fn read_file(path: &Path) -> Result<Vec<Dashboard>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    if crate::metrics::is_library(path) {
        return Err(format!(
            "{}: a metric library, not a dashboard",
            path.display()
        ));
    }
    let dashboards = match ext {
        "ndjson" | "json" => crate::kibana::read(&text),
        "yaml" | "yml" => crate::yaml::from_multiple(&text).map_err(|e| e.to_string()),
        other => Err(format!("unknown dashboard file type {other:?}")),
    };
    dashboards.map_err(|e| format!("{}: {e}", path.display()))
}

fn is_yaml(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "yaml" || e == "yml")
}

/// Whether a dashboard refers to a metric by name, or defines its own.
pub fn uses_metrics(d: &Dashboard) -> bool {
    !d.metrics.is_empty()
        || d.panels.iter().any(|p| match &p.content {
            crate::model::PanelContent::Chart(c) => c.layers.iter().any(|l| {
                l.columns
                    .iter()
                    .any(|c| matches!(c.op, crate::model::Operation::Metric { .. }))
            }),
            _ => false,
        })
}

/// Every dashboard in a directory (not recursive). A file that fails to load
/// is reported and skipped rather than hiding the others.
pub fn load_dir(dir: &Path) -> (Vec<Dashboard>, Vec<String>) {
    let (found, errors) = load_dir_with_paths(dir);
    (found.into_iter().map(|(_, d)| d).collect(), errors)
}

/// Like [`load_dir`], keeping the file each dashboard came from. Hidden
/// files (`.history/`, temporary files) and the metric library are ignored;
/// a library that can't be read is reported.
pub fn load_dir_with_paths(dir: &Path) -> (Vec<(std::path::PathBuf, Dashboard)>, Vec<String>) {
    let mut dashboards = Vec::new();
    let mut errors = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => return (dashboards, vec![format!("{}: {e}", dir.display())]),
    };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let hidden = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'));
        let dashboard_file = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "ndjson" | "yaml" | "yml"));
        if hidden || !dashboard_file || !path.is_file() {
            continue;
        }
        if crate::metrics::is_library(&path) {
            if let Err(e) = crate::metrics::load(dir) {
                errors.push(e);
            }
            continue;
        }
        match load_file(&path) {
            Ok(found) => dashboards.extend(found.into_iter().map(|d| (path.clone(), d))),
            Err(e) => errors.push(e),
        }
    }
    (dashboards, errors)
}
