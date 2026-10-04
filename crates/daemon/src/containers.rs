//! The daemon's knowledge of dev containers: the last Docker lookup, mappings remembered after a container is
//! removed, and which sessions are container sessions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use claude_index::containers::{Container, Docker, Mount, to_container, to_host};
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

/// The dev container a session ran in, remembered after it closes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPlace {
    /// That container's id.
    pub container_id: String,
    /// The host folder it served.
    pub host_folder: PathBuf,
}

/// What `containers.json` holds.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    mappings: Vec<Mapping>,
    #[serde(default)]
    sessions: BTreeMap<String, SessionPlace>,
}

/// Dev containers as last seen, every mapping seen before, and the container each session ran in (saved in `file`).
#[derive(Clone, Debug, Default)]
pub struct Registry {
    containers: Vec<Container>,
    mappings: Vec<Mapping>,
    sessions: BTreeMap<String, SessionPlace>,
    file: Option<PathBuf>,
}

fn same_container(a: &str, b: &str) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

impl Registry {
    /// Start from the mappings saved in `file` (missing or unreadable: none).
    pub fn load(file: Option<PathBuf>) -> Registry {
        let saved: Saved = file
            .as_deref()
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Registry { containers: Vec::new(), mappings: saved.mappings, sessions: saved.sessions, file }
    }

    /// Remember the container a running session's process is in (from the index), so it reopens there after it
    /// closes. Whether anything changed.
    pub fn remember(&mut self, record: &SessionRecord) -> bool {
        match (&record.state, record.container_id.as_deref()) {
            (IndexState::InContainer { .. }, Some(id)) => self.remember_session(&record.id, id),
            _ => false,
        }
    }

    /// Remember that `session` runs in the container `container_id` (a known one). Whether anything changed.
    pub fn remember_session(&mut self, session: &str, container_id: &str) -> bool {
        let Some(mapping) = self.mappings.iter().rev().find(|m| same_container(&m.container_id, container_id)) else {
            return false;
        };
        let place =
            SessionPlace { container_id: mapping.container_id.clone(), host_folder: mapping.host_folder.clone() };
        if self.sessions.get(session) == Some(&place) {
            return false;
        }
        self.sessions.insert(session.into(), place);
        self.save();
        true
    }

    /// Ask Docker for the current containers, remember their mappings (oldest first) and save them.
    pub fn refresh(&mut self, docker: &impl Docker) -> Result<(), String> {
        self.containers = docker.list()?;
        let before = self.mappings.clone();
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
        if self.mappings != before {
            self.save();
        }
        Ok(())
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = file.with_extension("json.tmp");
            let saved = Saved { mappings: self.mappings.clone(), sessions: self.sessions.clone() };
            std::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?)?;
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
    /// one when it was seen running in a container (remembered), when a container process runs it, or when its
    /// directory lies under a remembered mapping (or `/workspaces`) and does not exist on this host. Its own
    /// container's mapping wins, else the newest that covers its directory.
    pub fn place(
        &self,
        record: &SessionRecord,
        host_dir_exists: &dyn Fn(&Path) -> bool,
    ) -> Option<(ContainerPlace, Option<PathBuf>)> {
        // A session seen running in a container belongs to it, wherever its directory is.
        if let Some(seen) = self.sessions.get(&record.id) {
            let mapping = self
                .mappings
                .iter()
                .rev()
                .find(|m| m.container_id == seen.container_id)
                .or_else(|| self.mappings.iter().rev().find(|m| m.host_folder == seen.host_folder));
            let host_cwd = mapping.and_then(|m| to_host(&m.mounts, &record.cwd));
            return Some((
                ContainerPlace { host_folder: Some(seen.host_folder.clone()), path: record.cwd.clone() },
                host_cwd,
            ));
        }
        let own = record
            .container_id
            .as_deref()
            .and_then(|id| self.mappings.iter().rev().find(|m| same_container(&m.container_id, id)));
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

/// What resuming a session whose dev container is gone says.
pub const GONE: &str =
    "This session ran in a dev container that no longer exists. Open the project in VS Code to rebuild it.";

/// Quote `word` for `bash -c` (single quotes; `'` becomes `'\''`).
pub fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The `docker exec` command line that runs `program args` in `c` at `container_cwd`, through an interactive bash
/// so `.bashrc` puts `~/.local/bin` on `PATH`, as in VS Code's terminal. Programs other than Claude get `bash -i`.
pub fn exec_command(
    c: &Container,
    container_cwd: &Path,
    program: &str,
    args: &[String],
    marker: &str,
) -> (String, Vec<String>) {
    let mut out: Vec<String> = vec!["exec".into(), "-it".into()];
    if let Some(user) = &c.remote_user {
        out.extend(["-u".into(), user.clone()]);
    }
    out.extend(["-w".into(), container_cwd.display().to_string()]);
    let mut vars =
        vec!["TERM=xterm-256color".to_string(), "COLORTERM=truecolor".into(), format!("{PANE_MARKER}={marker}")];
    vars.extend(c.config_dir.as_ref().map(|dir| format!("CLAUDE_CONFIG_DIR={dir}")));
    for var in vars {
        out.extend(["-e".into(), var]);
    }
    out.push(c.id.clone());
    if Path::new(program).file_stem().is_some_and(|stem| stem == "claude") {
        let words: Vec<String> =
            std::iter::once(program).chain(args.iter().map(String::as_str)).map(shell_quote).collect();
        out.extend(["bash".into(), "-ic".into(), format!("exec {}", words.join(" "))]);
    } else {
        out.extend(["bash".into(), "-i".into()]);
    }
    ("docker".into(), out)
}

fn best(mut found: Vec<&Container>) -> Option<&Container> {
    found.sort_by(|a, b| a.running.cmp(&b.running).then_with(|| a.created.cmp(&b.created)));
    found.pop()
}

/// The container serving `host_folder`: a running one first, then the newest.
pub fn choose<'a>(containers: &'a [Container], host_folder: &Path) -> Option<&'a Container> {
    best(containers.iter().filter(|c| c.host_folder == host_folder).collect())
}

/// The container that mounts `dir`: a running one first, then the newest.
pub fn for_dir<'a>(containers: &'a [Container], dir: &Path) -> Option<&'a Container> {
    best(containers.iter().filter(|c| to_container(&c.mounts, dir).is_some()).collect())
}

/// Refresh, pick the container serving `host_folder`, and start it if it is stopped.
pub fn ready(docker: &impl Docker, registry: &mut Registry, host_folder: &Path) -> Result<Container, String> {
    registry.refresh(docker).map_err(|err| format!("Docker is not available: {err}"))?;
    let container = choose(registry.containers(), host_folder).cloned().ok_or_else(|| GONE.to_string())?;
    if !container.running {
        docker
            .start(&container.id)
            .map_err(|err| format!("Could not start the dev container {}: {err}", container.name))?;
    }
    Ok(container)
}

/// A shell script listing, from inside a container, every process carrying a `GANTRY_PANE` marker as
/// `<pid> <marker>` lines. Used where the host cannot read those processes' environments (another user's, e.g. root).
pub fn marked_script() -> String {
    format!(
        "for p in /proc/[0-9]*; do v=$({{ tr '\\0' '\\n' < $p/environ; }} 2>/dev/null | grep '^{PANE_MARKER}='); \
         [ -n \"$v\" ] && echo \"${{p#/proc/}} ${{v#{PANE_MARKER}=}}\"; done; true"
    )
}

/// The `<pid> <marker>` lines `marked_script` prints.
pub fn parse_marked(output: &str) -> Vec<(u32, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (pid, marker) = line.split_once(' ')?;
            Some((pid.parse().ok()?, marker.to_string()))
        })
        .collect()
}

/// A shell script that asks (SIGTERM), from inside a container, every process carrying `marker` to stop.
pub fn stop_script(marker: &str) -> String {
    let wanted = shell_quote(&format!("{PANE_MARKER}={marker}"));
    format!(
        "for p in /proc/[0-9]*; do {{ tr '\\0' '\\n' < $p/environ; }} 2>/dev/null | grep -qx {wanted} && kill -TERM ${{p#/proc/}}; done; true"
    )
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
            claude_dir: None,
            last_prompt: None,
        }
    }

    #[test]
    fn container_sessions_map_to_their_host_folder_and_mappings_survive_removal() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("containers.json");
        let mut registry = Registry::load(Some(file.clone()));
        registry.refresh(&FakeDocker(vec![container("c1", "/home/u/app", "/workspaces/app", "1")])).unwrap();
        let (place, host) =
            registry.place(&record("/workspaces/app/sub", IndexState::Closed, None), &|_| false).unwrap();
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
        let place = |id: Option<&str>| {
            registry.place(&record("/workspaces/app", IndexState::Closed, id), &|_| false).unwrap().0.host_folder
        };
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

    #[test]
    fn claude_runs_through_an_interactive_bash_as_the_container_user() {
        let c = container("c1", "/home/u/app", "/workspaces/app", "1");
        let (program, args) =
            exec_command(&c, Path::new("/workspaces/app/sub"), "claude", &["--resume".into(), "it's".into()], "i:4");
        assert_eq!(program, "docker");
        assert_eq!(
            args,
            [
                "exec",
                "-it",
                "-u",
                "u",
                "-w",
                "/workspaces/app/sub",
                "-e",
                "TERM=xterm-256color",
                "-e",
                "COLORTERM=truecolor",
                "-e",
                "GANTRY_PANE=i:4",
                "-e",
                "CLAUDE_CONFIG_DIR=/home/u/.claude",
                "c1",
                "bash",
                "-ic",
                "exec 'claude' '--resume' 'it'\\''s'",
            ]
            .map(String::from)
        );
    }

    #[test]
    fn a_shell_pane_is_an_interactive_bash() {
        let c = container("c1", "/home/u/app", "/workspaces/app", "1");
        let (_, args) = exec_command(&c, Path::new("/workspaces/app"), "/bin/zsh", &[], "i:5");
        assert_eq!(&args[args.len() - 3..], ["c1", "bash", "-i"]);
    }

    #[test]
    fn the_running_container_wins_then_the_newest() {
        let mut stopped = container("a", "/h/app", "/workspaces/app", "2026-09-01");
        stopped.running = false;
        let older = container("b", "/h/app", "/workspaces/app", "2026-01-01");
        assert_eq!(choose(&[stopped.clone(), older.clone()], Path::new("/h/app")).unwrap().id, "b");
        assert_eq!(choose(std::slice::from_ref(&stopped), Path::new("/h/app")).unwrap().id, "a");
        assert!(choose(&[stopped], Path::new("/h/other")).is_none());
        assert_eq!(for_dir(std::slice::from_ref(&older), Path::new("/h/app/src")).unwrap().id, "b");
        assert!(for_dir(&[older], Path::new("/h")).is_none());
    }

    struct RecordingDocker {
        containers: Vec<Container>,
        started: std::cell::RefCell<Vec<String>>,
    }

    impl Docker for RecordingDocker {
        fn list(&self) -> Result<Vec<Container>, String> {
            Ok(self.containers.clone())
        }
        fn start(&self, id: &str) -> Result<(), String> {
            self.started.borrow_mut().push(id.into());
            Ok(())
        }
    }

    struct FailingDocker;

    impl Docker for FailingDocker {
        fn list(&self) -> Result<Vec<Container>, String> {
            Err("Cannot connect to the Docker daemon".into())
        }
        fn start(&self, _id: &str) -> Result<(), String> {
            Err("no".into())
        }
    }

    #[test]
    fn a_stopped_container_is_started_and_a_removed_one_explains_itself() {
        let mut c = container("a", "/h/app", "/workspaces/app", "1");
        c.running = false;
        let docker = RecordingDocker { containers: vec![c], started: Default::default() };
        let mut registry = Registry::load(None);
        assert_eq!(ready(&docker, &mut registry, Path::new("/h/app")).unwrap().id, "a");
        assert_eq!(*docker.started.borrow(), ["a"]);
        assert_eq!(ready(&docker, &mut registry, Path::new("/h/gone")).unwrap_err(), GONE);
    }

    #[test]
    fn docker_failures_reach_the_pane() {
        let err = ready(&FailingDocker, &mut Registry::load(None), Path::new("/h/app")).unwrap_err();
        assert!(err.contains("Cannot connect"), "{err}");
    }

    #[test]
    fn a_session_seen_in_a_container_reopens_there_after_it_closes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("containers.json");
        let mut registry = Registry::load(Some(file.clone()));
        registry
            .refresh(&FakeDocker(vec![
                container("old", "/home/u/a/app", "/workspaces/app", "2026-01-01"),
                container("new", "/home/u/b/app", "/workspaces/app", "2026-06-01"),
                // A container that mounts the project at its host path.
                container("same", "/home/u/c", "/home/u/c", "2026-03-01"),
            ]))
            .unwrap();
        // While running, the index names the container (from the process's cgroup); once closed it does not.
        assert!(registry.remember(&record("/workspaces/app", IndexState::InContainer { pid: Some(5) }, Some("old"))));
        assert!(registry.remember(&SessionRecord {
            id: "t".into(),
            ..record("/home/u/c", IndexState::InContainer { pid: Some(6) }, Some("same"))
        }));
        let later = Registry::load(Some(file));
        let closed = later.place(&record("/workspaces/app", IndexState::Closed, None), &|_| false).unwrap();
        assert_eq!((closed.0.host_folder, closed.1), (Some("/home/u/a/app".into()), Some("/home/u/a/app".into())));
        let at_host_path = SessionRecord { id: "t".into(), ..record("/home/u/c", IndexState::Closed, None) };
        assert_eq!(later.place(&at_host_path, &|_| true).unwrap().0.host_folder, Some("/home/u/c".into()));
    }

    #[test]
    fn an_unchanged_refresh_does_not_rewrite_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("containers.json");
        let mut registry = Registry::load(Some(file.clone()));
        let docker = FakeDocker(vec![container("c1", "/h/app", "/workspaces/app", "1")]);
        registry.refresh(&docker).unwrap();
        std::fs::remove_file(&file).unwrap();
        registry.refresh(&docker).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn markers_are_listed_and_stopped_from_inside_a_container() {
        let list = marked_script();
        assert!(list.contains("GANTRY_PANE=") && list.contains("/proc/[0-9]*"), "{list}");
        assert_eq!(parse_marked("12 i:3\n13 i:4\nnoise\n14\n"), vec![(12, "i:3".to_string()), (13, "i:4".into())]);
        let stop = stop_script("i:3");
        assert!(stop.contains("grep -qx 'GANTRY_PANE=i:3'") && stop.contains("kill -TERM"), "{stop}");
        assert!(stop_script("it's").contains(r"'GANTRY_PANE=it'\''s'"));
    }
}
