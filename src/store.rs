//! Saving agent-written dashboards, safely.
//!
//! - Only into `<dashboards>/agents/<id>.yaml`; ids are slugs, never paths.
//! - Never over a dashboard someone else wrote: ids used by hand-written
//!   YAML or Kibana exports are refused.
//! - Overwrites must quote the hash of the version the agent read, so an
//!   agent cannot silently undo a change it never saw.
//! - The replaced version moves to `agents/.history/<id>/`, keeping 20.
//! - Writes go to a temporary file then rename, so readers never see half a file.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::model::Dashboard;
use crate::server::{Entry, Source};

const HISTORY_KEPT: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub path: PathBuf,
    pub hash: String,
    /// False when an existing dashboard was replaced.
    pub created: bool,
}

/// The content hash agents quote to overwrite.
pub fn hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

pub fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Saves `dashboard` into `agents_dir`. `others` are every dashboard
/// currently loaded, to refuse ids that belong to someone else.
pub fn save(
    agents_dir: &Path,
    dashboard: &Dashboard,
    expected_hash: Option<&str>,
    others: &[Entry],
) -> Result<Saved, String> {
    let id = &dashboard.id;
    if !valid_id(id) {
        return Err(format!(
            "id {id:?} is not allowed: use 1–64 lower-case letters, digits and dashes, starting with a letter or digit"
        ));
    }
    if let Some(other) = others
        .iter()
        .find(|e| &e.dashboard.id == id && e.source != Source::Agent)
    {
        return Err(format!(
            "id {id:?} belongs to {}, which agents cannot change; save under a new id (e.g. {id}-copy)",
            other.path.display()
        ));
    }

    let path = agents_dir.join(format!("{id}.yaml"));
    let yaml = format!(
        "# Written by an agent through Teleplot's MCP endpoint. Edit freely; agents\n# must re-read it (and its hash) before changing it again.\n{}",
        serde_yaml_ng::to_string(dashboard).map_err(|e| e.to_string())?
    );
    std::fs::create_dir_all(agents_dir).map_err(|e| format!("{}: {e}", agents_dir.display()))?;

    let existing = std::fs::read(&path).ok();
    if let Some(current) = &existing {
        let current_hash = hash(current);
        match expected_hash {
            Some(h) if h == current_hash => {}
            Some(_) => {
                return Err(format!(
                    "{id} changed since you read it; call teleplot_get_dashboard again and apply your change to the current version"
                ));
            }
            None => {
                return Err(format!(
                    "{id} already exists; to replace it pass expected_hash from teleplot_get_dashboard (or save under a new id)"
                ));
            }
        }
        keep_history(agents_dir, id, current)?;
    }

    let tmp = agents_dir.join(format!(".{id}.yaml.tmp"));
    std::fs::write(&tmp, yaml.as_bytes()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Saved {
        path,
        hash: hash(yaml.as_bytes()),
        created: existing.is_none(),
    })
}

fn keep_history(agents_dir: &Path, id: &str, current: &[u8]) -> Result<(), String> {
    let dir = agents_dir.join(".history").join(id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stamp = jiff::Timestamp::now()
        .strftime("%Y%m%dT%H%M%S%.3fZ")
        .to_string();
    // Saves within the same millisecond get a sequence number, so no version is lost.
    let file = (0..1000)
        .map(|n| dir.join(format!("{stamp}-{n:03}.yaml")))
        .find(|p| !p.exists())
        .ok_or("too many saves in one millisecond")?;
    std::fs::write(file, current).map_err(|e| e.to_string())?;
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    versions.sort();
    let excess = versions.len().saturating_sub(HISTORY_KEPT);
    for old in &versions[..excess] {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("teleplot-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn dashboard(id: &str, title: &str) -> Dashboard {
        serde_yaml_ng::from_str(&format!("id: {id}\ntitle: {title}\npanels: []\n")).unwrap()
    }

    #[test]
    fn ids_are_slugs_not_paths() {
        assert!(valid_id("cfr-by-team"));
        for bad in [
            "",
            "../etc",
            "a/b",
            "Caps",
            "-lead",
            "x.yaml",
            &"a".repeat(65),
        ] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        let dir = temp_dir("ids");
        assert!(
            save(&dir, &dashboard("../oops", "x"), None, &[])
                .unwrap_err()
                .contains("not allowed")
        );
    }

    #[test]
    fn creates_then_overwrites_only_with_the_current_hash() {
        let dir = temp_dir("overwrite");
        let first = save(&dir, &dashboard("cfr", "One"), None, &[]).unwrap();
        assert!(first.created);
        assert_eq!(hash(&std::fs::read(&first.path).unwrap()), first.hash);

        let err = save(&dir, &dashboard("cfr", "Two"), None, &[]).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        let err = save(&dir, &dashboard("cfr", "Two"), Some("stale"), &[]).unwrap_err();
        assert!(err.contains("changed since you read it"), "{err}");

        let second = save(&dir, &dashboard("cfr", "Two"), Some(&first.hash), &[]).unwrap();
        assert!(!second.created);
        assert!(
            std::fs::read_to_string(&second.path)
                .unwrap()
                .contains("title: Two")
        );
        let history: Vec<_> = std::fs::read_dir(dir.join(".history/cfr"))
            .unwrap()
            .collect();
        assert_eq!(history.len(), 1, "the replaced version is kept");
        assert!(!dir.join(".cfr.yaml.tmp").exists());
    }

    #[test]
    fn keeps_twenty_versions() {
        let dir = temp_dir("history");
        let mut h = save(&dir, &dashboard("d", "v0"), None, &[]).unwrap().hash;
        for v in 1..=25 {
            h = save(&dir, &dashboard("d", &format!("v{v}")), Some(&h), &[])
                .unwrap()
                .hash;
        }
        assert_eq!(
            std::fs::read_dir(dir.join(".history/d")).unwrap().count(),
            HISTORY_KEPT
        );
    }

    #[test]
    fn never_takes_an_id_someone_else_owns() {
        let dir = temp_dir("owned");
        let human = Entry {
            dashboard: dashboard("ops", "Ops"),
            path: "dashboards/ops.yaml".into(),
            source: Source::Yaml,
        };
        let err = save(&dir, &dashboard("ops", "Mine"), None, &[human]).unwrap_err();
        assert!(
            err.contains("agents cannot change") && err.contains("ops-copy"),
            "{err}"
        );
    }
}
