//! The dashboards being served, reloaded when the directory changes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use crate::model::Dashboard;

pub struct Catalog {
    dir: PathBuf,
    state: RwLock<Loaded>,
}

#[derive(Default)]
struct Loaded {
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

    pub fn dashboards(&self) -> Arc<Vec<Dashboard>> {
        self.state.read().expect("catalog lock").dashboards.clone()
    }

    pub fn errors(&self) -> Vec<String> {
        self.state.read().expect("catalog lock").errors.clone()
    }

    pub fn get(&self, id: &str) -> Option<Dashboard> {
        self.dashboards().iter().find(|d| d.id == id).cloned()
    }

    /// Re-reads the directory if any dashboard file was added, removed or modified.
    pub fn reload_if_changed(&self) -> bool {
        let fingerprint = fingerprint(&self.dir);
        if self.state.read().expect("catalog lock").fingerprint == fingerprint
            && !fingerprint.is_empty()
        {
            return false;
        }
        let (dashboards, errors) = crate::dashboards::load_dir(&self.dir);
        for e in &errors {
            tracing::warn!("{e}");
        }
        tracing::info!(
            "loaded {} dashboards from {}",
            dashboards.len(),
            self.dir.display()
        );
        *self.state.write().expect("catalog lock") = Loaded {
            dashboards: Arc::new(dashboards),
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
