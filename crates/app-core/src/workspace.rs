//! The GUI's workspace: tabs of split panes, what each pane runs, and reconciling a saved layout
//! with the daemon's live panes at startup.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use protocol::message::{PaneId, PaneInfo, SessionState, SessionStatus, SpawnRequest, claude_session};
use serde::{Deserialize, Serialize};

use crate::attention::most_urgent;
use crate::sessions::SessionStore;
use crate::settings::{Settings, ShellSettings, expand_home};
use crate::split_tree::{Axis, Direction, Node, NodeId};

/// Program name of Claude Code.
pub const CLAUDE: &str = "claude";

/// What a pane runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneLaunch {
    /// Program.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Directory it runs in.
    pub cwd: PathBuf,
    /// Host folder of the dev container it runs in; `None` = this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<PathBuf>,
    /// Runs Claude Code, whatever the program is called. Layouts from before this flag leave it unset and
    /// fall back to the program name (see `is_claude`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claude: bool,
    /// The host Claude config dir it runs with; `None` = the daemon's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_dir: Option<PathBuf>,
    /// Extra environment variables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
}

/// The config dirs in effect: the configured list, or the implicit Default (the daemon's dir, `claude`, no
/// arguments).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeDirs {
    /// The dirs, in settings order.
    pub dirs: Vec<ResolvedDir>,
    /// Index of the default dir.
    pub default: usize,
}

/// One config dir with its path expanded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedDir {
    /// Display name.
    pub name: String,
    /// Directory, `~` expanded.
    pub path: PathBuf,
    /// Program that runs Claude.
    pub command: String,
    /// Arguments placed before Gantry's own.
    pub args: Vec<String>,
}

impl ClaudeDirs {
    /// The dirs of `settings`; with none configured, the implicit "Default" at `daemon_dir` (else `<home>/.claude`).
    pub fn from_settings(settings: &Settings, daemon_dir: Option<&Path>, home: Option<&Path>) -> ClaudeDirs {
        if settings.claude_dirs.is_empty() {
            let path = daemon_dir.map(Path::to_path_buf).or_else(|| home.map(|home| home.join(".claude"))).unwrap_or_default();
            return ClaudeDirs { dirs: vec![ResolvedDir { name: "Default".into(), path, command: CLAUDE.into(), args: Vec::new() }], default: 0 };
        }
        let dirs: Vec<ResolvedDir> = settings
            .claude_dirs
            .iter()
            .map(|dir| ResolvedDir { name: dir.name.clone(), path: expand_home(&dir.path, home), command: dir.command.clone(), args: dir.args.clone() })
            .collect();
        let default = settings.default_claude_dir.as_deref().and_then(|name| dirs.iter().position(|dir| dir.name == name)).unwrap_or(0);
        ClaudeDirs { dirs, default }
    }

    /// The dir for a new session in `folder`: the explicit `pick` (a name), else the one remembered for exactly
    /// this folder, else the default. Names that match no dir are skipped.
    pub fn for_folder(&self, folder: &Path, pick: Option<&str>, memory: &BTreeMap<PathBuf, String>) -> &ResolvedDir {
        let named = |name: &str| self.dirs.iter().find(|dir| dir.name == name);
        pick.and_then(named).or_else(|| memory.get(folder).and_then(|name| named(name))).unwrap_or(&self.dirs[self.default])
    }

    /// The dir at `path`, if configured.
    pub fn by_path(&self, path: &Path) -> Option<&ResolvedDir> {
        self.dirs.iter().find(|dir| dir.path == path)
    }
}

impl PaneLaunch {
    /// A new Claude Code session, with its own `--session-id`, in `cwd`.
    pub fn claude(cwd: PathBuf) -> PaneLaunch {
        PaneLaunch {
            program: CLAUDE.into(),
            args: vec!["--session-id".into(), uuid::Uuid::new_v4().to_string()],
            cwd,
            container: None,
            claude: true,
            claude_dir: None,
            env: Vec::new(),
        }
    }

    /// Claude Code resuming `session` in `cwd`, in the dev container serving `container` (`None` = this machine).
    pub fn resume(session: &str, cwd: PathBuf, container: Option<PathBuf>) -> PaneLaunch {
        PaneLaunch {
            program: CLAUDE.into(),
            args: vec!["--resume".into(), session.into()],
            cwd,
            container,
            claude: true,
            claude_dir: None,
            env: Vec::new(),
        }
    }

    /// A new Claude Code session in `cwd` with `dir`'s command and arguments. A dir other than `daemon_dir` adds
    /// `CLAUDE_CONFIG_DIR`. In a dev container (`container`) the program stays `claude`, the container's own config
    /// dir applies and only the dir's arguments are kept.
    pub fn claude_with(cwd: PathBuf, dir: &ResolvedDir, daemon_dir: Option<&Path>, container: Option<PathBuf>) -> PaneLaunch {
        let mut args = dir.args.clone();
        args.extend(["--session-id".into(), uuid::Uuid::new_v4().to_string()]);
        Self::claude_in(cwd, args, dir, daemon_dir, container)
    }

    /// Claude Code resuming `session` in `cwd`. `session_dir` is the config dir the session was recorded in: its
    /// command and arguments are used when it is configured (else the default dir's), and `CLAUDE_CONFIG_DIR`
    /// keeps pointing at it unless it is the daemon's own.
    pub fn resume_with(
        session: &str,
        cwd: PathBuf,
        container: Option<PathBuf>,
        session_dir: Option<&Path>,
        dirs: &ClaudeDirs,
        daemon_dir: Option<&Path>,
    ) -> PaneLaunch {
        let configured = session_dir.and_then(|path| dirs.by_path(path));
        let dir = configured.unwrap_or(&dirs.dirs[dirs.default]);
        let mut args = dir.args.clone();
        args.extend(["--resume".into(), session.into()]);
        let recorded = ResolvedDir { path: session_dir.map_or_else(|| dir.path.clone(), Path::to_path_buf), ..dir.clone() };
        Self::claude_in(cwd, args, &recorded, daemon_dir, container)
    }

    fn claude_in(cwd: PathBuf, args: Vec<String>, dir: &ResolvedDir, daemon_dir: Option<&Path>, container: Option<PathBuf>) -> PaneLaunch {
        if container.is_some() {
            return PaneLaunch { program: CLAUDE.into(), args, cwd, container, claude: true, claude_dir: None, env: Vec::new() };
        }
        let own = daemon_dir == Some(dir.path.as_path());
        PaneLaunch {
            program: dir.command.clone(),
            args,
            cwd,
            container: None,
            claude: true,
            claude_dir: (!own).then(|| dir.path.clone()),
            env: if own { Vec::new() } else { vec![("CLAUDE_CONFIG_DIR".into(), dir.path.to_string_lossy().into_owned())] },
        }
    }

    /// A shell in `cwd`; an empty `shell.program` is the daemon's default shell.
    pub fn shell(cwd: PathBuf, shell: &ShellSettings) -> PaneLaunch {
        PaneLaunch { program: shell.program.clone(), args: shell.args.clone(), cwd, container: None, claude: false, claude_dir: None, env: Vec::new() }
    }

    /// The same launch, in the dev container serving `host_folder` (`None` = this machine).
    pub fn in_container(self, host_folder: Option<PathBuf>) -> PaneLaunch {
        PaneLaunch { container: host_folder, ..self }
    }

    /// The Claude session this command line names (`--session-id`, `--resume`, `-r`).
    pub fn session(&self) -> Option<&str> {
        claude_session(self.is_claude(), &self.program, &self.args)
    }

    /// Whether this runs Claude Code.
    pub fn is_claude(&self) -> bool {
        self.claude || Path::new(&self.program).file_stem().is_some_and(|stem| stem == CLAUDE)
    }

    /// Short label: program and directory names, e.g. `claude · repo`.
    pub fn label(&self) -> String {
        let name = |path: &Path| path.file_name().map(|n| n.to_string_lossy().into_owned());
        let program = name(Path::new(&self.program)).unwrap_or_else(|| if self.program.is_empty() { "shell".into() } else { self.program.clone() });
        match name(&self.cwd) {
            Some(dir) => format!("{program} · {dir}"),
            None => program,
        }
    }

    /// The daemon request that starts this, optionally in `cwd_from`'s current directory.
    pub fn request(&self, cwd_from: Option<PaneId>, cols: u16, rows: u16, scrollback: u32) -> SpawnRequest {
        SpawnRequest {
            program: self.program.clone(),
            args: self.args.clone(),
            cwd: self.cwd.clone(),
            cols,
            rows,
            cwd_from,
            container: self.container.clone(),
            claude: self.is_claude(),
            env: self.env.clone(),
            scrollback: Some(scrollback),
        }
    }
}

/// What the GUI was asked to open at startup.
#[derive(Clone, Debug)]
pub struct Launch {
    /// Explicitly requested command (`gantry -- cmd` or `--cwd`); always opens a new tab.
    pub explicit: Option<PaneLaunch>,
    /// What to open when there is nothing to restore.
    pub default: PaneLaunch,
}

/// One pane slot of the layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leaf {
    /// Daemon pane shown here; `None` while starting or after its process ended.
    pub pane: Option<PaneId>,
    /// What it runs.
    pub launch: PaneLaunch,
    /// Claude session it last ran (followed across `/clear`); `None` for other programs.
    #[serde(default)]
    pub session: Option<String>,
}

impl Leaf {
    /// A leaf about to run `launch`.
    pub fn new(launch: PaneLaunch) -> Leaf {
        let session = launch.session().map(String::from);
        Leaf { pane: None, launch, session }
    }

    /// What Continue / Restart does, given the daemon's sessions: focus the pane that already runs the
    /// leaf's session, refuse while it runs outside Gantry, else start `rerun` — resuming when the session is
    /// known, or when the list has not arrived yet.
    pub fn rerun_with(&self, sessions: &SessionStore) -> Rerun {
        let known = self.session.as_deref().and_then(|id| sessions.get(id));
        match known.map(|session| session.state) {
            Some(SessionState::Managed { pane }) => Rerun::Focus(pane),
            Some(SessionState::External { .. }) => Rerun::Busy,
            _ => Rerun::Start(self.rerun(known.is_some() || !sessions.loaded())),
        }
    }

    /// What re-running this finished leaf starts. Claude: `--resume` its session when the daemon knows
    /// it (it has a transcript), else the same `--session-id` afresh; without a session (layouts from
    /// before session tracking) `--continue`. Other programs: the same command.
    pub fn rerun(&self, resumable: bool) -> PaneLaunch {
        let launch = &self.launch;
        if !launch.is_claude() {
            return launch.clone();
        }
        let args = match &self.session {
            Some(id) if resumable => vec!["--resume".into(), id.clone()],
            Some(id) => vec!["--session-id".into(), id.clone()],
            None => vec!["--continue".into()],
        };
        // Arguments before the session flag (a config dir's own) are kept.
        let own = launch.args.iter().position(|arg| matches!(arg.as_str(), "--session-id" | "--resume" | "-r" | "--continue")).unwrap_or(0);
        let args = launch.args[..own].iter().cloned().chain(args).collect();
        PaneLaunch { args, ..launch.clone() }
    }
}

/// What Continue / Restart on a finished leaf should do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rerun {
    /// Start this.
    Start(PaneLaunch),
    /// The session already runs in this pane: show it instead.
    Focus(PaneId),
    /// The session runs outside Gantry; starting it again would run it twice.
    Busy,
}

/// One tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    /// Its panes.
    pub root: Node,
    /// The leaf with focus inside this tab.
    pub focused: NodeId,
    /// The Settings tab (not saved; its root is a node id with no leaf).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub settings: bool,
}

/// Default sidebar width, in pixels.
pub const SIDEBAR_WIDTH: f32 = 280.0;

/// How the sidebar is shown (saved with the layout).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarState {
    /// Shown at all.
    pub visible: bool,
    /// Width in pixels.
    pub width: f32,
}

impl Default for SidebarState {
    fn default() -> Self {
        SidebarState { visible: true, width: SIDEBAR_WIDTH }
    }
}

/// All tabs of the window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    /// Daemon instance the pane ids belong to.
    pub instance: String,
    /// Tabs, left to right.
    pub tabs: Vec<Tab>,
    /// Index of the visible tab.
    pub active: usize,
    /// Every leaf by id.
    pub leaves: BTreeMap<NodeId, Leaf>,
    /// Sidebar visibility and width.
    #[serde(default)]
    pub sidebar: SidebarState,
    next_id: NodeId,
}

impl Workspace {
    fn alloc(&mut self) -> NodeId {
        self.next_id += 1;
        self.next_id
    }

    /// Open `launch` in a new last tab and focus it. Returns the new leaf.
    pub fn new_tab(&mut self, launch: PaneLaunch) -> NodeId {
        let leaf = self.alloc();
        self.leaves.insert(leaf, Leaf::new(launch));
        self.tabs.push(Tab { root: Node::Leaf(leaf), focused: leaf, settings: false });
        self.active = self.tabs.len() - 1;
        leaf
    }

    /// Show the Settings tab, opening it as the last tab if it is not open. Returns its index.
    pub fn open_settings(&mut self) -> usize {
        let index = match self.settings_tab() {
            Some(index) => index,
            None => {
                let id = self.alloc();
                self.tabs.push(Tab { root: Node::Leaf(id), focused: id, settings: true });
                self.tabs.len() - 1
            }
        };
        self.active = index;
        index
    }

    /// Index of the Settings tab, if open.
    pub fn settings_tab(&self) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.settings)
    }

    /// Close the Settings tab (as `close` does for a last leaf: the neighbour becomes active).
    pub fn close_settings(&mut self) {
        let Some(index) = self.settings_tab() else { return };
        self.tabs.remove(index);
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    /// A copy without the Settings tab, for saving.
    pub fn saveable(&self) -> Workspace {
        let mut saved = self.clone();
        saved.close_settings();
        saved
    }

    /// Split the focused leaf, putting `launch` second, and focus it. `None` when there are no tabs.
    pub fn split(&mut self, axis: Axis, launch: PaneLaunch) -> Option<NodeId> {
        let target = self.focused()?;
        let (split_id, leaf) = (self.alloc(), self.alloc());
        let tab = &mut self.tabs[self.active];
        tab.root.split(target, axis, split_id, leaf);
        tab.focused = leaf;
        self.leaves.insert(leaf, Leaf::new(launch));
        Some(leaf)
    }

    /// Remove `leaf` (and its tab if it was the last leaf). Focus moves to its sibling.
    pub fn close(&mut self, leaf: NodeId) -> Option<Leaf> {
        let index = self.tab_of(leaf)?;
        let tab = &mut self.tabs[index];
        let sibling = tab.root.sibling_leaf(leaf);
        let root = std::mem::replace(&mut tab.root, Node::Leaf(0));
        match root.without(leaf) {
            Some(root) => {
                tab.root = root;
                if tab.focused == leaf {
                    tab.focused = sibling.expect("a split leaf has a sibling");
                }
            }
            None => {
                self.tabs.remove(index);
                if index < self.active || self.active >= self.tabs.len() {
                    self.active = self.active.saturating_sub(1);
                }
            }
        }
        self.leaves.remove(&leaf)
    }

    /// The visible tab.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// The focused leaf of the visible tab.
    pub fn focused(&self) -> Option<NodeId> {
        self.active_tab().filter(|tab| !tab.settings).map(|tab| tab.focused)
    }

    /// Focus `leaf`, selecting its tab. False if it is not in the layout.
    pub fn focus(&mut self, leaf: NodeId) -> bool {
        let Some(index) = self.tab_of(leaf) else { return false };
        self.active = index;
        self.tabs[index].focused = leaf;
        true
    }

    /// Move focus to the neighbouring leaf in `direction`; returns it.
    pub fn focus_direction(&mut self, direction: Direction) -> Option<NodeId> {
        let tab = self.tabs.get_mut(self.active).filter(|tab| !tab.settings)?;
        let next = tab.root.neighbor(tab.focused, direction)?;
        tab.focused = next;
        Some(next)
    }

    /// Show tab `index`. False if out of range.
    pub fn select_tab(&mut self, index: usize) -> bool {
        let valid = index < self.tabs.len();
        if valid {
            self.active = index;
        }
        valid
    }

    /// Show the tab `delta` places away, wrapping around.
    pub fn cycle_tab(&mut self, delta: isize) {
        let len = self.tabs.len() as isize;
        if len > 0 {
            self.active = (self.active as isize + delta).rem_euclid(len) as usize;
        }
    }

    /// Move the visible tab `delta` places, stopping at the ends; it stays visible.
    pub fn move_tab(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let target = (self.active as isize + delta).clamp(0, self.tabs.len() as isize - 1) as usize;
        self.move_tab_to(self.active, target);
    }

    /// Move tab `from` to position `to`; the visible tab stays visible wherever it ends up. Out-of-range
    /// indices do nothing.
    pub fn move_tab_to(&mut self, from: usize, to: usize) {
        let len = self.tabs.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = match self.active {
            active if active == from => to,
            active if from < active && active <= to => active - 1,
            active if to <= active && active < from => active + 1,
            active => active,
        };
    }

    /// Set a split's ratio in the visible tab.
    pub fn set_ratio(&mut self, split: NodeId, ratio: f32) {
        if let Some(tab) = self.tabs.get_mut(self.active).filter(|tab| !tab.settings) {
            tab.root.set_ratio(split, ratio);
        }
    }

    /// Leaf showing daemon pane `pane`.
    pub fn leaf_for_pane(&self, pane: PaneId) -> Option<NodeId> {
        self.leaves.iter().find(|(_, leaf)| leaf.pane == Some(pane)).map(|(id, _)| *id)
    }

    /// Leaf whose Claude session is `session`.
    pub fn leaf_for_session(&self, session: &str) -> Option<NodeId> {
        self.leaves.iter().find(|(_, leaf)| leaf.session.as_deref() == Some(session)).map(|(id, _)| *id)
    }

    /// Whether `leaf` is in the visible tab.
    pub fn is_visible(&self, leaf: NodeId) -> bool {
        self.active_tab().is_some_and(|tab| tab.root.contains(leaf))
    }

    /// What `leaf`'s Claude session is doing, while its pane runs that session.
    pub fn leaf_status<'s>(&self, leaf: NodeId, sessions: &'s SessionStore) -> Option<&'s SessionStatus> {
        let leaf = self.leaves.get(&leaf)?;
        let pane = leaf.pane?;
        let session = sessions.get(leaf.session.as_deref()?)?;
        if session.state != (SessionState::Managed { pane }) {
            return None;
        }
        session.status.as_ref()
    }

    /// The most urgent status among tab `index`'s leaves.
    pub fn tab_status<'s>(&self, index: usize, sessions: &'s SessionStore) -> Option<&'s SessionStatus> {
        let tab = self.tabs.get(index).filter(|tab| !tab.settings)?;
        most_urgent(tab.root.leaves().into_iter().filter_map(|leaf| self.leaf_status(leaf, sessions)))
    }

    /// Index of the tab containing `leaf`.
    pub fn tab_of(&self, leaf: NodeId) -> Option<usize> {
        self.tabs.iter().position(|tab| !tab.settings && tab.root.contains(leaf))
    }

    /// `leaf`'s process started as `pane` in `cwd`.
    pub fn pane_started(&mut self, leaf: NodeId, pane: PaneId, cwd: PathBuf) {
        if let Some(leaf) = self.leaves.get_mut(&leaf) {
            leaf.pane = Some(pane);
            leaf.launch.cwd = cwd;
        }
    }

    /// `pane`'s process ended; returns the leaf that showed it.
    pub fn pane_exited(&mut self, pane: PaneId) -> Option<NodeId> {
        let leaf = self.leaf_for_pane(pane)?;
        self.leaves.get_mut(&leaf)?.pane = None;
        Some(leaf)
    }

    /// The daemon reports that `pane` runs `session`; returns whether the leaf's session changed.
    pub fn session_seen(&mut self, pane: PaneId, session: &str) -> bool {
        let Some(leaf) = self.leaf_for_pane(pane).and_then(|leaf| self.leaves.get_mut(&leaf)) else { return false };
        if leaf.session.as_deref() == Some(session) {
            return false;
        }
        leaf.session = Some(session.into());
        true
    }

    /// Whether every tab's leaves exist, each tab's focus is one of its leaves, ids are unique and
    /// below the id counter, every leaf belongs to exactly one tab, and `active` is in range.
    pub fn is_consistent(&self) -> bool {
        let mut seen = HashSet::new();
        let mut tab_leaves = 0;
        for tab in &self.tabs {
            for id in tab.root.ids() {
                if id > self.next_id || !seen.insert(id) {
                    return false;
                }
            }
            if tab.settings {
                continue;
            }
            let leaves = tab.root.leaves();
            if !leaves.contains(&tab.focused) || !leaves.iter().all(|leaf| self.leaves.contains_key(leaf)) {
                return false;
            }
            tab_leaves += leaves.len();
        }
        self.tabs.iter().filter(|tab| tab.settings).count() <= 1
            && tab_leaves == self.leaves.len()
            && (self.tabs.is_empty() || self.active < self.tabs.len())
    }
}

/// A saved layout reconciled with the daemon's live panes.
pub struct Restored {
    /// The workspace to show.
    pub workspace: Workspace,
    /// Leaves to attach to their (live) pane.
    pub attach: Vec<(NodeId, PaneId)>,
    /// Leaves whose process must be started.
    pub spawn: Vec<NodeId>,
}

/// Rebuild the workspace at startup. Leaves keep their live pane; leaves whose pane is gone (or
/// belongs to another daemon instance) become dead; live panes missing from the layout become new
/// tabs (most recently focused last, and active); an explicit launch opens a new focused tab; an
/// empty result opens the default. Without `restore_layout`, leaves whose pane is gone are dropped (live panes
/// still reattach).
pub fn restore(saved: Option<Workspace>, instance: &str, live: &[PaneInfo], launch: &Launch, restore_layout: bool) -> Restored {
    let mut workspace = saved.unwrap_or_default();
    let same_daemon = workspace.instance == instance;
    workspace.instance = instance.into();
    let live_ids: HashSet<PaneId> = live.iter().map(|info| info.pane).collect();
    for leaf in workspace.leaves.values_mut() {
        if !same_daemon || leaf.pane.is_some_and(|pane| !live_ids.contains(&pane)) {
            leaf.pane = None;
        }
    }
    if !restore_layout {
        let dead: Vec<NodeId> = workspace.leaves.iter().filter(|(_, leaf)| leaf.pane.is_none()).map(|(id, _)| *id).collect();
        for leaf in dead {
            workspace.close(leaf);
        }
    }
    let mut attach: Vec<(NodeId, PaneId)> =
        workspace.leaves.iter().filter_map(|(id, leaf)| leaf.pane.map(|pane| (*id, pane))).collect();
    let known: HashSet<PaneId> = attach.iter().map(|(_, pane)| *pane).collect();
    let mut orphans: Vec<&PaneInfo> = live.iter().filter(|info| !known.contains(&info.pane)).collect();
    orphans.sort_by_key(|info| (info.last_focused, info.pane));
    for info in orphans {
        let leaf = workspace.new_tab(PaneLaunch {
            program: info.program.clone(),
            args: info.args.clone(),
            cwd: info.cwd.clone(),
            container: info.container.clone(),
            claude: info.claude,
            claude_dir: None,
            env: Vec::new(),
        });
        workspace.pane_started(leaf, info.pane, info.cwd.clone());
        attach.push((leaf, info.pane));
    }
    let mut spawn = Vec::new();
    if let Some(explicit) = &launch.explicit {
        spawn.push(workspace.new_tab(explicit.clone()));
    }
    if workspace.tabs.is_empty() {
        spawn.push(workspace.new_tab(launch.default.clone()));
    }
    Restored { workspace, attach, spawn }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_saved_before_containers_load_as_local_launches() {
        let json = r#"{"program":"claude","args":["--resume","s"],"cwd":"/r"}"#;
        let launch: PaneLaunch = serde_json::from_str(json).unwrap();
        assert_eq!(launch.container, None);
        assert_eq!(serde_json::to_string(&launch).unwrap(), json);
    }

    #[test]
    fn a_container_launch_reruns_and_requests_in_its_container() {
        let launch = PaneLaunch::claude("/home/u/app".into()).in_container(Some("/home/u/app".into()));
        let leaf = Leaf::new(launch);
        assert_eq!(leaf.rerun(true).container, Some(PathBuf::from("/home/u/app")));
        assert_eq!(leaf.launch.request(None, 80, 24, 10_000).container, Some(PathBuf::from("/home/u/app")));
    }

    fn shell(cwd: &str) -> PaneLaunch {
        PaneLaunch { program: "/bin/bash".into(), args: vec![], cwd: cwd.into(), container: None, claude: false, claude_dir: None, env: Vec::new() }
    }

    fn info(pane: PaneId, program: &str, last_focused: u64) -> PaneInfo {
        PaneInfo { pane, program: program.into(), args: vec!["-l".into()], cwd: "/srv".into(), last_focused, container: None, claude: false }
    }

    #[test]
    fn new_tabs_and_splits_focus_the_new_leaf() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        assert_eq!((ws.tabs.len(), ws.focused()), (1, Some(a)));
        let b = ws.split(Axis::Horizontal, shell("/b")).unwrap();
        assert_eq!(ws.focused(), Some(b));
        assert_eq!(ws.active_tab().unwrap().root.leaves(), vec![a, b]);
        let c = ws.new_tab(shell("/c"));
        assert_eq!((ws.active, ws.focused()), (1, Some(c)));
        assert!(ws.is_consistent());
    }

    #[test]
    fn open_settings_twice_gives_one_tab() {
        let mut ws = Workspace::default();
        ws.new_tab(shell("/a"));
        ws.new_tab(shell("/b"));
        assert_eq!(ws.open_settings(), 2);
        ws.select_tab(0);
        assert_eq!(ws.open_settings(), 2);
        assert_eq!((ws.tabs.len(), ws.active, ws.settings_tab()), (3, 2, Some(2)));
        assert!(!ws.leaves.contains_key(&ws.tabs[2].focused));
        assert_eq!(ws.tab_status(2, &SessionStore::default()), None);
    }

    #[test]
    fn settings_tab_takes_part_in_cycle_select_and_move() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        let b = ws.new_tab(shell("/b"));
        ws.open_settings();
        ws.cycle_tab(1);
        assert_eq!(ws.active, 0);
        ws.cycle_tab(-1);
        assert_eq!(ws.settings_tab(), Some(ws.active));
        ws.move_tab(-2);
        assert_eq!((ws.active, ws.settings_tab()), (0, Some(0)));
        assert_eq!(ws.tabs.iter().map(|t| t.focused).skip(1).collect::<Vec<_>>(), vec![a, b]);
        assert!(ws.select_tab(2));
        assert_eq!(ws.focused(), Some(b));
        assert!(ws.is_consistent());
    }

    #[test]
    fn closing_settings_activates_neighbour() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        ws.open_settings();
        let b = ws.new_tab(shell("/b"));
        ws.select_tab(1);
        ws.close_settings();
        assert_eq!((ws.settings_tab(), ws.active, ws.focused()), (None, 1, Some(b)));
        ws.open_settings();
        ws.close_settings();
        assert_eq!((ws.active, ws.focused()), (1, Some(b)), "closing the last tab activates the new last");
        ws.select_tab(0);
        ws.open_settings();
        ws.select_tab(1);
        ws.close_settings();
        assert_eq!((ws.active, ws.focused()), (1, Some(b)), "closing a tab before the active one keeps it active");
        ws.close_settings();
        assert_eq!(ws.tabs.len(), 2);
        assert!(ws.focus(a) && ws.is_consistent());
    }

    #[test]
    fn settings_tab_is_never_saved() {
        let mut ws = Workspace { instance: "i".into(), ..Default::default() };
        let a = ws.new_tab(shell("/a"));
        ws.open_settings();
        let b = ws.new_tab(shell("/b"));
        ws.select_tab(1);
        let saved = ws.saveable();
        assert_eq!((saved.settings_tab(), saved.tabs.len(), saved.active, saved.focused()), (None, 2, 1, Some(b)));
        assert!(saved.is_consistent());
        ws.select_tab(2);
        assert_eq!(ws.saveable().active, 1, "settings before active: active shifts down");
        ws.select_tab(0);
        assert_eq!((ws.saveable().active, ws.saveable().focused()), (0, Some(a)));
        ws.select_tab(1);
        ws.move_tab_to(1, 2);
        assert_eq!((ws.saveable().active, ws.saveable().focused()), (1, Some(b)), "settings last and active: last tab");
        assert_eq!(ws.tabs.len(), 3, "the original keeps it");
    }

    #[test]
    fn split_and_focus_do_nothing_while_settings_active() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        let b = ws.split(Axis::Horizontal, shell("/b")).unwrap();
        ws.open_settings();
        let before = ws.clone();
        assert_eq!(ws.focused(), None);
        assert_eq!(ws.split(Axis::Vertical, shell("/c")), None);
        assert_eq!(ws.focus_direction(Direction::Left), None);
        ws.set_ratio(ws.tabs[0].root.ids()[0], 0.3);
        assert_eq!(ws, before);
        assert!(!ws.is_visible(a) && !ws.is_visible(b));
    }

    #[test]
    fn consistent_with_settings_open() {
        let mut ws = Workspace::default();
        ws.new_tab(shell("/a"));
        ws.open_settings();
        assert!(ws.is_consistent());
        let mut two = ws.clone();
        two.tabs.push(two.tabs[1].clone());
        assert!(!two.is_consistent(), "ids stay unique and there is one settings tab");
        ws.close_settings();
        assert!(ws.is_consistent());
    }

    #[test]
    fn close_leaf_with_settings_id_does_nothing() {
        let mut ws = Workspace::default();
        ws.new_tab(shell("/a"));
        ws.open_settings();
        let id = ws.tabs[1].focused;
        assert_eq!((ws.tab_of(id), ws.close(id), ws.focus(id)), (None, None, false));
        assert_eq!((ws.leaf_for_pane(1), ws.leaf_for_session("x")), (None, None));
        assert_eq!((ws.tabs.len(), ws.active), (2, 1));
    }

    #[test]
    fn split_without_tabs_does_nothing() {
        assert_eq!(Workspace::default().split(Axis::Vertical, shell("/")), None);
    }

    #[test]
    fn closing_the_focused_leaf_focuses_its_sibling() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        let b = ws.split(Axis::Horizontal, shell("/b")).unwrap();
        let c = ws.split(Axis::Vertical, shell("/c")).unwrap(); // a | (b over c), c focused
        assert_eq!(ws.close(c).unwrap().launch.cwd, PathBuf::from("/c"));
        assert_eq!(ws.focused(), Some(b));
        ws.focus(a);
        ws.close(b);
        assert_eq!((ws.focused(), ws.leaves.len()), (Some(a), 1));
        assert!(ws.is_consistent());
    }

    #[test]
    fn closing_an_unfocused_leaf_keeps_focus() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        let b = ws.split(Axis::Horizontal, shell("/b")).unwrap();
        ws.close(a);
        assert_eq!(ws.focused(), Some(b));
    }

    #[test]
    fn closing_a_tabs_last_leaf_removes_the_tab_and_keeps_active_valid() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        let b = ws.new_tab(shell("/b"));
        let c = ws.new_tab(shell("/c"));
        ws.close(c);
        assert_eq!((ws.tabs.len(), ws.active, ws.focused()), (2, 1, Some(b)));
        ws.close(a);
        assert_eq!((ws.tabs.len(), ws.active, ws.focused()), (1, 0, Some(b)));
        ws.close(b);
        assert_eq!((ws.tabs.len(), ws.focused()), (0, None));
        assert_eq!(ws.close(b), None);
    }

    #[test]
    fn tabs_cycle_with_wraparound_and_move_with_the_active_tab() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        ws.new_tab(shell("/b"));
        ws.new_tab(shell("/c"));
        ws.cycle_tab(1);
        assert_eq!(ws.active, 0);
        ws.cycle_tab(-1);
        assert_eq!(ws.active, 2);
        assert!(ws.select_tab(0) && !ws.select_tab(3));
        ws.move_tab(1);
        assert_eq!((ws.active, ws.tabs[1].focused), (1, a));
        ws.move_tab(-5);
        assert_eq!((ws.active, ws.tabs[0].focused), (0, a), "moves stop at the ends");
    }

    #[test]
    fn focus_moves_between_neighbours_and_across_tabs() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        let b = ws.split(Axis::Horizontal, shell("/b")).unwrap();
        assert_eq!(ws.focus_direction(Direction::Left), Some(a));
        assert_eq!(ws.focus_direction(Direction::Left), None);
        let other = ws.new_tab(shell("/o"));
        assert!(ws.focus(b));
        assert_eq!(ws.active, 0, "focusing a leaf selects its tab");
        assert!(ws.focus(other) && ws.active == 1);
    }

    #[test]
    fn pane_lifecycle_updates_the_leaf() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/guess"));
        ws.pane_started(a, 7, "/real".into());
        assert_eq!((ws.leaf_for_pane(7), ws.leaves[&a].launch.cwd.clone()), (Some(a), PathBuf::from("/real")));
        assert_eq!(ws.pane_exited(7), Some(a));
        assert_eq!(ws.leaves[&a].pane, None);
    }

    #[test]
    fn pane_exited_for_an_unknown_pane_is_ignored() {
        let mut ws = Workspace::default();
        ws.new_tab(shell("/a"));
        assert_eq!(ws.pane_exited(42), None);
    }

    fn claude_leaf(session: Option<&str>) -> Leaf {
        Leaf { pane: None, launch: PaneLaunch { program: "claude".into(), args: vec![], cwd: "/r".into(), container: None, claude: false, claude_dir: None, env: Vec::new() }, session: session.map(Into::into) }
    }

    #[test]
    fn new_claude_launches_get_a_fresh_session_id() {
        let (a, b) = (PaneLaunch::claude("/r".into()), PaneLaunch::claude("/r".into()));
        assert_eq!(a.args[0], "--session-id");
        assert_ne!(a.session(), b.session());
        assert!(uuid::Uuid::parse_str(a.session().unwrap()).is_ok());
        let mut ws = Workspace::default();
        let leaf = ws.new_tab(a.clone());
        assert_eq!(ws.leaves[&leaf].session.as_deref(), a.session());
        assert_eq!(PaneLaunch::claude("/home/me/repo".into()).label(), "claude · repo");
    }

    #[test]
    fn resume_launches_name_their_session() {
        let launch = PaneLaunch::resume("abc", "/r".into(), None);
        assert_eq!((launch.args.clone(), launch.session()), (vec!["--resume".to_string(), "abc".to_string()], Some("abc")));
        assert_eq!(shell("/r").session(), None);
    }

    #[test]
    fn rerun_resumes_the_leafs_session() {
        assert_eq!(claude_leaf(Some("abc")).rerun(true).args, vec!["--resume".to_string(), "abc".to_string()]);
        // No transcript (nothing was said): start the same session id afresh.
        assert_eq!(claude_leaf(Some("abc")).rerun(false).args, vec!["--session-id".to_string(), "abc".to_string()]);
        // Layouts from before session tracking.
        assert_eq!(claude_leaf(None).rerun(true).args, vec!["--continue".to_string()]);
        assert_eq!(claude_leaf(None).rerun(true).cwd, PathBuf::from("/r"));
        let sh = Leaf::new(shell("/r"));
        assert_eq!(sh.rerun(true), sh.launch);
    }

    fn session_info(id: &str, state: protocol::message::SessionState) -> protocol::message::SessionInfo {
        protocol::message::SessionInfo {
            id: id.into(),
            cwd: "/r".into(),
            title: String::new(),
            last_active_ms: 0,
            state,
            cwd_exists: true,
            status: None,
            status_since_ms: 0,
            container: None, claude_dir: None,
        }
    }

    #[test]
    fn rerun_never_runs_a_session_twice() {
        use protocol::message::SessionState;
        let leaf = claude_leaf(Some("x"));
        let mut store = SessionStore::default();
        // Before the first list: assume it may exist and resume.
        assert_eq!(leaf.rerun_with(&store), Rerun::Start(leaf.rerun(true)));
        store.replace(vec![]);
        assert_eq!(leaf.rerun_with(&store), Rerun::Start(leaf.rerun(false)));
        store.replace(vec![session_info("x", SessionState::Closed)]);
        assert_eq!(leaf.rerun_with(&store), Rerun::Start(leaf.rerun(true)));
        store.replace(vec![session_info("x", SessionState::Managed { pane: 4 })]);
        assert_eq!(leaf.rerun_with(&store), Rerun::Focus(4));
        store.replace(vec![session_info("x", SessionState::External { pid: Some(9) })]);
        assert_eq!(leaf.rerun_with(&store), Rerun::Busy);
        let sh = Leaf::new(shell("/r"));
        assert_eq!(sh.rerun_with(&store), Rerun::Start(sh.launch.clone()));
    }

    #[test]
    fn leaves_and_tabs_show_the_status_of_the_session_their_pane_runs() {
        use protocol::message::{SessionInfo, SessionState, SessionStatus};
        let mut ws = Workspace::default();
        let a = ws.new_tab(PaneLaunch::resume("a", "/r".into(), None));
        ws.pane_started(a, 1, "/r".into());
        let b = ws.split(Axis::Horizontal, PaneLaunch::resume("b", "/r".into(), None)).unwrap();
        ws.pane_started(b, 2, "/r".into());
        let other = ws.new_tab(shell("/r"));
        let mut store = SessionStore::default();
        let with = |id: &str, pane, status| SessionInfo { status: Some(status), ..session_info(id, SessionState::Managed { pane }) };
        store.replace(vec![with("a", 1, SessionStatus::Idle), with("b", 2, SessionStatus::NeedsInput { reason: None })]);
        assert_eq!(ws.leaf_status(a, &store), Some(&SessionStatus::Idle));
        assert_eq!(ws.tab_status(0, &store), Some(&SessionStatus::NeedsInput { reason: None }));
        assert_eq!((ws.leaf_status(other, &store), ws.tab_status(1, &store)), (None, None));
        // A stale entry for another pane (the leaf's process ended, the session moved) is not this leaf's status.
        store.replace(vec![with("a", 9, SessionStatus::Busy)]);
        assert_eq!(ws.leaf_status(a, &store), None);
        // The visible tab is the second one (opened last).
        assert_eq!((ws.is_visible(other), ws.is_visible(a)), (true, false));
    }

    #[test]
    fn leaves_are_found_by_session() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(PaneLaunch::resume("x", "/r".into(), None));
        ws.new_tab(shell("/r"));
        assert_eq!((ws.leaf_for_session("x"), ws.leaf_for_session("y")), (Some(a), None));
    }

    #[test]
    fn leaves_follow_the_session_their_pane_reports() {
        let mut ws = Workspace::default();
        let leaf = ws.new_tab(PaneLaunch::claude("/r".into()));
        ws.pane_started(leaf, 5, "/r".into());
        assert!(ws.session_seen(5, "after-clear"));
        assert!(!ws.session_seen(5, "after-clear"));
        assert!(!ws.session_seen(99, "x"));
        assert_eq!(ws.leaves[&leaf].session.as_deref(), Some("after-clear"));
        assert_eq!(ws.leaves[&leaf].rerun(true).args, vec!["--resume".to_string(), "after-clear".to_string()]);
    }

    #[test]
    fn restore_adopted_claude_panes_know_their_session() {
        let live = [PaneInfo { pane: 4, program: "claude".into(), args: vec!["--resume".into(), "xyz".into()], cwd: "/srv".into(), last_focused: 1, container: None, claude: false }];
        let restored = restore(None, "i", &live, &launch(), true);
        let leaf = restored.attach[0].0;
        assert_eq!(restored.workspace.leaves[&leaf].session.as_deref(), Some("xyz"));
    }

    #[test]
    fn move_tab_to_moves_one_tab_and_the_active_tab_follows_its_content() {
        let mut ws = Workspace::default();
        for dir in ["/a", "/b", "/c", "/d"] {
            ws.new_tab(shell(dir));
        }
        let order = |ws: &Workspace| ws.tabs.iter().map(|tab| ws.leaves[&tab.focused].launch.cwd.display().to_string()).collect::<Vec<_>>();
        ws.active = 1; // "/b"
        ws.move_tab_to(0, 3);
        assert_eq!(order(&ws), ["/b", "/c", "/d", "/a"]);
        assert_eq!(ws.active, 0, "the visible tab is still /b");
        ws.move_tab_to(3, 0);
        assert_eq!(order(&ws), ["/a", "/b", "/c", "/d"]);
        assert_eq!(ws.active, 1);
        ws.move_tab_to(1, 2);
        assert_eq!(order(&ws), ["/a", "/c", "/b", "/d"]);
        assert_eq!(ws.active, 2, "the moved tab was the visible one");
    }

    #[test]
    fn moving_a_tab_onto_itself_changes_nothing() {
        let mut ws = Workspace::default();
        ws.new_tab(shell("/a"));
        ws.new_tab(shell("/b"));
        let before = ws.clone();
        ws.move_tab_to(1, 1);
        ws.move_tab_to(0, 7);
        ws.move_tab_to(9, 0);
        assert_eq!(ws, before);
    }

    #[test]
    fn the_sidebar_defaults_to_visible() {
        let state = Workspace::default().sidebar;
        assert_eq!((state.visible, state.width), (true, SIDEBAR_WIDTH));
    }

    #[test]
    fn layouts_saved_with_a_sidebar_view_still_load() {
        let state: SidebarState = serde_json::from_str(r#"{"visible":false,"width":300.0,"view":"Status"}"#).unwrap();
        assert_eq!((state.visible, state.width), (false, 300.0));
    }

    #[test]
    fn spawn_requests_carry_the_focused_pane() {
        let request = shell("/a").request(Some(3), 100, 30, 10_000);
        assert_eq!((request.cwd_from, request.cols, request.rows, request.program.as_str()), (Some(3), 100, 30, "/bin/bash"));
    }

    fn launch() -> Launch {
        Launch { explicit: None, default: shell("/home") }
    }

    #[test]
    fn restore_with_nothing_spawns_the_default() {
        let restored = restore(None, "i1", &[], &launch(), true);
        assert_eq!(restored.workspace.tabs.len(), 1);
        assert_eq!(restored.spawn, vec![restored.workspace.focused().unwrap()]);
        assert!(restored.attach.is_empty());
        assert_eq!(restored.workspace.instance, "i1");
    }

    #[test]
    fn restore_reattaches_live_panes_and_marks_missing_ones_dead() {
        let mut saved = Workspace { instance: "i1".into(), ..Workspace::default() };
        let a = saved.new_tab(shell("/a"));
        saved.pane_started(a, 1, "/a".into());
        let b = saved.split(Axis::Vertical, shell("/b")).unwrap();
        saved.pane_started(b, 2, "/b".into());
        let restored = restore(Some(saved), "i1", &[info(1, "bash", 5)], &launch(), true);
        assert_eq!(restored.attach, vec![(a, 1)]);
        assert_eq!(restored.workspace.leaves[&b].pane, None, "pane 2 is gone");
        assert!(restored.spawn.is_empty(), "dead panes wait for Continue/Restart");
        assert_eq!(restored.workspace.tabs.len(), 1);
    }

    #[test]
    fn restore_from_another_daemon_instance_attaches_nothing() {
        let mut saved = Workspace { instance: "old".into(), ..Workspace::default() };
        let a = saved.new_tab(shell("/a"));
        saved.pane_started(a, 1, "/a".into());
        let restored = restore(Some(saved), "new", &[info(1, "zsh", 1)], &launch(), true);
        assert_eq!(restored.workspace.leaves[&a].pane, None);
        assert_eq!(restored.workspace.tabs.len(), 2, "the new daemon's pane 1 is adopted as its own tab");
        let (adopted, pane) = restored.attach[0];
        assert_ne!(adopted, a);
        assert_eq!((pane, restored.workspace.leaves[&adopted].launch.program.as_str()), (1, "zsh"));
    }

    #[test]
    fn restore_adopts_live_panes_missing_from_the_layout_most_recent_last() {
        let restored = restore(None, "i", &[info(5, "a", 9), info(4, "b", 2)], &launch(), true);
        let ws = &restored.workspace;
        assert_eq!(restored.attach.iter().map(|(_, pane)| *pane).collect::<Vec<_>>(), vec![4, 5]);
        assert_eq!(ws.leaves[&ws.focused().unwrap()].pane, Some(5));
        assert_eq!(ws.leaves[&ws.focused().unwrap()].launch.args, vec!["-l".to_string()]);
        assert!(restored.spawn.is_empty());
    }

    #[test]
    fn restore_with_an_explicit_launch_opens_it_in_a_new_focused_tab() {
        let mut saved = Workspace::default();
        saved.new_tab(shell("/a"));
        let launch = Launch { explicit: Some(PaneLaunch::claude("/x".into())), default: shell("/home") };
        let restored = restore(Some(saved), "i", &[], &launch, true);
        let focused = restored.workspace.focused().unwrap();
        assert_eq!(restored.spawn, vec![focused]);
        assert!(restored.workspace.leaves[&focused].launch.is_claude());
        assert_eq!(restored.workspace.tabs.len(), 2);
    }

    fn two_dirs_settings() -> Settings {
        use crate::settings::ClaudeDir;
        Settings {
            claude_dirs: vec![
            ClaudeDir { name: "Personal".into(), path: "~/.claude".into(), command: "claude".into(), args: vec![] },
            ClaudeDir { name: "Work".into(), path: "~/.claude-work".into(), command: "claude-w".into(), args: vec!["--model".into(), "opus".into()] },
        ],
            ..Settings::default()
        }
    }

    fn two_dirs() -> ClaudeDirs {
        ClaudeDirs::from_settings(&two_dirs_settings(), Some(Path::new("/home/me/.claude")), Some(Path::new("/home/me")))
    }

    #[test]
    fn implicit_default_when_no_dirs() {
        let dirs = ClaudeDirs::from_settings(&Settings::default(), Some(Path::new("/d/.claude")), Some(Path::new("/d")));
        assert_eq!(dirs.dirs, vec![ResolvedDir { name: "Default".into(), path: "/d/.claude".into(), command: "claude".into(), args: vec![] }]);
        let launch = PaneLaunch::claude_with("/r".into(), &dirs.dirs[0], Some(Path::new("/d/.claude")), None);
        assert_eq!((launch.program.as_str(), launch.env.is_empty(), launch.claude_dir), ("claude", true, None));
    }

    #[test]
    fn resolution_pick_then_memory_then_default() {
        let mut settings = two_dirs_settings();
        settings.default_claude_dir = Some("Work".into());
        let dirs = ClaudeDirs::from_settings(&settings, None, Some(Path::new("/home/me")));
        assert_eq!(dirs.dirs[1].path, PathBuf::from("/home/me/.claude-work"));
        let memory = BTreeMap::from([(PathBuf::from("/r/a"), "Personal".to_string())]);
        assert_eq!(dirs.for_folder(Path::new("/r/b"), None, &memory).name, "Work");
        assert_eq!(dirs.for_folder(Path::new("/r/a"), None, &memory).name, "Personal");
        assert_eq!(dirs.for_folder(Path::new("/r/a"), Some("Work"), &memory).name, "Work");
    }

    #[test]
    fn memory_is_exact_folder_not_parent() {
        let dirs = two_dirs();
        let memory = BTreeMap::from([(PathBuf::from("/r/a"), "Work".to_string())]);
        assert_eq!(dirs.for_folder(Path::new("/r/a/sub"), None, &memory).name, "Personal");
    }

    #[test]
    fn memory_for_unknown_dir_uses_default() {
        let dirs = two_dirs();
        let memory = BTreeMap::from([(PathBuf::from("/r/a"), "Gone".to_string())]);
        assert_eq!(dirs.for_folder(Path::new("/r/a"), None, &memory).name, "Personal");
        assert_eq!(dirs.for_folder(Path::new("/r/a"), Some("Nope"), &memory).name, "Personal");
    }

    #[test]
    fn claude_with_sets_env_only_for_non_daemon_dir() {
        let dirs = two_dirs();
        let daemon = Some(Path::new("/home/me/.claude"));
        let own = PaneLaunch::claude_with("/r".into(), &dirs.dirs[0], daemon, None);
        assert_eq!((own.env.is_empty(), own.claude_dir.clone(), own.claude), (true, None, true));
        let work = PaneLaunch::claude_with("/r".into(), &dirs.dirs[1], daemon, None);
        assert_eq!(work.program, "claude-w");
        assert_eq!(&work.args[..2], ["--model", "opus"]);
        assert_eq!(work.args[2], "--session-id");
        assert_eq!(work.env, vec![("CLAUDE_CONFIG_DIR".to_string(), "/home/me/.claude-work".to_string())]);
        assert_eq!(work.claude_dir, Some(PathBuf::from("/home/me/.claude-work")));
        assert!(work.session().is_some(), "the wrapper program is still tracked");
    }

    #[test]
    fn container_launch_keeps_claude_and_dir_args() {
        let dirs = two_dirs();
        let launch = PaneLaunch::claude_with("/r".into(), &dirs.dirs[1], Some(Path::new("/home/me/.claude")), Some("/r".into()));
        assert_eq!((launch.program.as_str(), launch.container.clone(), launch.env.is_empty(), launch.claude_dir.clone()), ("claude", Some(PathBuf::from("/r")), true, None));
        assert_eq!(&launch.args[..2], ["--model", "opus"]);
    }

    #[test]
    fn resume_uses_session_dir_command() {
        let dirs = two_dirs();
        let daemon = Some(Path::new("/home/me/.claude"));
        let launch = PaneLaunch::resume_with("s1", "/r".into(), None, Some(Path::new("/home/me/.claude-work")), &dirs, daemon);
        assert_eq!(launch.program, "claude-w");
        assert_eq!(launch.args, ["--model", "opus", "--resume", "s1"]);
        assert_eq!(launch.env, vec![("CLAUDE_CONFIG_DIR".to_string(), "/home/me/.claude-work".to_string())]);
        let default = PaneLaunch::resume_with("s1", "/r".into(), None, Some(Path::new("/home/me/.claude")), &dirs, daemon);
        assert_eq!((default.program.as_str(), default.env.is_empty()), ("claude", true));
    }

    #[test]
    fn resume_unconfigured_dir_keeps_its_path() {
        let dirs = two_dirs();
        let launch = PaneLaunch::resume_with("s1", "/r".into(), None, Some(Path::new("/old/.claude-removed")), &dirs, Some(Path::new("/home/me/.claude")));
        assert_eq!((launch.program.as_str(), launch.args.clone()), ("claude", vec!["--resume".to_string(), "s1".to_string()]));
        assert_eq!(launch.env, vec![("CLAUDE_CONFIG_DIR".to_string(), "/old/.claude-removed".to_string())]);
    }

    #[test]
    fn old_layout_claude_flag_falls_back_to_name() {
        let json = r#"{"program":"claude","args":["--resume","s"],"cwd":"/r"}"#;
        let launch: PaneLaunch = serde_json::from_str(json).unwrap();
        assert!(!launch.claude && launch.is_claude());
        assert_eq!(launch.session(), Some("s"));
        let wrapper: PaneLaunch = serde_json::from_str(r#"{"program":"claude-w","args":["--resume","s"],"cwd":"/r","claude":true}"#).unwrap();
        assert_eq!(wrapper.session(), Some("s"));
        let other: PaneLaunch = serde_json::from_str(r#"{"program":"claude-w","args":["--resume","s"],"cwd":"/r"}"#).unwrap();
        assert_eq!(other.session(), None);
    }

    #[test]
    fn shell_launch_program_and_args() {
        let launch = PaneLaunch::shell("/r".into(), &ShellSettings { program: "/bin/zsh".into(), args: vec!["-l".into()] });
        assert_eq!((launch.program.as_str(), launch.args.clone(), launch.is_claude()), ("/bin/zsh", vec!["-l".to_string()], false));
        let default = PaneLaunch::shell("/r".into(), &ShellSettings::default());
        assert_eq!(default.program, "");
        assert_eq!(default.label(), "shell · r");
    }

    #[test]
    fn restore_without_layout_drops_dead_leaves_keeps_live() {
        let mut saved = Workspace { instance: "i1".into(), ..Workspace::default() };
        let a = saved.new_tab(shell("/a"));
        saved.pane_started(a, 1, "/a".into());
        let b = saved.split(Axis::Vertical, shell("/b")).unwrap();
        saved.pane_started(b, 2, "/b".into());
        let c = saved.new_tab(shell("/c"));
        saved.pane_started(c, 3, "/c".into());
        let restored = restore(Some(saved), "i1", &[info(1, "bash", 5)], &launch(), false);
        assert_eq!(restored.attach, vec![(a, 1)]);
        assert_eq!(restored.workspace.leaves.keys().copied().collect::<Vec<_>>(), vec![a]);
        assert_eq!(restored.workspace.tabs.len(), 1);
        assert!(restored.spawn.is_empty() && restored.workspace.is_consistent());
        let none = restore(Some(Workspace { instance: "i1".into(), ..Workspace::default() }), "i1", &[], &launch(), false);
        assert_eq!(none.spawn.len(), 1, "no tab left: one new tab");
    }

    #[test]
    fn rerun_keeps_dir_and_env() {
        let dirs = two_dirs();
        let launch = PaneLaunch::claude_with("/r".into(), &dirs.dirs[1], Some(Path::new("/home/me/.claude")), None);
        let mut leaf = Leaf::new(launch.clone());
        leaf.session = Some("sid".into());
        let rerun = leaf.rerun(true);
        assert_eq!(rerun.args, ["--model", "opus", "--resume", "sid"]);
        assert_eq!((rerun.program, rerun.env, rerun.claude_dir, rerun.claude), (launch.program, launch.env, launch.claude_dir, true));
        assert_eq!(leaf.rerun(false).args, ["--model", "opus", "--session-id", "sid"]);
    }

    #[test]
    fn requests_carry_flag_env_and_scrollback() {
        let dirs = two_dirs();
        let launch = PaneLaunch::claude_with("/r".into(), &dirs.dirs[1], None, None);
        let request = launch.request(None, 80, 24, 2_000);
        assert_eq!((request.claude, request.scrollback, request.env.len()), (true, Some(2_000), 1));
    }
}
