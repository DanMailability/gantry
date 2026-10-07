//! The GUI's workspace: tabs of split panes, what each pane runs, and reconciling a saved layout
//! with the daemon's live panes at startup.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use protocol::host::{HostId, PaneKey};
use protocol::message::{PaneId, PaneInfo, SessionState, SessionStatus, SpawnRequest, claude_session};
use serde::{Deserialize, Serialize};

use crate::attention::most_urgent;
use crate::sessions::SessionStore;
use crate::settings::{Settings, ShellSettings, expand_home};
use crate::split_tree::{Axis, Direction, Node, NodeId};
use crate::theme_colors::ProjectColor;

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
    /// Picked badge color.
    pub color: Option<ProjectColor>,
}

/// A config dir's mark on sessions, tabs and pane headers: the first letter of its name on its color.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirBadge {
    /// The dir's name.
    pub name: String,
    /// Its first letter, upper-cased.
    pub letter: String,
    /// The picked color, else the automatic color of its path.
    pub color: ProjectColor,
}

impl ResolvedDir {
    /// Its badge.
    pub fn badge(&self) -> DirBadge {
        let letter = self.name.chars().next().map(|first| first.to_uppercase().collect()).unwrap_or_default();
        DirBadge {
            name: self.name.clone(),
            letter,
            color: self.color.unwrap_or_else(|| ProjectColor::auto(&self.path)),
        }
    }
}

impl ClaudeDirs {
    /// The dirs of `settings`; with none configured, the implicit "Default" at `daemon_dir` (else `<home>/.claude`).
    pub fn from_settings(settings: &Settings, daemon_dir: Option<&Path>, home: Option<&Path>) -> ClaudeDirs {
        if settings.claude_dirs.is_empty() {
            let path =
                daemon_dir.map(Path::to_path_buf).or_else(|| home.map(|home| home.join(".claude"))).unwrap_or_default();
            return ClaudeDirs {
                dirs: vec![ResolvedDir {
                    name: "Default".into(),
                    path,
                    command: CLAUDE.into(),
                    args: Vec::new(),
                    color: None,
                }],
                default: 0,
            };
        }
        let dirs: Vec<ResolvedDir> = settings
            .claude_dirs
            .iter()
            .map(|dir| ResolvedDir {
                name: dir.name.clone(),
                path: expand_home(&dir.path, home),
                command: dir.command.clone(),
                args: dir.args.clone(),
                color: dir.color,
            })
            .collect();
        let default = settings
            .default_claude_dir
            .as_deref()
            .and_then(|name| dirs.iter().position(|dir| dir.name == name))
            .unwrap_or(0);
        ClaudeDirs { dirs, default }
    }

    /// The dir for a new session in `folder`: the explicit `pick` (a name), else the one remembered for exactly
    /// this folder, else the default. Names that match no dir are skipped.
    pub fn for_folder(&self, folder: &Path, pick: Option<&str>, memory: &BTreeMap<PathBuf, String>) -> &ResolvedDir {
        let named = |name: &str| self.dirs.iter().find(|dir| dir.name == name);
        pick.and_then(named)
            .or_else(|| memory.get(folder).and_then(|name| named(name)))
            .unwrap_or(&self.dirs[self.default])
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
    pub fn claude_with(
        cwd: PathBuf,
        dir: &ResolvedDir,
        daemon_dir: Option<&Path>,
        container: Option<PathBuf>,
    ) -> PaneLaunch {
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
        Self::session_with("--resume", session, cwd, container, session_dir, dirs, daemon_dir)
    }

    /// Like `resume_with`, for a session with no transcript yet: starts it afresh under the same id (`--session-id`).
    pub fn restart_with(
        session: &str,
        cwd: PathBuf,
        container: Option<PathBuf>,
        session_dir: Option<&Path>,
        dirs: &ClaudeDirs,
        daemon_dir: Option<&Path>,
    ) -> PaneLaunch {
        Self::session_with("--session-id", session, cwd, container, session_dir, dirs, daemon_dir)
    }

    fn session_with(
        flag: &str,
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
        args.extend([flag.into(), session.into()]);
        let recorded =
            ResolvedDir { path: session_dir.map_or_else(|| dir.path.clone(), Path::to_path_buf), ..dir.clone() };
        Self::claude_in(cwd, args, &recorded, daemon_dir, container)
    }

    fn claude_in(
        cwd: PathBuf,
        args: Vec<String>,
        dir: &ResolvedDir,
        daemon_dir: Option<&Path>,
        container: Option<PathBuf>,
    ) -> PaneLaunch {
        if container.is_some() {
            return PaneLaunch {
                program: CLAUDE.into(),
                args,
                cwd,
                container,
                claude: true,
                claude_dir: None,
                env: Vec::new(),
            };
        }
        let own = daemon_dir == Some(dir.path.as_path());
        PaneLaunch {
            program: dir.command.clone(),
            args,
            cwd,
            container: None,
            claude: true,
            claude_dir: (!own).then(|| dir.path.clone()),
            env: if own {
                Vec::new()
            } else {
                vec![("CLAUDE_CONFIG_DIR".into(), dir.path.to_string_lossy().into_owned())]
            },
        }
    }

    /// A shell in `cwd`; an empty `shell.program` is the daemon's default shell.
    pub fn shell(cwd: PathBuf, shell: &ShellSettings) -> PaneLaunch {
        PaneLaunch {
            program: shell.program.clone(),
            args: shell.args.clone(),
            cwd,
            container: None,
            claude: false,
            claude_dir: None,
            env: Vec::new(),
        }
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
        let program = name(Path::new(&self.program))
            .unwrap_or_else(|| if self.program.is_empty() { "shell".into() } else { self.program.clone() });
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
    /// The host whose daemon runs (ran) it; `local` unless it is on an SSH host.
    #[serde(default, skip_serializing_if = "HostId::is_local")]
    pub host: HostId,
    /// What it runs.
    pub launch: PaneLaunch,
    /// Claude session it last ran (followed across `/clear`); `None` for other programs.
    #[serde(default)]
    pub session: Option<String>,
    /// Name the user gave a shell pane; `None` = the pane's own title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Leaf {
    /// A leaf about to run `launch`.
    pub fn new(launch: PaneLaunch) -> Leaf {
        let session = launch.session().map(String::from);
        Leaf { pane: None, host: HostId::local(), launch, session, name: None }
    }

    /// The pane's host and id, while it runs.
    pub fn key(&self) -> Option<PaneKey> {
        Some(PaneKey::new(self.host.clone(), self.pane?))
    }

    /// What Continue / Restart does, given the daemon's sessions: focus the pane that already runs the
    /// leaf's session, refuse while it runs outside Gantry, else start `rerun` — resuming when the session is
    /// known, or when the list has not arrived yet.
    pub fn rerun_with(&self, sessions: &SessionStore, dirs: &ClaudeDirs, daemon_dir: Option<&Path>) -> Rerun {
        let known = self.session.as_deref().and_then(|id| sessions.get(&self.host, id));
        match known.map(|session| session.state) {
            Some(SessionState::Managed { pane }) => Rerun::Focus(PaneKey::new(self.host.clone(), pane)),
            Some(SessionState::External { .. }) => Rerun::Busy,
            _ => Rerun::Start(self.rerun_resolved(
                known.is_some() || !sessions.loaded(&self.host),
                known.and_then(|info| info.claude_dir.as_deref()),
                dirs,
                daemon_dir,
            )),
        }
    }

    /// `rerun`, but a Claude leaf with a session is launched through its config dir's current command, arguments and
    /// environment: `session_dir` (where the session was recorded), else the dir the leaf ran with.
    pub fn rerun_resolved(
        &self,
        resumable: bool,
        session_dir: Option<&Path>,
        dirs: &ClaudeDirs,
        daemon_dir: Option<&Path>,
    ) -> PaneLaunch {
        let launch = &self.launch;
        let Some(id) = self.session.as_deref().filter(|_| launch.is_claude()) else { return self.rerun(resumable) };
        let session_dir = session_dir.or(launch.claude_dir.as_deref());
        let (cwd, container) = (launch.cwd.clone(), launch.container.clone());
        if resumable {
            PaneLaunch::resume_with(id, cwd, container, session_dir, dirs, daemon_dir)
        } else {
            PaneLaunch::restart_with(id, cwd, container, session_dir, dirs, daemon_dir)
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
        let own = launch
            .args
            .iter()
            .position(|arg| matches!(arg.as_str(), "--session-id" | "--resume" | "-r" | "--continue"))
            .unwrap_or(0);
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
    Focus(PaneKey),
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
    /// Name the user gave the tab; `None` = the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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

/// Most panes one tab may hold; further splits are refused.
pub const MAX_PANES_PER_TAB: usize = 4;

/// All tabs of the window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    /// The daemon instance each host's pane ids belong to.
    #[serde(default)]
    pub instances: BTreeMap<HostId, String>,
    /// Tabs, left to right.
    pub tabs: Vec<Tab>,
    /// Index of the visible tab.
    pub active: usize,
    /// Every leaf by id.
    pub leaves: BTreeMap<NodeId, Leaf>,
    /// Sidebar visibility and width.
    #[serde(default)]
    pub sidebar: SidebarState,
    /// Finished tabs and leaves that "Start with one new tab" dropped at startup; written back into every saved copy
    /// so that switching to "Restore tabs and panes" brings them back.
    #[serde(skip)]
    dropped: Vec<(Tab, Vec<(NodeId, Leaf)>)>,
    next_id: NodeId,
}

impl Workspace {
    fn alloc(&mut self) -> NodeId {
        self.next_id += 1;
        self.next_id
    }

    /// Remember which daemon instance `host`'s pane ids belong to.
    pub fn set_instance(&mut self, host: &HostId, instance: &str) {
        self.instances.insert(host.clone(), instance.into());
    }

    /// Open `launch` in a new tab (before the Settings tab, which stays last) and focus it. Returns the new leaf.
    pub fn new_tab(&mut self, launch: PaneLaunch) -> NodeId {
        let leaf = self.alloc();
        self.leaves.insert(leaf, Leaf::new(launch));
        let at = self.settings_tab().unwrap_or(self.tabs.len());
        self.tabs.insert(at, Tab { root: Node::Leaf(leaf), focused: leaf, settings: false, name: None });
        self.active = at;
        leaf
    }

    /// `new_tab`, running on `host`.
    pub fn new_tab_on(&mut self, launch: PaneLaunch, host: HostId) -> NodeId {
        let leaf = self.new_tab(launch);
        self.leaves.get_mut(&leaf).expect("the leaf was just created").host = host;
        leaf
    }

    /// Move `from`'s leaves and its daemon instance to the name `to` (a rename of an SSH host).
    pub fn rename_host(&mut self, from: &HostId, to: &HostId) {
        let leaves = self
            .leaves
            .values_mut()
            .chain(self.dropped.iter_mut().flat_map(|(_, leaves)| leaves.iter_mut().map(|(_, leaf)| leaf)));
        for leaf in leaves.filter(|leaf| leaf.host == *from) {
            leaf.host = to.clone();
        }
        if let Some(instance) = self.instances.remove(from) {
            self.instances.insert(to.clone(), instance);
        }
    }

    /// Drop every leaf of `host` (closing the tabs that empties) and its daemon instance. Returns the panes those
    /// leaves had.
    pub fn remove_host(&mut self, host: &HostId) -> Vec<PaneKey> {
        let doomed: Vec<NodeId> =
            self.leaves.iter().filter(|(_, leaf)| leaf.host == *host).map(|(id, _)| *id).collect();
        let mut panes = Vec::new();
        for id in doomed {
            if let Some(leaf) = self.close(id) {
                panes.extend(leaf.pane.map(|pane| PaneKey::new(host.clone(), pane)));
            }
        }
        for (_, leaves) in &mut self.dropped {
            leaves.retain(|(_, leaf)| leaf.host != *host);
        }
        self.instances.remove(host);
        panes
    }

    /// Name a shell pane (trimmed; empty clears it). False, changing nothing, for a missing or Claude leaf.
    pub fn rename_leaf(&mut self, leaf: NodeId, name: &str) -> bool {
        match self.leaves.get_mut(&leaf) {
            Some(found) if !found.launch.is_claude() => {
                found.name = Some(name.trim().to_string()).filter(|name| !name.is_empty());
                true
            }
            _ => false,
        }
    }

    /// Name a tab (trimmed; empty clears it). False, changing nothing, for a missing tab or the Settings tab.
    pub fn rename_tab(&mut self, index: usize, name: &str) -> bool {
        match self.tabs.get_mut(index) {
            Some(tab) if !tab.settings => {
                tab.name = Some(name.trim().to_string()).filter(|name| !name.is_empty());
                true
            }
            _ => false,
        }
    }

    /// The name the user gave a shell pane, if any (a Claude pane never shows one, even from a hand-edited layout).
    pub fn leaf_name(&self, leaf: NodeId) -> Option<&str> {
        self.leaves.get(&leaf).filter(|leaf| !leaf.launch.is_claude())?.name.as_deref()
    }

    /// The name the user gave a tab, if any.
    pub fn tab_name(&self, index: usize) -> Option<&str> {
        self.tabs.get(index)?.name.as_deref()
    }

    /// Show the Settings tab, opening it as the last tab if it is not open. Returns its index.
    pub fn open_settings(&mut self) -> usize {
        let index = match self.settings_tab() {
            Some(index) => index,
            None => {
                let id = self.alloc();
                self.tabs.push(Tab { root: Node::Leaf(id), focused: id, settings: true, name: None });
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

    /// A copy without the Settings tab, and with the tabs dropped at startup appended, for saving.
    pub fn saveable(&self) -> Workspace {
        let mut saved = self.clone();
        saved.close_settings();
        for (tab, leaves) in std::mem::take(&mut saved.dropped) {
            saved.tabs.push(tab);
            saved.leaves.extend(leaves);
        }
        saved
    }

    /// Whether the active tab can take another pane: it is not the Settings tab and holds fewer than
    /// `MAX_PANES_PER_TAB` panes.
    pub fn can_split(&self) -> bool {
        self.tabs.get(self.active).is_some_and(|tab| !tab.settings && tab.root.leaves().len() < MAX_PANES_PER_TAB)
    }

    /// Split the focused leaf, putting `launch` second (on this machine), and focus it. `None` when there are no
    /// tabs or the tab is full.
    pub fn split(&mut self, axis: Axis, launch: PaneLaunch) -> Option<NodeId> {
        self.split_on(axis, launch, HostId::local())
    }

    /// Like `split`, with the new leaf on `host`.
    pub fn split_on(&mut self, axis: Axis, launch: PaneLaunch, host: HostId) -> Option<NodeId> {
        if !self.can_split() {
            return None;
        }
        let target = self.focused()?;
        let (split_id, leaf) = (self.alloc(), self.alloc());
        let tab = &mut self.tabs[self.active];
        tab.root.split(target, axis, split_id, leaf);
        tab.focused = leaf;
        self.leaves.insert(leaf, Leaf { host, ..Leaf::new(launch) });
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

    /// The tabs to keep aside for `dead` leaves: a tab whose leaves are all dead whole, a dead leaf of a tab with live
    /// ones as a tab of its own.
    fn park(&self, dead: &[NodeId]) -> Vec<(Tab, Vec<(NodeId, Leaf)>)> {
        let mut parked = Vec::new();
        for tab in self.tabs.iter().filter(|tab| !tab.settings) {
            let leaves = tab.root.leaves();
            let (gone, kept): (Vec<NodeId>, Vec<NodeId>) = leaves.into_iter().partition(|leaf| dead.contains(leaf));
            let pair = |id: NodeId| (id, self.leaves[&id].clone());
            if kept.is_empty() {
                parked.push((tab.clone(), gone.into_iter().map(pair).collect()));
            } else {
                parked.extend(gone.into_iter().map(|id| {
                    (Tab { root: Node::Leaf(id), focused: id, settings: false, name: None }, vec![pair(id)])
                }));
            }
        }
        parked
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

    /// Show the tab `delta` places away in `leaf_order` (the sidebar's leaves, top to bottom), wrapping around. The
    /// Settings tab comes first (its row is at the top), each tab counts once (at its first listed leaf), and tabs
    /// with no listed leaf follow in workspace order.
    pub fn cycle_tab_in(&mut self, leaf_order: &[NodeId], delta: isize) {
        let mut order: Vec<usize> = self.settings_tab().into_iter().collect();
        for index in leaf_order.iter().filter_map(|leaf| self.tab_of(*leaf)).chain(0..self.tabs.len()) {
            if !order.contains(&index) {
                order.push(index);
            }
        }
        let Some(at) = order.iter().position(|index| *index == self.active) else { return };
        let len = order.len() as isize;
        self.active = order[(at as isize + delta).rem_euclid(len) as usize];
    }

    /// Set a split's ratio in the visible tab.
    pub fn set_ratio(&mut self, split: NodeId, ratio: f32) {
        if let Some(tab) = self.tabs.get_mut(self.active).filter(|tab| !tab.settings) {
            tab.root.set_ratio(split, ratio);
        }
    }

    /// Leaf showing pane `key`.
    pub fn leaf_for_pane(&self, key: &PaneKey) -> Option<NodeId> {
        self.leaves.iter().find(|(_, leaf)| leaf.host == key.host && leaf.pane == Some(key.pane)).map(|(id, _)| *id)
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
        let session = sessions.get(&leaf.host, leaf.session.as_deref()?)?;
        if session.state != (SessionState::Managed { pane }) {
            return None;
        }
        session.status.as_ref()
    }

    /// The newest prompt of the session `leaf`'s pane runs, if it has one.
    pub fn leaf_last_prompt<'s>(&self, leaf: NodeId, sessions: &'s SessionStore) -> Option<&'s str> {
        let leaf = self.leaves.get(&leaf)?;
        let pane = leaf.pane?;
        let session = sessions.get(&leaf.host, leaf.session.as_deref()?)?;
        if session.state != (SessionState::Managed { pane }) {
            return None;
        }
        session.last_prompt.as_deref().filter(|prompt| !prompt.is_empty())
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

    /// Leaves without a running pane (they show Continue / Restart), tab by tab in screen order.
    pub fn finished_leaves(&self) -> Vec<NodeId> {
        let tabs = self.tabs.iter().flat_map(|tab| tab.root.leaves());
        tabs.filter(|id| self.leaves.get(id).is_some_and(|leaf| leaf.pane.is_none())).collect()
    }

    /// `leaf`'s process started as `pane` in `cwd`.
    pub fn pane_started(&mut self, leaf: NodeId, pane: PaneId, cwd: PathBuf) {
        if let Some(leaf) = self.leaves.get_mut(&leaf) {
            leaf.pane = Some(pane);
            leaf.launch.cwd = cwd;
        }
    }

    /// `key`'s process ended; returns the leaf that showed it.
    pub fn pane_exited(&mut self, key: &PaneKey) -> Option<NodeId> {
        let leaf = self.leaf_for_pane(key)?;
        self.leaves.get_mut(&leaf)?.pane = None;
        Some(leaf)
    }

    /// The daemon reports that pane `key` runs `session`; returns whether the leaf's session changed.
    pub fn session_seen(&mut self, key: &PaneKey, session: &str) -> bool {
        let Some(leaf) = self.leaf_for_pane(key).and_then(|leaf| self.leaves.get_mut(&leaf)) else { return false };
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

/// One connected host's daemon, as `restore` sees it.
pub struct HostLive<'a> {
    /// The host.
    pub host: HostId,
    /// Its daemon's instance id (in `Welcome`).
    pub instance: &'a str,
    /// Its live panes, most recently focused first.
    pub panes: &'a [PaneInfo],
}

/// A saved layout reconciled with the daemons' live panes.
pub struct Restored {
    /// The workspace to show.
    pub workspace: Workspace,
    /// Leaves to attach to their (live) pane.
    pub attach: Vec<(NodeId, PaneKey)>,
    /// Leaves whose process must be started.
    pub spawn: Vec<NodeId>,
}

/// Match one host's leaves with its daemon's live panes, when the host connects (at start, or again after a
/// reconnect): leaves keep their pane while it is the same daemon instance and the pane is still live, the others
/// lose it (they show Continue / Restart); live panes no leaf shows become new tabs on that host (most recently
/// focused last). The host's instance is recorded. Returns the leaves to attach, with their panes.
pub fn reconcile(workspace: &mut Workspace, live: &HostLive<'_>) -> Vec<(NodeId, PaneKey)> {
    let same_instance = workspace.instances.get(&live.host).map(String::as_str) == Some(live.instance);
    let live_panes: HashSet<PaneId> = live.panes.iter().map(|info| info.pane).collect();
    for leaf in workspace.leaves.values_mut().filter(|leaf| leaf.host == live.host) {
        if !leaf.pane.is_some_and(|pane| same_instance && live_panes.contains(&pane)) {
            leaf.pane = None;
        }
    }
    workspace.set_instance(&live.host, live.instance);
    let mut attach: Vec<(NodeId, PaneKey)> = workspace
        .leaves
        .iter()
        .filter(|(_, leaf)| leaf.host == live.host)
        .filter_map(|(id, leaf)| Some((*id, leaf.key()?)))
        .collect();
    let known: HashSet<PaneId> = attach.iter().map(|(_, key)| key.pane).collect();
    let mut orphans: Vec<&PaneInfo> = live.panes.iter().filter(|info| !known.contains(&info.pane)).collect();
    orphans.sort_by_key(|info| (info.last_focused, info.pane));
    for info in orphans {
        let leaf = workspace.new_tab_on(
            PaneLaunch {
                program: info.program.clone(),
                args: info.args.clone(),
                cwd: info.cwd.clone(),
                container: info.container.clone(),
                claude: info.claude,
                claude_dir: None,
                env: Vec::new(),
            },
            live.host.clone(),
        );
        workspace.pane_started(leaf, info.pane, info.cwd.clone());
        attach.push((leaf, PaneKey::new(live.host.clone(), info.pane)));
    }
    attach
}

/// A pane closed while its host was not connected: the host, the daemon instance it belonged to, and its id.
pub type KilledOffline = (HostId, String, PaneId);

/// Take `host`'s entries out of `killed` and apply them to the live panes of the daemon instance `instance` that
/// just connected: returns the panes that are left to show, and the ones to kill now (closed while offline, still
/// running). Entries of another instance of the host mean nothing now and are dropped.
pub fn without_killed_offline(
    killed: &mut Vec<KilledOffline>,
    host: &HostId,
    instance: &str,
    panes: &[PaneInfo],
) -> (Vec<PaneInfo>, Vec<PaneId>) {
    let mut ours = HashSet::new();
    killed.retain(|(killed_host, killed_instance, pane)| {
        if killed_host != host {
            return true;
        }
        if killed_instance == instance {
            ours.insert(*pane);
        }
        false
    });
    let to_kill: Vec<PaneId> = panes.iter().map(|info| info.pane).filter(|pane| ours.contains(pane)).collect();
    let kept = panes.iter().filter(|info| !ours.contains(&info.pane)).cloned().collect();
    (kept, to_kill)
}

/// Rebuild the workspace at startup, host by host through `reconcile`. Leaves keep their live pane; leaves whose
/// pane is gone (or belongs to another daemon instance) become dead; leaves of a host that is not connected keep their
/// saved pane and instance until `reconcile` runs for it (and are never dropped, even without `restore_layout`); live panes
/// missing from the layout become new tabs (most recently focused last, and active); an explicit launch opens a new
/// focused tab; an empty result opens the default. Without `restore_layout`, leaves whose pane is gone are dropped
/// from the workspace (live panes still reattach) but kept aside for `saveable`; leaves in `keep` (the ones that
/// were running when an update stopped everything) stay in the workspace as finished leaves instead.
pub fn restore(
    saved: Option<Workspace>,
    hosts: &[HostLive<'_>],
    launch: &Launch,
    restore_layout: bool,
    keep: &HashSet<NodeId>,
) -> Restored {
    let mut workspace = saved.unwrap_or_default();
    // A host that is not connected keeps its leaves as saved (pane and instance): `reconcile` decides when it connects.
    let connected: HashSet<&HostId> = hosts.iter().map(|live| &live.host).collect();
    let mut attach = Vec::new();
    for live in hosts {
        attach.extend(reconcile(&mut workspace, live));
    }
    if !restore_layout {
        let dead: Vec<NodeId> = workspace
            .leaves
            .iter()
            .filter(|(id, leaf)| leaf.pane.is_none() && !keep.contains(id) && connected.contains(&leaf.host))
            .map(|(id, _)| *id)
            .collect();
        workspace.dropped = workspace.park(&dead);
        for leaf in dead {
            workspace.close(leaf);
        }
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
    use crate::settings::ClaudeDir;

    #[test]
    fn a_dirs_badge_is_its_upper_cased_first_letter_on_its_color() {
        let dir = |name: &str, color| ResolvedDir {
            name: name.into(),
            path: "/c/one".into(),
            command: "claude".into(),
            args: vec![],
            color,
        };
        assert_eq!(dir("work", None).badge().letter, "W");
        assert_eq!(dir("élan", None).badge().letter, "É");
        assert_eq!(
            dir("Private", Some(ProjectColor::Pink)).badge(),
            DirBadge { name: "Private".into(), letter: "P".into(), color: ProjectColor::Pink }
        );
    }

    #[test]
    fn an_unpicked_dir_color_follows_its_path_not_its_name() {
        let dir = |name: &str| ResolvedDir {
            name: name.into(),
            path: "/c/one".into(),
            command: "claude".into(),
            args: vec![],
            color: None,
        };
        assert_eq!(dir("Work").badge().color, ProjectColor::auto(Path::new("/c/one")));
        assert_eq!(dir("Work").badge().color, dir("Renamed").badge().color);
    }

    #[test]
    fn resolved_dirs_carry_the_picked_color() {
        let settings = Settings {
            claude_dirs: vec![
                ClaudeDir {
                    name: "A".into(),
                    path: "/a".into(),
                    command: "claude".into(),
                    args: vec![],
                    color: Some(ProjectColor::Lime),
                },
                ClaudeDir { name: "B".into(), path: "/b".into(), command: "claude".into(), args: vec![], color: None },
            ],
            ..Settings::default()
        };
        let dirs = ClaudeDirs::from_settings(&settings, None, None);
        assert_eq!(dirs.dirs[0].color, Some(ProjectColor::Lime));
        assert_eq!(dirs.dirs[1].color, None);
    }

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
        PaneLaunch {
            program: "/bin/bash".into(),
            args: vec![],
            cwd: cwd.into(),
            container: None,
            claude: false,
            claude_dir: None,
            env: Vec::new(),
        }
    }

    fn info(pane: PaneId, program: &str, last_focused: u64) -> PaneInfo {
        PaneInfo {
            pane,
            program: program.into(),
            args: vec!["-l".into()],
            cwd: "/srv".into(),
            last_focused,
            container: None,
            claude: false,
        }
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

    fn workspace_with_tabs(count: usize) -> Workspace {
        let mut ws = Workspace::default();
        for index in 0..count {
            ws.new_tab(shell(&format!("/t{index}")));
        }
        ws
    }

    #[test]
    fn new_tabs_go_before_settings() {
        let mut ws = workspace_with_tabs(2);
        ws.open_settings();
        ws.new_tab(shell("/new"));
        assert!(ws.tabs.last().unwrap().settings);
        assert_eq!(ws.active, ws.tabs.len() - 2, "the new tab is visible");
        assert!(ws.is_consistent());
    }

    #[test]
    fn cycle_follows_the_sidebar_order_with_settings_first() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        let b = ws.new_tab(shell("/b"));
        let c = ws.new_tab(shell("/c"));
        ws.open_settings();
        // The sidebar lists c, a, b.
        let order = [c, a, b];
        ws.active = ws.tab_of(c).unwrap();
        ws.cycle_tab_in(&order, 1);
        assert_eq!(ws.focused(), Some(a));
        ws.cycle_tab_in(&order, 1);
        assert_eq!(ws.focused(), Some(b));
        ws.cycle_tab_in(&order, 1);
        assert_eq!(ws.settings_tab(), Some(ws.active), "wraps to Settings, the top row");
        ws.cycle_tab_in(&order, 1);
        assert_eq!(ws.focused(), Some(c));
        ws.cycle_tab_in(&order, -1);
        assert_eq!(ws.settings_tab(), Some(ws.active));
        assert!(ws.is_consistent());
    }

    #[test]
    fn cycle_counts_a_split_tab_once_and_appends_unlisted_tabs() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        let a2 = ws.split(Axis::Horizontal, shell("/a2")).unwrap();
        let b = ws.new_tab(shell("/b"));
        let c = ws.new_tab(shell("/c"));
        // The sidebar lists the split tab's two leaves, then b; c has no row.
        let order = [a, a2, b];
        ws.active = ws.tab_of(a).unwrap();
        ws.cycle_tab_in(&order, 1);
        assert_eq!(ws.focused(), Some(b), "the split tab's second leaf is not a step");
        ws.cycle_tab_in(&order, 1);
        assert_eq!(ws.focused(), Some(c), "a tab with no row comes after the listed ones");
        ws.cycle_tab_in(&[], 1);
        assert_eq!(ws.active, 0, "an empty order falls back to workspace order");
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
    fn settings_tab_takes_part_in_select() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        let b = ws.new_tab(shell("/b"));
        ws.open_settings();
        assert_eq!(ws.tabs.iter().map(|t| t.focused).take(2).collect::<Vec<_>>(), vec![a, b]);
        assert!(ws.select_tab(1));
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
        let mut ws = workspace_on("i");
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
        assert_eq!((ws.leaf_for_pane(&PaneKey::local(1)), ws.leaf_for_session("x")), (None, None));
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
    fn select_tab_stops_at_the_last_tab() {
        let mut ws = Workspace::default();
        ws.new_tab(shell("/a"));
        ws.new_tab(shell("/b"));
        ws.new_tab(shell("/c"));
        assert!(ws.select_tab(0) && !ws.select_tab(3));
        assert_eq!(ws.active, 0);
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
        assert_eq!(
            (ws.leaf_for_pane(&PaneKey::local(7)), ws.leaves[&a].launch.cwd.clone()),
            (Some(a), PathBuf::from("/real"))
        );
        assert_eq!(ws.pane_exited(&PaneKey::local(7)), Some(a));
        assert_eq!(ws.leaves[&a].pane, None);
    }

    #[test]
    fn rename_leaf_trims_clears_and_refuses_claude() {
        let mut ws = Workspace::default();
        let shell_leaf = ws.new_tab(shell("/repo"));
        let claude = ws.split(Axis::Vertical, PaneLaunch::claude("/repo".into())).unwrap();
        assert!(ws.rename_leaf(shell_leaf, "  build box "));
        assert_eq!(ws.leaf_name(shell_leaf), Some("build box"));
        assert!(ws.rename_leaf(shell_leaf, "   "));
        assert_eq!(ws.leaf_name(shell_leaf), None);
        assert!(!ws.rename_leaf(claude, "nope"));
        assert_eq!(ws.leaf_name(claude), None);
        assert!(!ws.rename_leaf(999, "x"));
        ws.leaves.get_mut(&claude).unwrap().name = Some("hand edited".into());
        assert_eq!(ws.leaf_name(claude), None);
    }

    #[test]
    fn rename_tab_trims_clears_and_refuses_settings() {
        let mut ws = Workspace::default();
        ws.new_tab(shell("/repo"));
        assert!(ws.rename_tab(0, " Test cleanup "));
        assert_eq!(ws.tab_name(0), Some("Test cleanup"));
        assert!(ws.rename_tab(0, ""));
        assert_eq!(ws.tab_name(0), None);
        assert!(!ws.rename_tab(5, "x"));
        let settings = ws.open_settings();
        assert!(!ws.rename_tab(settings, "x"));
    }

    #[test]
    fn pane_exited_for_an_unknown_pane_is_ignored() {
        let mut ws = Workspace::default();
        ws.new_tab(shell("/a"));
        assert_eq!(ws.pane_exited(&PaneKey::local(42)), None);
    }

    fn claude_leaf(session: Option<&str>) -> Leaf {
        Leaf {
            pane: None,
            host: HostId::local(),
            launch: PaneLaunch {
                program: "claude".into(),
                args: vec![],
                cwd: "/r".into(),
                container: None,
                claude: false,
                claude_dir: None,
                env: Vec::new(),
            },
            session: session.map(Into::into),
            name: None,
        }
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
        assert_eq!(
            (launch.args.clone(), launch.session()),
            (vec!["--resume".to_string(), "abc".to_string()], Some("abc"))
        );
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
            container: None,
            claude_dir: None,
            last_prompt: None,
            host: protocol::host::HostId::local(),
        }
    }

    #[test]
    fn rerun_never_runs_a_session_twice() {
        use protocol::message::SessionState;
        let leaf = claude_leaf(Some("x"));
        let dirs = ClaudeDirs::from_settings(&Settings::default(), Some(Path::new("/d/.claude")), None);
        let daemon = Some(Path::new("/d/.claude"));
        let again = |resumable| leaf.rerun_resolved(resumable, None, &dirs, daemon);
        let mut store = SessionStore::default();
        // Before the first list: assume it may exist and resume.
        assert_eq!(leaf.rerun_with(&store, &dirs, daemon), Rerun::Start(again(true)));
        store.replace(&HostId::local(), vec![]);
        assert_eq!(leaf.rerun_with(&store, &dirs, daemon), Rerun::Start(again(false)));
        store.replace(&HostId::local(), vec![session_info("x", SessionState::Closed)]);
        assert_eq!(leaf.rerun_with(&store, &dirs, daemon), Rerun::Start(again(true)));
        store.replace(&HostId::local(), vec![session_info("x", SessionState::Managed { pane: 4 })]);
        assert_eq!(leaf.rerun_with(&store, &dirs, daemon), Rerun::Focus(PaneKey::local(4)));
        store.replace(&HostId::local(), vec![session_info("x", SessionState::External { pid: Some(9) })]);
        assert_eq!(leaf.rerun_with(&store, &dirs, daemon), Rerun::Busy);
        let sh = Leaf::new(shell("/r"));
        assert_eq!(sh.rerun_with(&store, &dirs, daemon), Rerun::Start(sh.launch.clone()));
    }

    // Unix-style paths: Windows joins them with `\`.
    #[cfg(unix)]
    #[test]
    fn continue_uses_the_dirs_current_command() {
        let daemon = Some(Path::new("/home/me/.claude"));
        let old = ClaudeDirs::from_settings(&two_dirs_settings(), daemon, Some(Path::new("/home/me")));
        let mut leaf = Leaf::new(PaneLaunch::claude_with("/r".into(), &old.dirs[1], daemon, None));
        leaf.session = Some("sid".into());
        let mut settings = two_dirs_settings();
        settings.claude_dirs[1].command = "claude-new".into();
        settings.claude_dirs[1].args = vec!["--model".into(), "sonnet".into()];
        let dirs = ClaudeDirs::from_settings(&settings, daemon, Some(Path::new("/home/me")));
        let mut store = SessionStore::default();
        store.replace(&HostId::local(), vec![]);
        let Rerun::Start(restart) = leaf.rerun_with(&store, &dirs, daemon) else { panic!("starts") };
        assert_eq!(
            (restart.program.as_str(), restart.args.clone()),
            ("claude-new", vec!["--model".to_string(), "sonnet".into(), "--session-id".into(), "sid".into()])
        );
        store.replace(&HostId::local(), vec![session_info("sid", protocol::message::SessionState::Closed)]);
        let Rerun::Start(resume) = leaf.rerun_with(&store, &dirs, daemon) else { panic!("starts") };
        assert_eq!(
            (resume.program.as_str(), resume.args.clone()),
            ("claude-new", vec!["--model".to_string(), "sonnet".into(), "--resume".into(), "sid".into()])
        );
        assert_eq!(resume.env, vec![("CLAUDE_CONFIG_DIR".to_string(), "/home/me/.claude-work".to_string())]);
    }

    #[test]
    fn continue_of_an_orphan_follows_the_sessions_tagged_dir() {
        let daemon = Some(Path::new("/home/me/.claude"));
        let dirs = ClaudeDirs::from_settings(&two_dirs_settings(), daemon, Some(Path::new("/home/me")));
        let mut leaf = claude_leaf(Some("sid"));
        assert!(leaf.launch.env.is_empty() && leaf.launch.claude_dir.is_none());
        let mut info = session_info("sid", protocol::message::SessionState::Closed);
        info.claude_dir = Some("/home/me/.claude-work".into());
        let mut store = SessionStore::default();
        store.replace(&HostId::local(), vec![info]);
        let Rerun::Start(launch) = leaf.rerun_with(&store, &dirs, daemon) else { panic!("starts") };
        assert_eq!(launch.env, vec![("CLAUDE_CONFIG_DIR".to_string(), "/home/me/.claude-work".to_string())]);
        assert_eq!(launch.claude_dir, Some(PathBuf::from("/home/me/.claude-work")));
        leaf.launch.claude_dir = Some("/home/me/.claude-work".into());
        store.replace(&HostId::local(), vec![]);
        let Rerun::Start(launch) = leaf.rerun_with(&store, &dirs, daemon) else { panic!("starts") };
        assert_eq!(launch.env.len(), 1, "falls back to the leaf's own dir when the session is unknown");
    }

    #[test]
    fn a_leaf_shows_the_last_prompt_of_the_session_its_pane_runs() {
        use protocol::message::{SessionInfo, SessionState};
        let mut ws = Workspace::default();
        let a = ws.new_tab(PaneLaunch::resume("a", "/r".into(), None));
        ws.pane_started(a, 1, "/r".into());
        let sh = ws.new_tab(shell("/r"));
        ws.pane_started(sh, 2, "/r".into());
        let with = |pane, prompt: Option<&str>| SessionInfo {
            last_prompt: prompt.map(Into::into),
            host: protocol::host::HostId::local(),
            ..session_info("a", SessionState::Managed { pane })
        };
        let mut store = SessionStore::default();
        store.replace(&HostId::local(), vec![with(1, Some("what next"))]);
        assert_eq!(ws.leaf_last_prompt(a, &store), Some("what next"));
        assert_eq!(ws.leaf_last_prompt(sh, &store), None, "a shell has no session");
        store.replace(&HostId::local(), vec![with(1, None)]);
        assert_eq!(ws.leaf_last_prompt(a, &store), None, "nothing yet");
        store.replace(&HostId::local(), vec![with(1, Some(""))]);
        assert_eq!(ws.leaf_last_prompt(a, &store), None, "empty text is nothing");
        store.replace(&HostId::local(), vec![with(9, Some("old"))]);
        assert_eq!(ws.leaf_last_prompt(a, &store), None, "the session moved to another pane");
        store.replace(
            &HostId::local(),
            vec![SessionInfo { last_prompt: Some("old".into()), ..session_info("a", SessionState::Closed) }],
        );
        assert_eq!(ws.leaf_last_prompt(a, &store), None, "an ended pane shows Continue, not a prompt");
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
        let with = |id: &str, pane, status| SessionInfo {
            status: Some(status),
            ..session_info(id, SessionState::Managed { pane })
        };
        store.replace(
            &HostId::local(),
            vec![with("a", 1, SessionStatus::Idle), with("b", 2, SessionStatus::NeedsInput { reason: None })],
        );
        assert_eq!(ws.leaf_status(a, &store), Some(&SessionStatus::Idle));
        assert_eq!(ws.tab_status(0, &store), Some(&SessionStatus::NeedsInput { reason: None }));
        assert_eq!((ws.leaf_status(other, &store), ws.tab_status(1, &store)), (None, None));
        // A stale entry for another pane (the leaf's process ended, the session moved) is not this leaf's status.
        store.replace(&HostId::local(), vec![with("a", 9, SessionStatus::Busy)]);
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
        assert!(ws.session_seen(&PaneKey::local(5), "after-clear"));
        assert!(!ws.session_seen(&PaneKey::local(5), "after-clear"));
        assert!(!ws.session_seen(&PaneKey::local(99), "x"));
        assert_eq!(ws.leaves[&leaf].session.as_deref(), Some("after-clear"));
        assert_eq!(ws.leaves[&leaf].rerun(true).args, vec!["--resume".to_string(), "after-clear".to_string()]);
    }

    #[test]
    fn restore_adopted_claude_panes_know_their_session() {
        let live = [PaneInfo {
            pane: 4,
            program: "claude".into(),
            args: vec!["--resume".into(), "xyz".into()],
            cwd: "/srv".into(),
            last_focused: 1,
            container: None,
            claude: false,
        }];
        let restored = restore_local(None, "i", &live, &launch(), true, &HashSet::new());
        let leaf = restored.attach[0].0;
        assert_eq!(restored.workspace.leaves[&leaf].session.as_deref(), Some("xyz"));
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
        assert_eq!(
            (request.cwd_from, request.cols, request.rows, request.program.as_str()),
            (Some(3), 100, 30, "/bin/bash")
        );
    }

    fn launch() -> Launch {
        Launch { explicit: None, default: shell("/home") }
    }

    fn workspace_on(instance: &str) -> Workspace {
        let mut ws = Workspace::default();
        ws.set_instance(&HostId::local(), instance);
        ws
    }

    fn on_host(ws: &mut Workspace, leaf: NodeId, host: &str) {
        ws.leaves.get_mut(&leaf).unwrap().host = HostId::named(host);
    }

    fn restore_local(
        saved: Option<Workspace>,
        instance: &str,
        live: &[PaneInfo],
        launch: &Launch,
        restore_layout: bool,
        keep: &HashSet<NodeId>,
    ) -> Restored {
        let hosts = [HostLive { host: HostId::local(), instance, panes: live }];
        restore(saved, &hosts, launch, restore_layout, keep)
    }

    #[test]
    fn leaves_of_two_hosts_with_the_same_pane_id_stay_apart() {
        let mut ws = Workspace::default();
        let local = ws.new_tab(shell("/a"));
        let remote = ws.new_tab(shell("/b"));
        on_host(&mut ws, remote, "devbox");
        ws.pane_started(local, 1, "/a".into());
        ws.pane_started(remote, 1, "/b".into());
        let devbox = |pane| PaneKey::new(HostId::named("devbox"), pane);
        assert_eq!(ws.leaf_for_pane(&PaneKey::local(1)), Some(local));
        assert_eq!(ws.leaf_for_pane(&devbox(1)), Some(remote));
        assert_eq!(ws.pane_exited(&PaneKey::local(1)), Some(local));
        assert_eq!(ws.leaves[&remote].pane, Some(1), "the other host's pane 1 is untouched");
        assert_eq!(ws.leaves[&remote].key(), Some(devbox(1)));
        ws.leaves.get_mut(&remote).unwrap().session = None;
        assert!(ws.session_seen(&devbox(1), "s"));
        assert_eq!(ws.leaves[&local].session, None, "a session seen on one host never lands on the other's leaf");
    }

    #[test]
    fn splitting_a_pane_with_its_host_runs_there() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        on_host(&mut ws, a, "devbox");
        let b = ws.split_on(Axis::Vertical, shell("/b"), HostId::named("devbox")).unwrap();
        assert_eq!(ws.leaves[&b].host, HostId::named("devbox"));
        let c = ws.new_tab(shell("/c"));
        assert!(ws.leaves[&c].host.is_local(), "a new tab opens on this machine");
    }

    #[test]
    fn a_split_is_local_unless_given_a_host() {
        let mut ws = Workspace::default();
        let a = ws.new_tab(shell("/a"));
        on_host(&mut ws, a, "devbox");
        let b = ws.split(Axis::Vertical, shell("/b")).unwrap();
        assert!(ws.leaves[&b].host.is_local());
    }

    #[test]
    fn restore_decides_per_host_which_leaves_are_still_live() {
        let mut saved = workspace_on("i1");
        saved.set_instance(&HostId::named("devbox"), "r1");
        let local = saved.new_tab(shell("/a"));
        saved.pane_started(local, 1, "/a".into());
        let remote = saved.new_tab(shell("/b"));
        on_host(&mut saved, remote, "devbox");
        saved.pane_started(remote, 1, "/b".into());
        // The local daemon is the same instance; devbox restarted (new instance) and has a pane 1 of its own.
        let devbox_panes = [info(1, "zsh", 9)];
        let hosts = [
            HostLive { host: HostId::local(), instance: "i1", panes: &[info(1, "bash", 5)] },
            HostLive { host: HostId::named("devbox"), instance: "r2", panes: &devbox_panes },
        ];
        let restored = restore(Some(saved), &hosts, &launch(), true, &HashSet::new());
        let ws = &restored.workspace;
        assert_eq!(ws.leaves[&local].pane, Some(1), "the local leaf keeps its live pane");
        assert_eq!(ws.leaves[&remote].pane, None, "devbox restarted: its old pane ids mean nothing");
        assert_eq!(ws.instances[&HostId::named("devbox")], "r2");
        let adopted: Vec<_> = restored.attach.iter().filter(|(_, key)| !key.host.is_local()).collect();
        assert_eq!(adopted.len(), 1, "devbox's live pane 1 is adopted as a tab of its own");
        assert_eq!(ws.leaves[&adopted[0].0].host, HostId::named("devbox"));
        assert_ne!(adopted[0].0, remote);
    }

    fn saved_with_a_remote_leaf() -> (Workspace, NodeId) {
        let mut saved = workspace_on("i1");
        saved.set_instance(&HostId::named("devbox"), "r1");
        let remote = saved.new_tab(shell("/b"));
        on_host(&mut saved, remote, "devbox");
        saved.pane_started(remote, 3, "/b".into());
        (saved, remote)
    }

    #[test]
    fn restore_without_a_connection_keeps_a_hosts_leaves_and_their_panes_for_when_it_connects() {
        let (saved, remote) = saved_with_a_remote_leaf();
        let restored = restore_local(Some(saved), "i1", &[], &launch(), true, &HashSet::new());
        let ws = &restored.workspace;
        assert_eq!(ws.leaves[&remote].pane, Some(3), "the remote pane may still run: reconcile decides");
        assert!(restored.spawn.is_empty(), "nothing is started in place of a remote leaf");
        assert!(restored.attach.is_empty(), "nothing is attached to a host that is not connected");
        assert_eq!(ws.instances[&HostId::named("devbox")], "r1", "kept for when the host is back");
        assert!(ws.finished_leaves().is_empty(), "a remote leaf waiting for its host is not finished");
    }

    #[test]
    fn a_pane_closed_while_offline_is_killed_and_left_out_when_the_host_connects() {
        let (devbox, lab) = (HostId::named("devbox"), HostId::named("lab"));
        let mut killed = vec![
            (devbox.clone(), "r1".to_string(), 3),
            (devbox.clone(), "r0".to_string(), 4),
            (lab.clone(), "l1".to_string(), 3),
        ];
        let panes = [info(3, "bash", 1), info(4, "bash", 2), info(5, "bash", 3)];
        let (kept, to_kill) = without_killed_offline(&mut killed, &devbox, "r1", &panes);
        assert_eq!(kept.iter().map(|info| info.pane).collect::<Vec<_>>(), vec![4, 5], "only the same instance counts");
        assert_eq!(to_kill, vec![3]);
        assert_eq!(killed, vec![(lab, "l1".to_string(), 3)], "the host's entries are used up, the other host's stay");
    }

    #[test]
    fn restore_then_reconcile_with_the_same_instance_reattaches_the_same_leaf() {
        let (saved, remote) = saved_with_a_remote_leaf();
        let mut ws = restore_local(Some(saved), "i1", &[], &launch(), true, &HashSet::new()).workspace;
        let tabs = ws.tabs.len();
        let panes = [info(3, "bash", 5)];
        let devbox = HostId::named("devbox");
        let attach = reconcile(&mut ws, &HostLive { host: devbox.clone(), instance: "r1", panes: &panes });
        assert_eq!(attach, vec![(remote, PaneKey::new(devbox, 3))]);
        assert_eq!(ws.tabs.len(), tabs, "no duplicate tab");
    }

    #[test]
    fn restore_then_reconcile_with_a_new_instance_makes_the_leaf_dead_and_adopts_the_live_panes() {
        let (saved, remote) = saved_with_a_remote_leaf();
        let mut ws = restore_local(Some(saved), "i1", &[], &launch(), true, &HashSet::new()).workspace;
        let panes = [info(3, "zsh", 5)];
        let devbox = HostId::named("devbox");
        let attach = reconcile(&mut ws, &HostLive { host: devbox, instance: "r2", panes: &panes });
        assert_eq!(ws.leaves[&remote].pane, None);
        assert_eq!(ws.finished_leaves(), vec![remote]);
        assert_eq!(attach.len(), 1);
        assert_ne!(attach[0].0, remote, "the live pane became a tab of its own");
    }

    #[test]
    fn restore_without_the_layout_keeps_the_leaves_of_hosts_that_are_not_connected() {
        let (mut saved, remote) = saved_with_a_remote_leaf();
        let dead = saved.new_tab(shell("/c"));
        on_host(&mut saved, dead, "devbox");
        let restored = restore_local(Some(saved), "i1", &[], &launch(), false, &HashSet::new());
        let ws = &restored.workspace;
        assert!(ws.leaves.contains_key(&remote), "a live-looking remote leaf stays");
        assert!(ws.leaves.contains_key(&dead), "so does a finished one: its host is simply not connected yet");
        assert!(ws.is_consistent());
    }

    #[test]
    fn restore_with_nothing_spawns_the_default() {
        let restored = restore_local(None, "i1", &[], &launch(), true, &HashSet::new());
        assert_eq!(restored.workspace.tabs.len(), 1);
        assert_eq!(restored.spawn, vec![restored.workspace.focused().unwrap()]);
        assert!(restored.attach.is_empty());
        assert_eq!(restored.workspace.instances[&HostId::local()], "i1");
    }

    #[test]
    fn restore_reattaches_live_panes_and_marks_missing_ones_dead() {
        let mut saved = workspace_on("i1");
        let a = saved.new_tab(shell("/a"));
        saved.pane_started(a, 1, "/a".into());
        let b = saved.split(Axis::Vertical, shell("/b")).unwrap();
        saved.pane_started(b, 2, "/b".into());
        let restored = restore_local(Some(saved), "i1", &[info(1, "bash", 5)], &launch(), true, &HashSet::new());
        assert_eq!(restored.attach, vec![(a, PaneKey::local(1))]);
        assert_eq!(restored.workspace.leaves[&b].pane, None, "pane 2 is gone");
        assert!(restored.spawn.is_empty(), "dead panes wait for Continue/Restart");
        assert_eq!(restored.workspace.tabs.len(), 1);
    }

    #[test]
    fn finished_leaves_are_the_ones_without_a_pane_in_screen_order() {
        let mut ws = workspace_on("i1");
        let a = ws.new_tab(shell("/a"));
        let b = ws.split(Axis::Vertical, shell("/b")).unwrap();
        let c = ws.new_tab(shell("/c"));
        ws.pane_started(b, 2, "/b".into());
        assert_eq!(ws.finished_leaves(), vec![a, c]);
    }

    #[test]
    fn restore_from_another_daemon_instance_attaches_nothing() {
        let mut saved = workspace_on("old");
        let a = saved.new_tab(shell("/a"));
        saved.pane_started(a, 1, "/a".into());
        let restored = restore_local(Some(saved), "new", &[info(1, "zsh", 1)], &launch(), true, &HashSet::new());
        assert_eq!(restored.workspace.leaves[&a].pane, None);
        assert_eq!(restored.workspace.tabs.len(), 2, "the new daemon's pane 1 is adopted as its own tab");
        let (adopted, pane) = restored.attach[0].clone();
        assert_ne!(adopted, a);
        assert_eq!((pane.pane, restored.workspace.leaves[&adopted].launch.program.as_str()), (1, "zsh"));
    }

    #[test]
    fn restore_adopts_live_panes_missing_from_the_layout_most_recent_last() {
        let restored = restore_local(None, "i", &[info(5, "a", 9), info(4, "b", 2)], &launch(), true, &HashSet::new());
        let ws = &restored.workspace;
        assert_eq!(restored.attach.iter().map(|(_, key)| key.pane).collect::<Vec<_>>(), vec![4, 5]);
        assert_eq!(ws.leaves[&ws.focused().unwrap()].pane, Some(5));
        assert_eq!(ws.leaves[&ws.focused().unwrap()].launch.args, vec!["-l".to_string()]);
        assert!(restored.spawn.is_empty());
    }

    #[test]
    fn restore_with_an_explicit_launch_opens_it_in_a_new_focused_tab() {
        let mut saved = Workspace::default();
        saved.new_tab(shell("/a"));
        let launch = Launch { explicit: Some(PaneLaunch::claude("/x".into())), default: shell("/home") };
        let restored = restore_local(Some(saved), "i", &[], &launch, true, &HashSet::new());
        let focused = restored.workspace.focused().unwrap();
        assert_eq!(restored.spawn, vec![focused]);
        assert!(restored.workspace.leaves[&focused].launch.is_claude());
        assert_eq!(restored.workspace.tabs.len(), 2);
    }

    fn two_dirs_settings() -> Settings {
        use crate::settings::ClaudeDir;
        Settings {
            claude_dirs: vec![
                ClaudeDir {
                    name: "Personal".into(),
                    path: "~/.claude".into(),
                    command: "claude".into(),
                    args: vec![],
                    color: None,
                },
                ClaudeDir {
                    name: "Work".into(),
                    path: "~/.claude-work".into(),
                    command: "claude-w".into(),
                    args: vec!["--model".into(), "opus".into()],
                    color: None,
                },
            ],
            ..Settings::default()
        }
    }

    fn two_dirs() -> ClaudeDirs {
        ClaudeDirs::from_settings(
            &two_dirs_settings(),
            Some(Path::new("/home/me/.claude")),
            Some(Path::new("/home/me")),
        )
    }

    #[test]
    fn implicit_default_when_no_dirs() {
        let dirs =
            ClaudeDirs::from_settings(&Settings::default(), Some(Path::new("/d/.claude")), Some(Path::new("/d")));
        assert_eq!(
            dirs.dirs,
            vec![ResolvedDir {
                name: "Default".into(),
                path: "/d/.claude".into(),
                command: "claude".into(),
                args: vec![],
                color: None
            }]
        );
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

    // Unix-style paths: Windows joins them with `\`.
    #[cfg(unix)]
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
        let launch =
            PaneLaunch::claude_with("/r".into(), &dirs.dirs[1], Some(Path::new("/home/me/.claude")), Some("/r".into()));
        assert_eq!(
            (launch.program.as_str(), launch.container.clone(), launch.env.is_empty(), launch.claude_dir.clone()),
            ("claude", Some(PathBuf::from("/r")), true, None)
        );
        assert_eq!(&launch.args[..2], ["--model", "opus"]);
    }

    #[test]
    fn resume_uses_session_dir_command() {
        let dirs = two_dirs();
        let daemon = Some(Path::new("/home/me/.claude"));
        let launch =
            PaneLaunch::resume_with("s1", "/r".into(), None, Some(Path::new("/home/me/.claude-work")), &dirs, daemon);
        assert_eq!(launch.program, "claude-w");
        assert_eq!(launch.args, ["--model", "opus", "--resume", "s1"]);
        assert_eq!(launch.env, vec![("CLAUDE_CONFIG_DIR".to_string(), "/home/me/.claude-work".to_string())]);
        let default =
            PaneLaunch::resume_with("s1", "/r".into(), None, Some(Path::new("/home/me/.claude")), &dirs, daemon);
        assert_eq!((default.program.as_str(), default.env.is_empty()), ("claude", true));
    }

    #[test]
    fn resume_unconfigured_dir_keeps_its_path() {
        let dirs = two_dirs();
        let launch = PaneLaunch::resume_with(
            "s1",
            "/r".into(),
            None,
            Some(Path::new("/old/.claude-removed")),
            &dirs,
            Some(Path::new("/home/me/.claude")),
        );
        assert_eq!(
            (launch.program.as_str(), launch.args.clone()),
            ("claude", vec!["--resume".to_string(), "s1".to_string()])
        );
        assert_eq!(launch.env, vec![("CLAUDE_CONFIG_DIR".to_string(), "/old/.claude-removed".to_string())]);
    }

    #[test]
    fn old_layout_claude_flag_falls_back_to_name() {
        let json = r#"{"program":"claude","args":["--resume","s"],"cwd":"/r"}"#;
        let launch: PaneLaunch = serde_json::from_str(json).unwrap();
        assert!(!launch.claude && launch.is_claude());
        assert_eq!(launch.session(), Some("s"));
        let wrapper: PaneLaunch =
            serde_json::from_str(r#"{"program":"claude-w","args":["--resume","s"],"cwd":"/r","claude":true}"#).unwrap();
        assert_eq!(wrapper.session(), Some("s"));
        let other: PaneLaunch =
            serde_json::from_str(r#"{"program":"claude-w","args":["--resume","s"],"cwd":"/r"}"#).unwrap();
        assert_eq!(other.session(), None);
    }

    #[test]
    fn shell_launch_program_and_args() {
        let launch =
            PaneLaunch::shell("/r".into(), &ShellSettings { program: "/bin/zsh".into(), args: vec!["-l".into()] });
        assert_eq!(
            (launch.program.as_str(), launch.args.clone(), launch.is_claude()),
            ("/bin/zsh", vec!["-l".to_string()], false)
        );
        let default = PaneLaunch::shell("/r".into(), &ShellSettings::default());
        assert_eq!(default.program, "");
        assert_eq!(default.label(), "shell · r");
    }

    #[test]
    fn restore_without_layout_drops_dead_leaves_keeps_live() {
        let mut saved = workspace_on("i1");
        let a = saved.new_tab(shell("/a"));
        saved.pane_started(a, 1, "/a".into());
        let b = saved.split(Axis::Vertical, shell("/b")).unwrap();
        saved.pane_started(b, 2, "/b".into());
        let c = saved.new_tab(shell("/c"));
        saved.pane_started(c, 3, "/c".into());
        let restored = restore_local(Some(saved), "i1", &[info(1, "bash", 5)], &launch(), false, &HashSet::new());
        assert_eq!(restored.attach, vec![(a, PaneKey::local(1))]);
        assert_eq!(restored.workspace.leaves.keys().copied().collect::<Vec<_>>(), vec![a]);
        assert_eq!(restored.workspace.tabs.len(), 1);
        assert!(restored.spawn.is_empty() && restored.workspace.is_consistent());
        let none = restore_local(Some(workspace_on("i1")), "i1", &[], &launch(), false, &HashSet::new());
        assert_eq!(none.spawn.len(), 1, "no tab left: one new tab");
    }

    #[test]
    fn restore_keep_holds_the_listed_finished_leaves_when_the_layout_is_off() {
        let mut saved = workspace_on("i1");
        let a = saved.new_tab(shell("/a"));
        saved.pane_started(a, 1, "/a".into());
        let b = saved.new_tab(shell("/b"));
        let keep = HashSet::from([a]);
        // A new daemon: every pane id is stale, so `a` is finished too.
        let restored = restore_local(Some(saved.clone()), "i2", &[], &launch(), false, &keep);
        assert_eq!(restored.workspace.leaves.keys().copied().collect::<Vec<_>>(), vec![a]);
        assert_eq!(restored.workspace.finished_leaves(), vec![a]);
        assert!(restored.spawn.is_empty() && restored.workspace.is_consistent());
        let saved_back = restored.workspace.saveable();
        assert_eq!(saved_back.leaves.keys().copied().collect::<Vec<_>>(), vec![a, b], "the other tab is parked");
        // With the layout on, nothing changes.
        let on = restore_local(Some(saved.clone()), "i2", &[], &launch(), true, &keep);
        let plain = restore_local(Some(saved), "i2", &[], &launch(), true, &HashSet::new());
        assert_eq!(on.workspace.leaves.len(), 2);
        assert_eq!(on.workspace.tabs, plain.workspace.tabs);
    }

    #[test]
    fn restore_without_layout_keeps_dead_leaves_for_saving_and_live_launches() {
        let mut saved = workspace_on("i1");
        let a = saved.new_tab(PaneLaunch {
            claude_dir: Some("/w".into()),
            env: vec![("CLAUDE_CONFIG_DIR".into(), "/w".into())],
            ..shell("/a")
        });
        saved.pane_started(a, 1, "/a".into());
        let b = saved.split(Axis::Vertical, shell("/b")).unwrap();
        saved.pane_started(b, 2, "/b".into());
        let c = saved.new_tab(shell("/c"));
        saved.pane_started(c, 3, "/c".into());
        let d = saved.split(Axis::Vertical, shell("/d")).unwrap();
        saved.pane_started(d, 4, "/d".into());
        saved.sidebar.visible = false;
        let mut restored =
            restore_local(Some(saved), "i1", &[info(1, "bash", 5)], &launch(), false, &HashSet::new()).workspace;
        restored.sidebar.width = 300.0;
        let kept = restored.saveable();
        assert!(kept.is_consistent());
        assert_eq!(kept.leaves.keys().copied().collect::<Vec<_>>(), vec![a, b, c, d]);
        assert_eq!(kept.tabs.len(), 3, "the live tab, the dead leaf of the mixed tab, the dead split tab");
        assert_eq!(kept.tabs[2].root.leaves(), vec![c, d]);
        assert_eq!(
            (kept.leaves[&a].launch.claude_dir.clone(), kept.leaves[&a].launch.env.len()),
            (Some("/w".into()), 1)
        );
        assert_eq!((kept.sidebar.visible, kept.sidebar.width), (false, 300.0));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("layout.json");
        crate::layout_store::save(&file, &restored).unwrap();
        let back = restore_local(
            crate::layout_store::load(&file).unwrap(),
            "i1",
            &[info(1, "bash", 5)],
            &launch(),
            true,
            &HashSet::new(),
        );
        assert_eq!(back.workspace.leaves.len(), 4, "switching back restores the finished leaves");
        assert!(!back.workspace.sidebar.visible);
        assert_eq!(restored.leaves.len(), 1, "the running workspace only has the live leaf");
    }

    #[test]
    fn rerun_keeps_dir_and_env() {
        let dirs = two_dirs();
        let launch = PaneLaunch::claude_with("/r".into(), &dirs.dirs[1], Some(Path::new("/home/me/.claude")), None);
        let mut leaf = Leaf::new(launch.clone());
        leaf.session = Some("sid".into());
        let rerun = leaf.rerun(true);
        assert_eq!(rerun.args, ["--model", "opus", "--resume", "sid"]);
        assert_eq!(
            (rerun.program, rerun.env, rerun.claude_dir, rerun.claude),
            (launch.program, launch.env, launch.claude_dir, true)
        );
        assert_eq!(leaf.rerun(false).args, ["--model", "opus", "--session-id", "sid"]);
    }

    #[test]
    fn requests_carry_flag_env_and_scrollback() {
        let dirs = two_dirs();
        let launch = PaneLaunch::claude_with("/r".into(), &dirs.dirs[1], None, None);
        let request = launch.request(None, 80, 24, 2_000);
        assert_eq!((request.claude, request.scrollback, request.env.len()), (true, Some(2_000), 1));
    }

    #[test]
    fn split_stops_at_four_panes_per_tab() {
        let mut ws = Workspace::default();
        ws.new_tab(PaneLaunch::claude("/repo".into()));
        for _ in 0..3 {
            assert!(ws.can_split());
            assert!(ws.split(Axis::Vertical, PaneLaunch::claude("/repo".into())).is_some());
        }
        assert!(!ws.can_split());
        assert_eq!(ws.split(Axis::Vertical, PaneLaunch::claude("/repo".into())), None);
        assert_eq!(ws.leaves.len(), 4);
    }

    #[test]
    fn closing_a_pane_allows_splitting_again() {
        let mut ws = Workspace::default();
        ws.new_tab(PaneLaunch::claude("/repo".into()));
        for _ in 0..3 {
            ws.split(Axis::Vertical, PaneLaunch::claude("/repo".into()));
        }
        let last = ws.focused().unwrap();
        ws.close(last);
        assert!(ws.can_split());
    }

    #[test]
    fn the_limit_is_per_tab() {
        let mut ws = Workspace::default();
        ws.new_tab(PaneLaunch::claude("/repo".into()));
        for _ in 0..3 {
            ws.split(Axis::Vertical, PaneLaunch::claude("/repo".into()));
        }
        ws.new_tab(PaneLaunch::claude("/repo".into()));
        assert!(ws.can_split());
    }

    #[test]
    fn a_saved_tab_over_the_limit_loads_and_stays_full_until_two_close() {
        let mut ws = Workspace::default();
        ws.new_tab(PaneLaunch::claude("/repo".into()));
        for _ in 0..3 {
            ws.split(Axis::Vertical, PaneLaunch::claude("/repo".into()));
        }
        let (split_id, extra) = (ws.alloc(), ws.alloc());
        let target = ws.focused().unwrap();
        ws.tabs[0].root.split(target, Axis::Vertical, split_id, extra);
        ws.leaves.insert(extra, Leaf::new(PaneLaunch::claude("/repo".into())));
        let mut ws: Workspace = serde_json::from_str(&serde_json::to_string(&ws).unwrap()).unwrap();
        assert_eq!(ws.tabs[0].root.leaves().len(), 5);
        assert!(!ws.can_split());
        assert_eq!(ws.split(Axis::Vertical, PaneLaunch::claude("/repo".into())), None);
        ws.close(extra);
        assert_eq!(ws.tabs[0].root.leaves().len(), 4);
        assert!(!ws.can_split());
    }

    #[test]
    fn no_split_without_tabs_or_on_the_settings_tab() {
        let mut ws = Workspace::default();
        assert!(!ws.can_split());
        ws.new_tab(PaneLaunch::claude("/repo".into()));
        assert!(ws.can_split());
        let settings = ws.open_settings();
        ws.select_tab(settings);
        assert!(!ws.can_split());
    }

    #[test]
    fn a_new_tab_can_open_on_a_host() {
        let mut ws = Workspace::default();
        let remote = ws.new_tab_on(shell("/home/me"), HostId::named("devbox"));
        let local = ws.new_tab(shell("/a"));
        assert_eq!(ws.leaves[&remote].host, HostId::named("devbox"));
        assert!(ws.leaves[&local].host.is_local());
        assert!(ws.is_consistent());
    }

    #[test]
    fn reconcile_reattaches_a_hosts_leaves_when_its_daemon_is_the_same_instance() {
        let devbox = HostId::named("devbox");
        let mut ws = Workspace::default();
        ws.set_instance(&devbox, "r1");
        let a = ws.new_tab_on(shell("/a"), devbox.clone());
        ws.pane_started(a, 4, "/a".into());
        let panes = [info(4, "bash", 5)];
        let attach = reconcile(&mut ws, &HostLive { host: devbox.clone(), instance: "r1", panes: &panes });
        assert_eq!(attach, vec![(a, PaneKey::new(devbox, 4))]);
        assert_eq!(ws.leaves[&a].pane, Some(4));
        assert_eq!(ws.tabs.len(), 1, "nothing was adopted");
    }

    #[test]
    fn reconcile_kills_the_leaves_of_a_restarted_daemon_and_adopts_its_live_panes() {
        let devbox = HostId::named("devbox");
        let mut ws = Workspace::default();
        ws.set_instance(&devbox, "r1");
        let old = ws.new_tab_on(shell("/a"), devbox.clone());
        ws.pane_started(old, 4, "/a".into());
        let panes = [info(4, "zsh", 9)];
        let attach = reconcile(&mut ws, &HostLive { host: devbox.clone(), instance: "r2", panes: &panes });
        assert_eq!(ws.leaves[&old].pane, None, "pane 4 of the old instance means nothing now");
        assert_eq!(ws.instances[&devbox], "r2");
        assert_eq!(attach.len(), 1);
        let (adopted, key) = &attach[0];
        assert_ne!(*adopted, old);
        assert_eq!(key.pane, 4);
        assert_eq!(ws.leaves[adopted].host, devbox);
        assert_eq!(ws.leaves[adopted].launch.program, "zsh");
    }

    #[test]
    fn reconcile_leaves_the_other_hosts_alone() {
        let (devbox, lab) = (HostId::named("devbox"), HostId::named("lab"));
        let mut ws = workspace_on("i1");
        ws.set_instance(&lab, "l1");
        let local = ws.new_tab(shell("/a"));
        ws.pane_started(local, 1, "/a".into());
        let other = ws.new_tab_on(shell("/b"), lab.clone());
        ws.pane_started(other, 1, "/b".into());
        // devbox connects with a pane 1 of its own and a new instance.
        let panes = [info(1, "bash", 1)];
        let attach = reconcile(&mut ws, &HostLive { host: devbox.clone(), instance: "r1", panes: &panes });
        assert_eq!(ws.leaves[&local].pane, Some(1));
        assert_eq!(ws.leaves[&other].pane, Some(1), "lab is not connected now but its leaf is not touched");
        assert!(attach.iter().all(|(_, key)| key.host == devbox));
        assert_eq!(ws.instances[&lab], "l1");
    }

    #[test]
    fn rename_host_moves_leaves_and_the_instance_key() {
        let (dev, main_pc) = (HostId::named("dev"), HostId::named("MainPC"));
        let mut ws = Workspace::default();
        ws.set_instance(&dev, "r1");
        ws.set_instance(&HostId::local(), "i1");
        let a = ws.new_tab_on(shell("/a"), dev.clone());
        let b = ws.new_tab_on(shell("/b"), dev.clone());
        let local = ws.new_tab(shell("/c"));
        ws.rename_host(&dev, &main_pc);
        assert_eq!((ws.leaves[&a].host.clone(), ws.leaves[&b].host.clone()), (main_pc.clone(), main_pc.clone()));
        assert!(ws.leaves[&local].host.is_local());
        assert_eq!(ws.instances[&main_pc], "r1");
        assert!(!ws.instances.contains_key(&dev));
        assert_eq!(ws.instances[&HostId::local()], "i1");
    }

    #[test]
    fn remove_host_drops_its_leaves_and_empty_tabs_keeping_other_hosts() {
        let dev = HostId::named("dev");
        let mut ws = Workspace::default();
        ws.set_instance(&dev, "r1");
        let only = ws.new_tab_on(shell("/a"), dev.clone());
        ws.pane_started(only, 4, "/a".into());
        let mixed_local = ws.new_tab(shell("/b"));
        let mixed_dev = ws.split_on(Axis::Horizontal, shell("/c"), dev.clone()).unwrap();
        ws.pane_started(mixed_dev, 5, "/c".into());
        let unstarted = ws.new_tab_on(shell("/d"), dev.clone());
        let removed = ws.remove_host(&dev);
        assert_eq!(removed.len(), 2, "only started panes have keys: {removed:?}");
        for pane in [4, 5] {
            assert!(removed.contains(&PaneKey::new(dev.clone(), pane)));
        }
        assert_eq!(ws.tabs.len(), 1, "the dev-only tabs are gone");
        assert!(ws.leaves.contains_key(&mixed_local));
        for gone in [only, mixed_dev, unstarted] {
            assert!(!ws.leaves.contains_key(&gone));
        }
        assert_eq!(ws.tabs[0].root.leaves(), vec![mixed_local]);
        assert!(!ws.instances.contains_key(&dev));
    }

    #[test]
    fn removing_a_host_from_the_list_and_the_workspace_leaves_no_key_of_it() {
        use crate::settings::{SshHost, add_ssh_host, remove_ssh_host};
        let dev = HostId::named("dev");
        let mut hosts = Vec::new();
        let entry = SshHost { name: "dev".into(), target: "me@dev".into(), command: "~/.local/bin/gantry".into() };
        add_ssh_host(&mut hosts, entry).unwrap();
        let mut ws = Workspace::default();
        ws.set_instance(&dev, "r1");
        ws.new_tab_on(shell("/a"), dev.clone());
        ws.new_tab(shell("/b"));
        assert!(remove_ssh_host(&mut hosts, "dev"));
        ws.remove_host(&dev);
        assert!(hosts.is_empty());
        assert!(!ws.instances.contains_key(&dev));
        assert!(ws.leaves.values().all(|leaf| leaf.host != dev));
        assert_eq!(ws.tabs.len(), 1);
    }
}
