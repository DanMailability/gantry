//! The daemon's knowledge of dev containers: the last Docker lookup, mappings remembered after a container is
//! removed, and which sessions are container sessions.

use std::path::{Path, PathBuf};

use claude_index::containers::{Container, Docker, Mount, to_host};
use claude_index::index::{IndexState, SessionRecord};
use protocol::message::{ContainerPlace, PaneId};
use serde::{Deserialize, Serialize};

/// Environment variable naming the pane a container process belongs to (`<daemon instance>:<pane>`).
pub const PANE_MARKER: &str = "GANTRY_PANE";
/// Where dev containers mount projects by default.
const WORKSPACES: &str = "/workspaces";

/// A container's host folder and mounts, remembered after the container is removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mapping {
    /// The host folder it served.
    pub host_folder: PathBuf,
    /// Its container id.
    pub container_id: String,
    /// When it was created (newer sorts later).
    pub created: String,
    /// Its bind mounts.
    pub mounts: Vec<Mount>,
}

/// Dev containers as last seen, plus every mapping seen before (saved in `file`).
#[derive(Clone, Debug, Default)]
pub struct Registry {
    containers: Vec<Container>,
    mappings: Vec<Mapping>,
    file: Option<PathBuf>,
}

fn same_container(a: &str, b: &str) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

impl Registry {
    /// Start from the mappings saved in `file` (missing or unreadable: none).
    pub fn load(file: Option<PathBuf>) -> Registry {
        let mappings = file
            .as_deref()
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Registry { containers: Vec::new(), mappings, file }
    }

    /// Ask Docker for the current containers, remember their mappings (oldest first) and save them.
    pub fn refresh(&mut self, docker: &impl Docker) -> Result<(), String> {
        self.containers = docker.list()?;
        for c in &self.containers {
            self.mappings.retain(|m| m.container_id != c.id);
            self.mappings.push(Mapping {
                host_folder: c.host_folder.clone(),
                container_id: c.id.clone(),
                created: c.created.clone(),
                mounts: c.mounts.clone(),
            });
        }
        self.mappings.sort_by(|a, b| a.created.cmp(&b.created));
        self.save();
        Ok(())
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = file.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&self.mappings)?)?;
            std::fs::rename(tmp, file)
        };
        if let Err(err) = write() {
            tracing::warn!("saving {} failed: {err}", file.display());
        }
    }

    /// Containers from the last refresh.
    pub fn containers(&self) -> &[Container] {
        &self.containers
    }

    /// Whether a refresh has seen the container `id` (a full id or a prefix of one).
    pub fn knows(&self, id: &str) -> bool {
        self.mappings.iter().any(|m| same_container(&m.container_id, id))
    }

    /// Whether `record` is a container session, and if so where, plus its directory on this host when known. It is
    /// one when a container process runs it, or when its directory lies under a remembered mapping (or
    /// `/workspaces`) and does not exist on this host. Its own container's mapping wins, else the newest that covers
    /// its directory.
    pub fn place(&self, record: &SessionRecord, host_dir_exists: &dyn Fn(&Path) -> bool) -> Option<(ContainerPlace, Option<PathBuf>)> {
        let own = record.container_id.as_deref().and_then(|id| self.mappings.iter().rev().find(|m| same_container(&m.container_id, id)));
        let mapping = own
            .filter(|m| to_host(&m.mounts, &record.cwd).is_some())
            .or_else(|| self.mappings.iter().rev().find(|m| to_host(&m.mounts, &record.cwd).is_some()));
        let host_cwd = mapping.and_then(|m| to_host(&m.mounts, &record.cwd));
        let in_container = matches!(record.state, IndexState::InContainer { .. });
        let looks_contained = record.cwd.starts_with(WORKSPACES) || host_cwd.is_some();
        if !in_container && !(looks_contained && !host_dir_exists(&record.cwd)) {
            return None;
        }
        let host_folder = mapping.map(|m| m.host_folder.clone());
        Some((ContainerPlace { host_folder, path: record.cwd.clone() }, host_cwd))
    }
}

/// The marker value of `pane` in the daemon `instance`.
pub fn marker(instance: &str, pane: PaneId) -> String {
    format!("{instance}:{pane}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_index::containers::Mount;

    pub(crate) struct FakeDocker(pub Vec<Container>);

    impl Docker for FakeDocker {
        fn list(&self) -> Result<Vec<Container>, String> {
            Ok(self.0.clone())
        }
        fn start(&self, _id: &str) -> Result<(), String> {
            Ok(())
        }
    }

    pub(crate) fn container(id: &str, host: &str, dest: &str, created: &str) -> Container {
        Container {
            id: id.into(),
            name: format!("n-{id}"),
            running: true,
            created: created.into(),
            host_folder: host.into(),
            mounts: vec![Mount { source: host.into(), destination: dest.into() }],
            remote_user: Some("u".into()),
            config_dir: Some("/home/u/.claude".into()),
        }
    }

    fn record(cwd: &str, state: IndexState, container_id: Option<&str>) -> SessionRecord {
        SessionRecord {
            id: "s".into(),
            cwd: cwd.into(),
            title: String::new(),
            last_active_ms: 0,
            state,
            cwd_exists: false,
            activity: None,
            activity_since_ms: 0,
            proc_start: None,
            container_id: container_id.map(Into::into),
        }
    }

    #[test]
    fn container_sessions_map_to_their_host_folder_and_mappings_survive_removal() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("containers.json");
        let mut registry = Registry::load(Some(file.clone()));
        registry.refresh(&FakeDocker(vec![container("c1", "/home/u/app", "/workspaces/app", "1")])).unwrap();
        let (place, host) = registry.place(&record("/workspaces/app/sub", IndexState::Closed, None), &|_| false).unwrap();
        assert_eq!(
            (place.host_folder.as_deref(), place.path.as_path(), host.as_deref()),
            (Some(Path::new("/home/u/app")), Path::new("/workspaces/app/sub"), Some(Path::new("/home/u/app/sub")))
        );
        // The container is removed; a registry loaded from the saved file still maps its sessions.
        let mut later = Registry::load(Some(file));
        later.refresh(&FakeDocker(vec![])).unwrap();
        assert!(later.place(&record("/workspaces/app", IndexState::Closed, None), &|_| false).is_some());
        // A session in a folder that exists on this host is not a container session.
        assert!(later.place(&record("/home/u/app", IndexState::Closed, None), &|_| true).is_none());
    }

    #[test]
    fn two_containers_at_the_same_path_map_by_container_id_else_the_newest() {
        let mut registry = Registry::load(None);
        registry
            .refresh(&FakeDocker(vec![
                container("old", "/home/u/a/app", "/workspaces/app", "2026-01-01"),
                container("new", "/home/u/b/app", "/workspaces/app", "2026-06-01"),
            ]))
            .unwrap();
        let place =
            |id: Option<&str>| registry.place(&record("/workspaces/app", IndexState::Closed, id), &|_| false).unwrap().0.host_folder;
        assert_eq!(place(Some("old")), Some("/home/u/a/app".into()));
        assert_eq!(place(None), Some("/home/u/b/app".into()));
    }

    #[test]
    fn unseen_workspaces_folders_are_container_sessions_without_a_host_folder() {
        let registry = Registry::load(None);
        let (place, host) = registry.place(&record("/workspaces/gone", IndexState::Closed, None), &|_| false).unwrap();
        assert_eq!((place.host_folder, host), (None, None));
        assert!(registry.place(&record("/tmp/x", IndexState::Closed, None), &|_| false).is_none());
        // A running container process is a container session wherever it runs.
        let running = record("/srv", IndexState::InContainer { pid: Some(5) }, Some("zz"));
        assert_eq!(registry.place(&running, &|_| true).unwrap().0.host_folder, None);
    }

    #[test]
    fn a_container_id_prefix_is_known() {
        let mut registry = Registry::load(None);
        registry.refresh(&FakeDocker(vec![container("4a31e67c9795abcd", "/h", "/w", "1")])).unwrap();
        assert!(registry.knows("4a31e67c9795abcd") && registry.knows("4a31e6"));
        assert!(!registry.knows("ffff"));
    }
}
