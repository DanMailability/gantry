//! Which machine a daemon runs on: host ids, pane keys and host-qualified project keys.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::message::PaneId;

/// The name of the host that is this machine.
const LOCAL: &str = "local";

/// Start of a remote project key (see [`project_key`]).
const REMOTE_PREFIX: &str = "ssh://";

/// A machine whose daemon the GUI talks to: `local`, or an SSH host by the name saved in the setup wizard.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostId(String);

impl HostId {
    /// This machine.
    pub fn local() -> HostId {
        HostId(LOCAL.into())
    }

    /// The host called `name`; the name `local`, or an empty one, is this machine.
    pub fn named(name: &str) -> HostId {
        if name.is_empty() { HostId::local() } else { HostId(name.into()) }
    }

    /// Whether this is the machine the GUI runs on.
    pub fn is_local(&self) -> bool {
        self.0 == LOCAL
    }

    /// The host's name.
    pub fn name(&self) -> &str {
        &self.0
    }

    /// The name to show for a remote host; `None` for this machine.
    pub fn remote_name(&self) -> Option<&str> {
        (!self.is_local()).then_some(self.0.as_str())
    }
}

impl Default for HostId {
    fn default() -> HostId {
        HostId::local()
    }
}

/// A pane of one host's daemon. Pane ids are only unique within a daemon, so a pane is named by both.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PaneKey {
    /// The host whose daemon owns the pane.
    pub host: HostId,
    /// The pane's id in that daemon.
    pub pane: PaneId,
}

impl PaneKey {
    /// Pane `pane` of `host`.
    pub fn new(host: HostId, pane: PaneId) -> PaneKey {
        PaneKey { host, pane }
    }

    /// Pane `pane` of this machine's daemon.
    pub fn local(pane: PaneId) -> PaneKey {
        PaneKey::new(HostId::local(), pane)
    }
}

/// The key of the project `root` on `host`: the folder itself for this machine (so saved project colors still match),
/// `ssh://<host>/<folder>` otherwise. The same folder on two hosts is two projects.
pub fn project_key(host: &HostId, root: &Path) -> PathBuf {
    if host.is_local() {
        return root.to_path_buf();
    }
    let mut key = format!("{REMOTE_PREFIX}{}", host.name());
    if !root.has_root() {
        key.push('/');
    }
    key.push_str(&root.to_string_lossy());
    PathBuf::from(key)
}

/// The host and folder a project key stands for (the inverse of [`project_key`]).
pub fn split_project_key(key: &Path) -> (HostId, PathBuf) {
    let Some(rest) = key.to_str().and_then(|key| key.strip_prefix(REMOTE_PREFIX)) else {
        return (HostId::local(), key.to_path_buf());
    };
    match rest.split_once('/') {
        Some((host, path)) => (HostId::named(host), PathBuf::from(format!("/{path}"))),
        None => (HostId::named(rest), PathBuf::from("/")),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    #[test]
    fn the_local_host_is_called_local() {
        assert!(HostId::local().is_local());
        assert_eq!(HostId::named("local"), HostId::local());
        assert_eq!(HostId::named(""), HostId::local());
        assert_eq!(HostId::default(), HostId::local());
        assert_eq!(HostId::local().remote_name(), None);
        assert_eq!(HostId::named("devbox").remote_name(), Some("devbox"));
        assert!(!HostId::named("devbox").is_local());
    }

    #[test]
    fn a_host_id_is_a_bare_string_on_disk() {
        assert_eq!(serde_json::to_string(&HostId::named("devbox")).unwrap(), "\"devbox\"");
        assert_eq!(serde_json::from_str::<HostId>("\"devbox\"").unwrap(), HostId::named("devbox"));
    }

    #[test]
    fn pane_keys_differ_by_host() {
        assert_eq!(PaneKey::local(1), PaneKey::new(HostId::local(), 1));
        assert_ne!(PaneKey::local(1), PaneKey::new(HostId::named("devbox"), 1));
        assert_ne!(PaneKey::local(1), PaneKey::local(2));
    }

    #[test]
    fn a_local_project_key_is_the_folder_itself() {
        assert_eq!(project_key(&HostId::local(), Path::new("/r/app")), PathBuf::from("/r/app"));
        assert_eq!(split_project_key(Path::new("/r/app")), (HostId::local(), PathBuf::from("/r/app")));
    }

    #[test]
    fn a_remote_project_key_carries_the_host() {
        let devbox = HostId::named("devbox");
        let key = project_key(&devbox, Path::new("/home/u/app"));
        assert_eq!(key, PathBuf::from("ssh://devbox/home/u/app"));
        assert_eq!(key.file_name().unwrap(), "app");
        assert_eq!(split_project_key(&key), (devbox, PathBuf::from("/home/u/app")));
        assert_ne!(key, project_key(&HostId::named("other"), Path::new("/home/u/app")));
    }

    #[test]
    fn the_root_folder_of_a_remote_host_round_trips() {
        let key = project_key(&HostId::named("devbox"), Path::new("/"));
        assert_eq!(split_project_key(&key), (HostId::named("devbox"), PathBuf::from("/")));
    }
}
