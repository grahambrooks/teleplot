//! Loading dashboards from disk: Kibana exports (`.ndjson`) and Teleplot
//! YAML (`.yaml`/`.yml`, one dashboard per file or several as YAML documents).

use std::path::Path;

use serde::Deserialize;

use crate::model::Dashboard;

/// Every dashboard in a file.
pub fn load_file(path: &Path) -> Result<Vec<Dashboard>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let dashboards = match ext {
        "ndjson" | "json" => crate::kibana::read(&text),
        "yaml" | "yml" => serde_yaml_ng::Deserializer::from_str(&text)
            .map(|doc| Dashboard::deserialize(doc).map_err(|e| e.to_string()))
            .collect(),
        other => Err(format!("unknown dashboard file type {other:?}")),
    };
    dashboards.map_err(|e| format!("{}: {e}", path.display()))
}

/// Every dashboard in a directory (not recursive). A file that fails to load
/// is reported and skipped rather than hiding the others.
pub fn load_dir(dir: &Path) -> (Vec<Dashboard>, Vec<String>) {
    let (found, errors) = load_dir_with_paths(dir);
    (found.into_iter().map(|(_, d)| d).collect(), errors)
}

/// Like [`load_dir`], keeping the file each dashboard came from. Hidden
/// files (`.history/`, temporary files) are ignored.
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
        match load_file(&path) {
            Ok(found) => dashboards.extend(found.into_iter().map(|d| (path.clone(), d))),
            Err(e) => errors.push(e),
        }
    }
    (dashboards, errors)
}
