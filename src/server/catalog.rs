//! The dashboards being served, reloaded when their files change.
//!
//! Dashboards come from the dashboards directory (Kibana exports and
//! hand-written YAML) and its `agents/` folder (written through MCP).

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::model::Dashboard;

/// Where agent-written dashboards live, inside the dashboards directory.
pub const AGENTS_DIR: &str = "agents";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A Kibana saved-objects export (read-only to agents).
    Kibana,
    /// Teleplot YAML written by a person (read-only to agents).
    Yaml,
    /// Teleplot YAML written through MCP, in `agents/`.
    Agent,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub dashboard: Dashboard,
    pub path: PathBuf,
    pub source: Source,
}

pub struct Catalog {
    dir: PathBuf,
    state: RwLock<Loaded>,
}

#[derive(Default)]
struct Loaded {
    entries: Arc<Vec<Entry>>,
    dashboards: Arc<Vec<Dashboard>>,
    errors: Vec<String>,
    fingerprint: Vec<(PathBuf, Option<SystemTime>, u64)>,
}

impl Catalog {
    pub fn load(dir: PathBuf) -> Arc<Self> {
        let catalog = Arc::new(Catalog {
            dir,
            state: RwLock::new(Loaded::default()),
        });
        catalog.reload_if_changed();
        catalog
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.dir.join(AGENTS_DIR)
    }

    pub fn dashboards(&self) -> Arc<Vec<Dashboard>> {
        self.state.read().expect("catalog lock").dashboards.clone()
    }

    pub fn entries(&self) -> Arc<Vec<Entry>> {
        self.state.read().expect("catalog lock").entries.clone()
    }

    pub fn errors(&self) -> Vec<String> {
        self.state.read().expect("catalog lock").errors.clone()
    }

    pub fn get(&self, id: &str) -> Option<Dashboard> {
        self.dashboards().iter().find(|d| d.id == id).cloned()
    }

    pub fn entry(&self, id: &str) -> Option<Entry> {
        self.entries()
            .iter()
            .find(|e| e.dashboard.id == id)
            .cloned()
    }

    /// Re-reads the directories if any dashboard file was added, removed or modified.
    pub fn reload_if_changed(&self) -> bool {
        let fingerprint = [self.dir.clone(), self.agents_dir()]
            .iter()
            .flat_map(|d| fingerprint(d))
            .collect::<Vec<_>>();
        if self.state.read().expect("catalog lock").fingerprint == fingerprint
            && !fingerprint.is_empty()
        {
            return false;
        }
        let mut entries = Vec::new();
        let mut errors = Vec::new();
        for (dir, agents) in [(self.dir.clone(), false), (self.agents_dir(), true)] {
            if agents && !dir.exists() {
                continue;
            }
            let (found, mut errs) = crate::dashboards::load_dir_with_paths(&dir);
            errors.append(&mut errs);
            for (path, dashboard) in found {
                let source = if agents {
                    Source::Agent
                } else if path
                    .extension()
                    .is_some_and(|e| e == "ndjson" || e == "json")
                {
                    Source::Kibana
                } else {
                    Source::Yaml
                };
                if entries
                    .iter()
                    .any(|e: &Entry| e.dashboard.id == dashboard.id)
                {
                    errors.push(format!(
                        "{}: dashboard id {:?} is already used by another file; skipped",
                        path.display(),
                        dashboard.id
                    ));
                    continue;
                }
                entries.push(Entry {
                    dashboard,
                    path,
                    source,
                });
            }
        }
        for e in &errors {
            tracing::warn!("{e}");
        }
        tracing::info!(
            "loaded {} dashboards from {}",
            entries.len(),
            self.dir.display()
        );
        let dashboards = Arc::new(entries.iter().map(|e| e.dashboard.clone()).collect());
        *self.state.write().expect("catalog lock") = Loaded {
            entries: Arc::new(entries),
            dashboards,
            errors,
            fingerprint,
        };
        true
    }

    /// Polls for changes. Editing a dashboard file is how dashboards change.
    pub fn watch(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                tick.tick().await;
                let catalog = self.clone();
                let _ = tokio::task::spawn_blocking(move || catalog.reload_if_changed()).await;
            }
        });
    }
}

fn fingerprint(dir: &Path) -> Vec<(PathBuf, Option<SystemTime>, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file()
                .then(|| (e.path(), meta.modified().ok(), meta.len()))
        })
        .collect();
    out.sort();
    out
}
