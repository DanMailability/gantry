//! The GUI's workspace: tabs of split panes, what each pane runs, and reconciling a saved layout
//! with the daemon's live panes at startup.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use protocol::message::{PaneId, PaneInfo, SpawnRequest, claude_session};
use serde::{Deserialize, Serialize};

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
}

impl PaneLaunch {
    /// A new Claude Code session, with its own `--session-id`, in `cwd`.
    pub fn claude(cwd: PathBuf) -> PaneLaunch {
        PaneLaunch { program: CLAUDE.into(), args: vec!["--session-id".into(), uuid::Uuid::new_v4().to_string()], cwd }
    }

    /// Claude Code resuming `session` in `cwd`.
    pub fn resume(session: &str, cwd: PathBuf) -> PaneLaunch {
        PaneLaunch { program: CLAUDE.into(), args: vec!["--resume".into(), session.into()], cwd }
    }

    /// The Claude session this command line names (`--session-id`, `--resume`, `-r`).
    pub fn session(&self) -> Option<&str> {
        claude_session(&self.program, &self.args)
    }

    /// Whether this runs Claude Code.
    pub fn is_claude(&self) -> bool {
        Path::new(&self.program).file_stem().is_some_and(|stem| stem == CLAUDE)
    }

    /// Short label: program and directory names, e.g. `claude · repo`.
    pub fn label(&self) -> String {
        let name = |path: &Path| path.file_name().map(|n| n.to_string_lossy().into_owned());
        let program = name(Path::new(&self.program)).unwrap_or_else(|| self.program.clone());
        match name(&self.cwd) {
            Some(dir) => format!("{program} · {dir}"),
            None => program,
        }
    }

    /// The daemon request that starts this, optionally in `cwd_from`'s current directory.
    pub fn request(&self, cwd_from: Option<PaneId>, cols: u16, rows: u16) -> SpawnRequest {
        SpawnRequest { program: self.program.clone(), args: self.args.clone(), cwd: self.cwd.clone(), cols, rows, cwd_from }
    }
}

/// What the GUI was asked to open at startup.
#[derive(Clone, Debug)]
pub struct Launch {
    /// Explicitly requested command (`ccm -- cmd` or `--cwd`); always opens a new tab.
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
        PaneLaunch { program: launch.program.clone(), args, cwd: launch.cwd.clone() }
    }
}

/// One tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    /// Its panes.
    pub root: Node,
    /// The leaf with focus inside this tab.
    pub focused: NodeId,
}

/// Which sidebar view is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TreeView {
    /// Directories with their sessions.
    #[default]
    Folders,
    /// Sessions grouped by state.
    Status,
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
    /// Folders or Status.
    pub view: TreeView,
}

impl Default for SidebarState {
    fn default() -> Self {
        SidebarState { visible: true, width: SIDEBAR_WIDTH, view: TreeView::Folders }
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
    /// Sidebar visibility, width and view.
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
        self.tabs.push(Tab { root: Node::Leaf(leaf), focused: leaf });
        self.active = self.tabs.len() - 1;
        leaf
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
        self.active_tab().map(|tab| tab.focused)
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
        let tab = self.tabs.get_mut(self.active)?;
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
        let tab = self.tabs.remove(self.active);
        self.tabs.insert(target, tab);
        self.active = target;
    }

    /// Set a split's ratio in the visible tab.
    pub fn set_ratio(&mut self, split: NodeId, ratio: f32) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.root.set_ratio(split, ratio);
        }
    }

    /// Leaf showing daemon pane `pane`.
    pub fn leaf_for_pane(&self, pane: PaneId) -> Option<NodeId> {
        self.leaves.iter().find(|(_, leaf)| leaf.pane == Some(pane)).map(|(id, _)| *id)
    }

    /// Index of the tab containing `leaf`.
    pub fn tab_of(&self, leaf: NodeId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.root.contains(leaf))
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
            let leaves = tab.root.leaves();
            if !leaves.contains(&tab.focused) || !leaves.iter().all(|leaf| self.leaves.contains_key(leaf)) {
                return false;
            }
            tab_leaves += leaves.len();
        }
        tab_leaves == self.leaves.len() && (self.tabs.is_empty() || self.active < self.tabs.len())
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
/// empty result opens the default.
pub fn restore(saved: Option<Workspace>, instance: &str, live: &[PaneInfo], launch: &Launch) -> Restored {
    let mut workspace = saved.unwrap_or_default();
    let same_daemon = workspace.instance == instance;
    workspace.instance = instance.into();
    let live_ids: HashSet<PaneId> = live.iter().map(|info| info.pane).collect();
    for leaf in workspace.leaves.values_mut() {
        if !same_daemon || leaf.pane.is_some_and(|pane| !live_ids.contains(&pane)) {
            leaf.pane = None;
        }
    }
    let mut attach: Vec<(NodeId, PaneId)> =
        workspace.leaves.iter().filter_map(|(id, leaf)| leaf.pane.map(|pane| (*id, pane))).collect();
    let known: HashSet<PaneId> = attach.iter().map(|(_, pane)| *pane).collect();
    let mut orphans: Vec<&PaneInfo> = live.iter().filter(|info| !known.contains(&info.pane)).collect();
    orphans.sort_by_key(|info| (info.last_focused, info.pane));
    for info in orphans {
        let leaf = workspace.new_tab(PaneLaunch { program: info.program.clone(), args: info.args.clone(), cwd: info.cwd.clone() });
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

    fn shell(cwd: &str) -> PaneLaunch {
        PaneLaunch { program: "/bin/bash".into(), args: vec![], cwd: cwd.into() }
    }

    fn info(pane: PaneId, program: &str, last_focused: u64) -> PaneInfo {
        PaneInfo { pane, program: program.into(), args: vec!["-l".into()], cwd: "/srv".into(), last_focused }
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
        Leaf { pane: None, launch: PaneLaunch { program: "claude".into(), args: vec![], cwd: "/r".into() }, session: session.map(Into::into) }
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
        let launch = PaneLaunch::resume("abc", "/r".into());
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
        let live = [PaneInfo { pane: 4, program: "claude".into(), args: vec!["--resume".into(), "xyz".into()], cwd: "/srv".into(), last_focused: 1 }];
        let restored = restore(None, "i", &live, &launch());
        let leaf = restored.attach[0].0;
        assert_eq!(restored.workspace.leaves[&leaf].session.as_deref(), Some("xyz"));
    }

    #[test]
    fn the_sidebar_defaults_to_a_visible_folder_view() {
        let state = Workspace::default().sidebar;
        assert_eq!((state.visible, state.width, state.view), (true, SIDEBAR_WIDTH, TreeView::Folders));
    }

    #[test]
    fn spawn_requests_carry_the_focused_pane() {
        let request = shell("/a").request(Some(3), 100, 30);
        assert_eq!((request.cwd_from, request.cols, request.rows, request.program.as_str()), (Some(3), 100, 30, "/bin/bash"));
    }

    fn launch() -> Launch {
        Launch { explicit: None, default: shell("/home") }
    }

    #[test]
    fn restore_with_nothing_spawns_the_default() {
        let restored = restore(None, "i1", &[], &launch());
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
        let restored = restore(Some(saved), "i1", &[info(1, "bash", 5)], &launch());
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
        let restored = restore(Some(saved), "new", &[info(1, "zsh", 1)], &launch());
        assert_eq!(restored.workspace.leaves[&a].pane, None);
        assert_eq!(restored.workspace.tabs.len(), 2, "the new daemon's pane 1 is adopted as its own tab");
        let (adopted, pane) = restored.attach[0];
        assert_ne!(adopted, a);
        assert_eq!((pane, restored.workspace.leaves[&adopted].launch.program.as_str()), (1, "zsh"));
    }

    #[test]
    fn restore_adopts_live_panes_missing_from_the_layout_most_recent_last() {
        let restored = restore(None, "i", &[info(5, "a", 9), info(4, "b", 2)], &launch());
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
        let restored = restore(Some(saved), "i", &[], &launch);
        let focused = restored.workspace.focused().unwrap();
        assert_eq!(restored.spawn, vec![focused]);
        assert!(restored.workspace.leaves[&focused].launch.is_claude());
        assert_eq!(restored.workspace.tabs.len(), 2);
    }
}
