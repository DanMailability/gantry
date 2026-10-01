//! The window's root view: tabs of split panes, routing daemon events to them, and saving the layout.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use app_core::attention::{Alert, AlertKind, AttentionTracker};
use app_core::layout_store;
use app_core::split_tree::{Axis, Direction, Divider, NodeId, Rect};
use app_core::workspace::{PaneLaunch, Rerun, Restored, Workspace};
use daemon::client::{ClientEvent, Connection, DaemonClient, batch_events};
use daemon::pty::default_shell;
use gpui::{
    Bounds, Context, CursorStyle, Decorations, Entity, FocusHandle, Focusable, IntoElement, MouseButton, MouseMoveEvent, Pixels,
    PathPromptOptions, PromptLevel, Render, Subscription, Task, Window, canvas, div, prelude::*, px, relative, rgb,
};
use protocol::message::{PaneId, SessionInfo, SessionState, SessionStatus, TakeOverOutcome};

use app_core::session_tree::{Activation, ShellEntry};

use crate::launcher_view::{LauncherEvent, LauncherView};
use crate::sidebar_view::{SidebarEvent, SidebarView};
use crate::terminal_view::{PaneEvent, TerminalView};
use crate::window_frame;

pub(crate) const PANEL: u32 = 0x1a1e26;
pub(crate) const BORDER: u32 = 0x2a2f3a;
pub(crate) const ACCENT: u32 = 0x6cb6ff;
pub(crate) const TEXT: u32 = 0xd6dde6;
pub(crate) const MUTED: u32 = 0x8b949e;
pub(crate) const SELECTED: u32 = 0x24324a;
pub(crate) const AMBER: u32 = 0xe3b341;
const BACKGROUND: u32 = 0x11141a;
pub(crate) const TAB_BAR_HEIGHT: f32 = 34.0;
/// Largest amount of output routed per UI update, so input and rendering keep running during a flood.
const MAX_BATCH_BYTES: usize = 256 * 1024;
const SIDEBAR_MIN: f32 = 160.0;
const SIDEBAR_MAX: f32 = 600.0;

#[allow(missing_docs)]
mod workspace_actions {
    gpui::actions!(
        workspace,
        [
            NewClaudeTab, NewShellTab, SplitRightClaude, SplitRightShell, SplitDownClaude, SplitDownShell, ClosePane,
            FocusLeft, FocusRight, FocusUp, FocusDown, PreviousTab, NextTab, MoveTabLeft, MoveTabRight,
            ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6, ActivateTab7,
            ActivateTab8, ActivateTab9, ToggleSidebar, FindSession, OpenLauncher
        ]
    );
}
use workspace_actions::*;

/// Register workspace shortcuts: `Ctrl+Shift+…` on Linux/Windows, `Cmd+…` on macOS.
pub fn bind_keys(cx: &mut gpui::App) {
    cx.bind_keys(key_bindings());
}

fn key_bindings() -> Vec<gpui::KeyBinding> {
    use gpui::KeyBinding;
    #[cfg(target_os = "macos")]
    let (m, prev_tab, next_tab, tab_n) = ("cmd", "cmd-shift-[", "cmd-shift-]", "cmd");
    #[cfg(not(target_os = "macos"))]
    let (m, prev_tab, next_tab, tab_n) = ("ctrl-shift", "ctrl-pageup", "ctrl-pagedown", "alt");
    let context = Some("Workspace");
    vec![
        KeyBinding::new(&format!("{m}-t"), NewClaudeTab, context),
        KeyBinding::new(&format!("{m}-alt-t"), NewShellTab, context),
        KeyBinding::new(&format!("{m}-d"), SplitRightClaude, context),
        KeyBinding::new(&format!("{m}-alt-d"), SplitRightShell, context),
        KeyBinding::new(&format!("{m}-e"), SplitDownClaude, context),
        KeyBinding::new(&format!("{m}-alt-e"), SplitDownShell, context),
        KeyBinding::new(&format!("{m}-w"), ClosePane, context),
        KeyBinding::new(&format!("{m}-left"), FocusLeft, context),
        KeyBinding::new(&format!("{m}-right"), FocusRight, context),
        KeyBinding::new(&format!("{m}-up"), FocusUp, context),
        KeyBinding::new(&format!("{m}-down"), FocusDown, context),
        KeyBinding::new(prev_tab, PreviousTab, context),
        KeyBinding::new(next_tab, NextTab, context),
        KeyBinding::new(&format!("{m}-pageup"), MoveTabLeft, context),
        KeyBinding::new(&format!("{m}-pagedown"), MoveTabRight, context),
        KeyBinding::new(&format!("{tab_n}-1"), ActivateTab1, context),
        KeyBinding::new(&format!("{tab_n}-2"), ActivateTab2, context),
        KeyBinding::new(&format!("{tab_n}-3"), ActivateTab3, context),
        KeyBinding::new(&format!("{tab_n}-4"), ActivateTab4, context),
        KeyBinding::new(&format!("{tab_n}-5"), ActivateTab5, context),
        KeyBinding::new(&format!("{tab_n}-6"), ActivateTab6, context),
        KeyBinding::new(&format!("{tab_n}-7"), ActivateTab7, context),
        KeyBinding::new(&format!("{tab_n}-8"), ActivateTab8, context),
        KeyBinding::new(&format!("{tab_n}-9"), ActivateTab9, context),
        KeyBinding::new(&format!("{m}-b"), ToggleSidebar, context),
        KeyBinding::new(&format!("{m}-f"), FindSession, context),
        KeyBinding::new(&format!("{m}-o"), OpenLauncher, context),
    ]
}

struct Slot {
    view: Entity<TerminalView>,
    _subscription: Subscription,
}

/// Where a taken-over session is resumed.
enum TakeOverTarget {
    /// A new tab, or a split of the focused pane.
    NewPane {
        /// The session's directory.
        cwd: PathBuf,
        /// As a split.
        split: bool,
    },
    /// The finished leaf that last ran it (Continue).
    Leaf(NodeId),
}

/// Root view of the ccm window.
pub struct WorkspaceView {
    workspace: Workspace,
    slots: HashMap<NodeId, Slot>,
    /// Spawn request id → leaf waiting for it.
    pending: HashMap<u64, NodeId>,
    next_request: u64,
    client: DaemonClient,
    layout_file: PathBuf,
    default_cwd: PathBuf,
    focus_handle: FocusHandle,
    prompting: Rc<Cell<bool>>,
    /// Divider being dragged.
    dragging: Option<Divider>,
    /// Window bounds of the visible tab's pane area (for divider drags).
    content_bounds: Bounds<Pixels>,
    sidebar: Entity<SidebarView>,
    /// Window bounds of the sidebar (for width drags).
    sidebar_bounds: Bounds<Pixels>,
    /// The sidebar's edge is being dragged.
    resizing_sidebar: bool,
    _sidebar_events: Subscription,
    /// The open launcher overlay.
    launcher: Option<(Entity<LauncherView>, Subscription)>,
    /// Take-over requests waiting for the daemon, by session.
    take_overs: HashMap<String, TakeOverTarget>,
    /// Status changes seen so far, to tell which ones alert.
    attention: AttentionTracker,
    /// Leaves with an alert the user has not seen yet.
    unseen: HashMap<NodeId, AlertKind>,
    _activation: Subscription,
    _events: Task<()>,
}

impl WorkspaceView {
    /// Show `restored`, attaching and spawning what it says, and start routing `connection`'s events.
    pub fn new(
        connection: Connection,
        restored: Restored,
        layout_file: PathBuf,
        default_cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Connection { client, events, .. } = connection;
        let events_task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(first) = events.recv().await {
                let batch = batch_events(first, &events, MAX_BATCH_BYTES);
                if this.update_in(cx, |view, window, cx| view.on_events(batch, window, cx)).is_err() {
                    return;
                }
                // Yield so keystrokes (e.g. Ctrl+C) are handled between batches.
                cx.background_executor().timer(Duration::ZERO).await;
            }
            let _ = this.update_in(cx, |view, window, cx| view.on_events(vec![ClientEvent::Disconnected], window, cx));
        });
        let sidebar = cx.new(|cx| SidebarView::new(restored.workspace.sidebar.view, cx));
        let sidebar_events = cx.subscribe_in(&sidebar, window, |this, _, event: &SidebarEvent, window, cx| {
            this.on_sidebar_event(event.clone(), window, cx)
        });
        // Coming back to the window counts as seeing its visible tab.
        let activation = cx.observe_window_activation(window, |this, window, cx| this.clear_seen(window, cx));
        let mut this = WorkspaceView {
            workspace: restored.workspace,
            slots: HashMap::new(),
            pending: HashMap::new(),
            next_request: 0,
            client,
            layout_file,
            default_cwd,
            focus_handle: cx.focus_handle(),
            prompting: Rc::new(Cell::new(false)),
            dragging: None,
            content_bounds: Bounds::default(),
            sidebar,
            sidebar_bounds: Bounds::default(),
            resizing_sidebar: false,
            _sidebar_events: sidebar_events,
            launcher: None,
            take_overs: HashMap::new(),
            attention: AttentionTracker::default(),
            unseen: HashMap::new(),
            _activation: activation,
            _events: events_task,
        };
        let leaves: Vec<NodeId> = this.workspace.leaves.keys().copied().collect();
        for leaf in leaves {
            this.add_slot(leaf, window, cx);
        }
        for (leaf, pane) in &restored.attach {
            let launch = this.workspace.leaves[leaf].launch.clone();
            this.slots[leaf].view.update(cx, |view, cx| view.attach(*pane, launch, cx));
            this.client.attach(*pane);
        }
        for (leaf, slot) in &this.slots {
            if this.workspace.leaves[leaf].pane.is_none() && !restored.spawn.contains(leaf) {
                slot.view.update(cx, |view, cx| view.exited(None, cx));
            }
        }
        for leaf in restored.spawn {
            this.spawn_leaf(leaf, None, cx);
        }
        this.focus_current(window, cx);
        this.save(cx);
        this
    }

    fn add_slot(&mut self, leaf: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let launch = self.workspace.leaves[&leaf].launch.clone();
        let client = self.client.clone();
        let view = cx.new(|cx| TerminalView::new(client, launch, window, cx));
        let subscription = cx.subscribe_in(&view, window, move |this, _, event: &PaneEvent, window, cx| this.on_pane_event(leaf, *event, window, cx));
        self.slots.insert(leaf, Slot { view, _subscription: subscription });
    }

    fn spawn_leaf(&mut self, leaf: NodeId, cwd_from: Option<PaneId>, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get(&leaf) else { return };
        let (cols, rows) = slot.view.read(cx).grid_size();
        self.next_request += 1;
        self.pending.insert(self.next_request, leaf);
        self.client.spawn(self.next_request, self.workspace.leaves[&leaf].launch.request(cwd_from, cols, rows));
    }

    fn on_events(&mut self, events: Vec<ClientEvent>, window: &mut Window, cx: &mut Context<Self>) {
        for event in events {
            match &event {
                ClientEvent::Output { pane, .. } | ClientEvent::Resync { pane, .. } => {
                    if let Some(slot) = self.workspace.leaf_for_pane(*pane).and_then(|leaf| self.slots.get(&leaf)) {
                        slot.view.update(cx, |view, cx| view.on_event(&event, cx));
                    }
                }
                ClientEvent::Exited { pane, code } => {
                    if let Some(leaf) = self.workspace.pane_exited(*pane) {
                        self.slots[&leaf].view.update(cx, |view, cx| view.exited(Some(*code), cx));
                        self.save(cx);
                    }
                }
                ClientEvent::Spawned { request, pane, cwd } => match self.pending.remove(request) {
                    Some(leaf) => {
                        self.workspace.pane_started(leaf, *pane, cwd.clone());
                        let launch = self.workspace.leaves[&leaf].launch.clone();
                        self.slots[&leaf].view.update(cx, |view, cx| view.attach(*pane, launch, cx));
                        if self.workspace.focused() == Some(leaf) {
                            self.client.focus(*pane);
                        }
                        self.save(cx);
                    }
                    // The leaf was closed while its process was starting.
                    None => self.client.kill(*pane),
                },
                ClientEvent::SpawnFailed { request, error } => {
                    if let Some(leaf) = self.pending.remove(request) {
                        self.slots[&leaf].view.update(cx, |view, cx| view.failed(error.clone(), cx));
                    }
                }
                ClientEvent::Sessions { sessions } => {
                    self.track_sessions(sessions, cx);
                    self.attention.reset(sessions);
                    let sessions = sessions.clone();
                    self.sidebar.update(cx, |sidebar, cx| sidebar.set_sessions(sessions, cx));
                }
                ClientEvent::SessionsChanged { upserted, removed } => {
                    self.track_sessions(upserted, cx);
                    let alerts = self.attention.update(upserted, removed);
                    let upserted = upserted.clone();
                    self.sidebar.update(cx, |sidebar, cx| sidebar.apply(upserted, removed, cx));
                    for alert in alerts {
                        self.raise(alert, window, cx);
                    }
                }
                ClientEvent::TakeOverResult { session, outcome } => self.on_take_over(session.clone(), outcome.clone(), window, cx),
                ClientEvent::TakenOver => cx.quit(),
                ClientEvent::Disconnected => {
                    for slot in self.slots.values() {
                        slot.view.update(cx, |view, cx| view.disconnected(cx));
                    }
                }
            }
        }
        cx.notify();
    }

    /// Leaves follow the session their pane runs (it changes on `/clear`).
    fn track_sessions(&mut self, sessions: &[SessionInfo], cx: &mut Context<Self>) {
        let mut changed = false;
        for session in sessions {
            if let SessionState::Managed { pane } = session.state {
                changed |= self.workspace.session_seen(pane, &session.id);
            }
        }
        if changed {
            self.save(cx);
        }
    }

    /// Mark `alert`'s pane unseen — unless the user is looking at it (visible tab, active window).
    fn raise(&mut self, alert: Alert, window: &mut Window, cx: &mut Context<Self>) {
        let Some(leaf) = self.workspace.leaf_for_pane(alert.pane) else { return };
        if window.is_window_active() && self.workspace.is_visible(leaf) {
            return;
        }
        self.unseen.insert(leaf, alert.kind);
        self.sync_unseen(cx);
    }

    /// Forget alerts of the panes the user can see now.
    fn clear_seen(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !window.is_window_active() {
            return;
        }
        let before = self.unseen.len();
        let workspace = &self.workspace;
        self.unseen.retain(|leaf, _| !workspace.is_visible(*leaf));
        if self.unseen.len() != before {
            self.sync_unseen(cx);
        }
        cx.notify();
    }

    /// Tell the sidebar which sessions carry the unseen badge.
    fn sync_unseen(&self, cx: &mut Context<Self>) {
        let sessions: HashSet<String> =
            self.unseen.keys().filter_map(|leaf| self.workspace.leaves.get(leaf)?.session.clone()).collect();
        self.sidebar.update(cx, |sidebar, cx| sidebar.set_unseen(sessions, cx));
    }

    /// Start `launch` in the finished `leaf`.
    fn start_leaf(&mut self, leaf: NodeId, launch: PaneLaunch, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get(&leaf) else { return };
        if let Some(entry) = self.workspace.leaves.get_mut(&leaf) {
            entry.launch = launch.clone();
        }
        slot.view.update(cx, |view, cx| view.restarting(launch, cx));
        self.spawn_leaf(leaf, None, cx);
        self.save(cx);
    }

    /// Continue / Restart a finished leaf. If its session already runs in another pane, show that one. If it runs
    /// outside ccm on this host, offer to take it over.
    fn rerun_leaf(&mut self, leaf: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.workspace.leaves.get(&leaf) else { return };
        let session = entry.session.clone();
        match entry.rerun_with(self.sidebar.read(cx).sessions()) {
            Rerun::Start(launch) => self.start_leaf(leaf, launch, cx),
            Rerun::Focus(pane) => {
                if let Some(running) = self.workspace.leaf_for_pane(pane) {
                    self.focus_leaf(running, window, cx);
                }
            }
            Rerun::Busy => {
                let sessions = self.sidebar.read(cx).sessions();
                let takeable = session.as_deref().and_then(|id| sessions.get(id)).is_some_and(|info| {
                    matches!(info.state, SessionState::External { pid: Some(_) }) && info.cwd_exists
                });
                if let (true, Some(session)) = (takeable, session) {
                    self.confirm_take_over(session, TakeOverTarget::Leaf(leaf), window, cx);
                }
            }
        }
    }

    /// Ask before stopping the outside process running `session`; on yes, request it and remember where to resume.
    fn confirm_take_over(&mut self, session: String, target: TakeOverTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.take_overs.contains_key(&session) {
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "Take over this session?",
            Some("The Claude running it outside ccm is stopped. Anything typed there but not sent is lost."),
            &["Take over", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, _, _| {
                    this.client.take_over(&session, false);
                    this.take_overs.insert(session, target);
                });
            }
        })
        .detach();
    }

    fn on_take_over(&mut self, session: String, outcome: TakeOverOutcome, window: &mut Window, cx: &mut Context<Self>) {
        match outcome {
            TakeOverOutcome::Exited => match self.take_overs.remove(&session) {
                Some(TakeOverTarget::NewPane { cwd, split }) => {
                    let axis = split.then_some(Axis::Horizontal);
                    self.open_launch(PaneLaunch::resume(&session, cwd), axis, None, window, cx);
                }
                Some(TakeOverTarget::Leaf(leaf)) => {
                    if let Some(launch) = self.workspace.leaves.get(&leaf).map(|entry| entry.rerun(true)) {
                        self.start_leaf(leaf, launch, cx);
                        self.focus_leaf(leaf, window, cx);
                    }
                }
                None => {}
            },
            TakeOverOutcome::StillRunning => {
                if !self.take_overs.contains_key(&session) {
                    return;
                }
                let answer = window.prompt(
                    PromptLevel::Warning,
                    "Claude is still running",
                    Some("It did not stop within 5 seconds. Force quit stops it immediately."),
                    &["Force quit", "Cancel"],
                    cx,
                );
                cx.spawn_in(window, async move |this, cx| {
                    let force = answer.await == Ok(0);
                    let _ = this.update_in(cx, |this, _, _| {
                        if force {
                            this.client.take_over(&session, true);
                        } else {
                            this.take_overs.remove(&session);
                        }
                    });
                })
                .detach();
            }
            TakeOverOutcome::Failed { error } => {
                self.take_overs.remove(&session);
                let answer = window.prompt(PromptLevel::Critical, "Could not take over the session", Some(&error), &["OK"], cx);
                cx.spawn(async move |_, _| {
                    let _ = answer.await;
                })
                .detach();
            }
        }
    }

    fn on_pane_event(&mut self, leaf: NodeId, event: PaneEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Focused => self.focus_leaf(leaf, window, cx),
            PaneEvent::Rerun => self.rerun_leaf(leaf, window, cx),
            PaneEvent::Close => self.close_leaf(leaf, window, cx),
            PaneEvent::TitleChanged => {
                self.sync_shells(cx);
                cx.notify()
            }
        }
    }

    /// Open a new Claude or shell pane — as a new tab, or splitting the focused pane along `axis` —
    /// in the focused pane's current directory.
    fn open(&mut self, claude: bool, axis: Option<Axis>, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.workspace.focused().and_then(|leaf| self.workspace.leaves.get(&leaf));
        let cwd = source.map_or_else(|| self.default_cwd.clone(), |leaf| leaf.launch.cwd.clone());
        let cwd_from = source.and_then(|leaf| leaf.pane);
        self.open_launch(new_launch(claude, cwd), axis, cwd_from, window, cx);
    }

    /// Start `launch` in a new tab, or splitting the focused pane along `axis`.
    pub(crate) fn open_launch(
        &mut self,
        launch: PaneLaunch,
        axis: Option<Axis>,
        cwd_from: Option<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let leaf = match axis.and_then(|axis| self.workspace.split(axis, launch.clone())) {
            Some(leaf) => leaf,
            None => self.workspace.new_tab(launch),
        };
        self.add_slot(leaf, window, cx);
        self.spawn_leaf(leaf, cwd_from, cx);
        self.focus_current(window, cx);
        self.save(cx);
    }

    fn close_leaf(&mut self, leaf: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.workspace.close(leaf).and_then(|removed| removed.pane) {
            self.client.kill(pane);
        }
        self.slots.remove(&leaf);
        if self.unseen.remove(&leaf).is_some() {
            self.sync_unseen(cx);
        }
        self.pending.retain(|_, waiting| *waiting != leaf);
        self.focus_current(window, cx);
        self.save(cx);
    }

    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.select_tab(index) {
            self.focus_current(window, cx);
            self.save(cx);
        }
    }

    fn focus_leaf(&mut self, leaf: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.focus(leaf) {
            self.focus_current(window, cx);
            self.save(cx);
        }
    }

    /// Give keyboard focus to the workspace's focused leaf (or the empty window).
    fn focus_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.workspace.focused().and_then(|leaf| self.slots.get(&leaf).map(|slot| (leaf, slot))) {
            Some((leaf, slot)) => {
                slot.view.focus_handle(cx).focus(window, cx);
                if let Some(pane) = self.workspace.leaves[&leaf].pane {
                    self.client.focus(pane);
                }
            }
            None => self.focus_handle.focus(window, cx),
        }
        self.clear_seen(window, cx);
    }

    /// Write the layout (atomically) and refresh the sidebar's shell entries.
    fn save(&self, cx: &mut Context<Self>) {
        if let Err(err) = layout_store::save(&self.layout_file, &self.workspace) {
            tracing::warn!("saving the layout failed: {err}");
        }
        self.sync_shells(cx);
    }

    fn sync_shells(&self, cx: &mut Context<Self>) {
        let shells: Vec<ShellEntry> = self
            .workspace
            .leaves
            .iter()
            .filter(|(_, leaf)| leaf.pane.is_some() && !leaf.launch.is_claude())
            .map(|(id, leaf)| ShellEntry {
                leaf: *id,
                cwd: leaf.launch.cwd.clone(),
                title: self.slots.get(id).map_or_else(|| leaf.launch.label(), |slot| slot.view.read(cx).title()),
            })
            .collect();
        self.sidebar.update(cx, |sidebar, cx| sidebar.set_shells(shells, cx));
    }

    fn on_sidebar_event(&mut self, event: SidebarEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            SidebarEvent::Activate { activation, split } => match activation {
                Activation::FocusPane(pane) => {
                    if let Some(leaf) = self.workspace.leaf_for_pane(pane) {
                        self.focus_leaf(leaf, window, cx);
                    }
                }
                Activation::FocusLeaf(leaf) => self.focus_leaf(leaf, window, cx),
                // A session already in the layout is shown there (and started again if it finished),
                // so one session never runs in two panes.
                Activation::Resume { session, cwd } => match self.workspace.leaf_for_session(&session) {
                    Some(leaf) => {
                        self.focus_leaf(leaf, window, cx);
                        let starting = self.pending.values().any(|waiting| *waiting == leaf);
                        if self.workspace.leaves[&leaf].pane.is_none() && !starting {
                            self.rerun_leaf(leaf, window, cx);
                        }
                    }
                    None => {
                        let axis = split.then_some(Axis::Horizontal);
                        self.open_launch(PaneLaunch::resume(&session, cwd), axis, None, window, cx);
                    }
                },
                Activation::TakeOver { session, cwd } => {
                    let target = match self.workspace.leaf_for_session(&session) {
                        Some(leaf) if self.workspace.leaves[&leaf].pane.is_none() => TakeOverTarget::Leaf(leaf),
                        _ => TakeOverTarget::NewPane { cwd, split },
                    };
                    self.confirm_take_over(session, target, window, cx);
                }
                Activation::ToggleDetails(_)
                | Activation::ToggleDir(_)
                | Activation::ShowAll(_)
                | Activation::Nothing => {}
            },
            SidebarEvent::NewHere { dir, claude } => self.open_launch(new_launch(claude, dir), None, None, window, cx),
            SidebarEvent::ViewChanged(view) => {
                self.workspace.sidebar.view = view;
                self.save(cx);
            }
            SidebarEvent::Dismiss => self.focus_current(window, cx),
        }
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, window: &mut Window, cx: &mut Context<Self>) {
        let state = &mut self.workspace.sidebar;
        state.visible = !state.visible;
        if !state.visible {
            self.focus_current(window, cx);
        }
        self.save(cx);
        cx.notify();
    }

    fn find_session(&mut self, _: &FindSession, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.sidebar.visible = true;
        self.sidebar.focus_handle(cx).focus(window, cx);
        self.save(cx);
        cx.notify();
    }

    fn drag_sidebar(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.resizing_sidebar {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.resizing_sidebar = false;
            self.save(cx);
            return;
        }
        let width = f32::from(event.position.x - self.sidebar_bounds.origin.x);
        self.workspace.sidebar.width = width.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        cx.notify();
    }

    fn open_launcher(&mut self, _: &OpenLauncher, window: &mut Window, cx: &mut Context<Self>) {
        let sessions: Vec<SessionInfo> = self.sidebar.read(cx).sessions().all().cloned().collect();
        let view = cx.new(|cx| LauncherView::new(sessions, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &LauncherEvent, window, cx| {
            this.on_launcher_event(event.clone(), window, cx)
        });
        view.focus_handle(cx).focus(window, cx);
        self.launcher = Some((view, subscription));
        cx.notify();
    }

    fn on_launcher_event(&mut self, event: LauncherEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.launcher = None;
        match event {
            LauncherEvent::Open { dir, claude } => self.open_launch(new_launch(claude, dir), None, None, window, cx),
            LauncherEvent::Browse { claude } => {
                let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
                cx.spawn_in(window, async move |this, cx| {
                    let dir = match picked.await {
                        Ok(Ok(Some(paths))) => paths.into_iter().next(),
                        _ => None,
                    };
                    let _ = this.update_in(cx, |this, window, cx| match dir {
                        Some(dir) => this.open_launch(new_launch(claude, dir), None, None, window, cx),
                        None => this.focus_current(window, cx),
                    });
                })
                .detach();
            }
            LauncherEvent::Dismiss => self.focus_current(window, cx),
        }
        cx.notify();
    }

    fn running_panes(&self, leaves: &[NodeId], cx: &Context<Self>) -> usize {
        leaves.iter().filter(|leaf| self.slots.get(leaf).is_some_and(|slot| slot.view.read(cx).is_running())).count()
    }

    /// Window close requested: true to close now; otherwise asks Detach / Stop all / Cancel.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let all: Vec<NodeId> = self.workspace.leaves.keys().copied().collect();
        if self.running_panes(&all, cx) == 0 {
            return true;
        }
        if self.prompting.replace(true) {
            return false;
        }
        let answer = window.prompt(
            PromptLevel::Info,
            "Quit ccm?",
            Some("Sessions keep running after Detach."),
            &["Detach", "Stop all sessions", "Cancel"],
            cx,
        );
        let (client, prompting, layout_file) = (self.client.clone(), self.prompting.clone(), self.layout_file.clone());
        cx.spawn(async move |_, cx| match answer.await {
            Ok(0) => cx.update(|cx| cx.quit()),
            Ok(1) => {
                if let Err(err) = layout_store::clear(&layout_file) {
                    tracing::warn!("clearing the layout failed: {err}");
                }
                client.stop_all_and_wait(Duration::from_secs(2));
                cx.update(|cx| cx.quit());
            }
            _ => prompting.set(false),
        })
        .detach();
        false
    }

    fn new_claude_tab(&mut self, _: &NewClaudeTab, window: &mut Window, cx: &mut Context<Self>) { self.open(true, None, window, cx) }
    fn new_shell_tab(&mut self, _: &NewShellTab, window: &mut Window, cx: &mut Context<Self>) { self.open(false, None, window, cx) }
    fn split_right_claude(&mut self, _: &SplitRightClaude, window: &mut Window, cx: &mut Context<Self>) { self.open(true, Some(Axis::Horizontal), window, cx) }
    fn split_right_shell(&mut self, _: &SplitRightShell, window: &mut Window, cx: &mut Context<Self>) { self.open(false, Some(Axis::Horizontal), window, cx) }
    fn split_down_claude(&mut self, _: &SplitDownClaude, window: &mut Window, cx: &mut Context<Self>) { self.open(true, Some(Axis::Vertical), window, cx) }
    fn split_down_shell(&mut self, _: &SplitDownShell, window: &mut Window, cx: &mut Context<Self>) { self.open(false, Some(Axis::Vertical), window, cx) }
    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(leaf) = self.workspace.focused() {
            self.confirm_close(vec![leaf], window, cx);
        }
    }
    fn focus_dir(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.focus_direction(direction).is_some() {
            self.focus_current(window, cx);
            self.save(cx);
        }
    }
    fn focus_left(&mut self, _: &FocusLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_dir(Direction::Left, window, cx)
    }
    fn focus_right(&mut self, _: &FocusRight, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_dir(Direction::Right, window, cx)
    }
    fn focus_up(&mut self, _: &FocusUp, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_dir(Direction::Up, window, cx)
    }
    fn focus_down(&mut self, _: &FocusDown, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_dir(Direction::Down, window, cx)
    }
    fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.cycle_tab(delta);
        self.focus_current(window, cx);
        self.save(cx);
    }
    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle(-1, window, cx)
    }
    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle(1, window, cx)
    }
    fn move_tab_left(&mut self, _: &MoveTabLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.workspace.move_tab(-1);
        self.save(cx);
        cx.notify();
    }
    fn move_tab_right(&mut self, _: &MoveTabRight, _: &mut Window, cx: &mut Context<Self>) {
        self.workspace.move_tab(1);
        self.save(cx);
        cx.notify();
    }
    fn activate_tab_1(&mut self, _: &ActivateTab1, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab(0, window, cx)
    }
    fn activate_tab_2(&mut self, _: &ActivateTab2, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab(1, window, cx)
    }
    fn activate_tab_3(&mut self, _: &ActivateTab3, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab(2, window, cx)
    }
    fn activate_tab_4(&mut self, _: &ActivateTab4, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab(3, window, cx)
    }
    fn activate_tab_5(&mut self, _: &ActivateTab5, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab(4, window, cx)
    }
    fn activate_tab_6(&mut self, _: &ActivateTab6, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab(5, window, cx)
    }
    fn activate_tab_7(&mut self, _: &ActivateTab7, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab(6, window, cx)
    }
    fn activate_tab_8(&mut self, _: &ActivateTab8, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab(7, window, cx)
    }
    fn activate_tab_9(&mut self, _: &ActivateTab9, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab(8, window, cx)
    }

    /// Close `leaves`, asking first if any still runs a process.
    fn confirm_close(&mut self, leaves: Vec<NodeId>, window: &mut Window, cx: &mut Context<Self>) {
        let running = self.running_panes(&leaves, cx);
        if running == 0 {
            for leaf in leaves {
                self.close_leaf(leaf, window, cx);
            }
            return;
        }
        let (title, detail) = if leaves.len() == 1 {
            ("Close pane?", "Its process is still running.".to_string())
        } else {
            ("Close tab?", format!("{running} running process(es) will be stopped."))
        };
        let answer = window.prompt(PromptLevel::Warning, title, Some(&detail), &["Close", "Cancel"], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| {
                    for leaf in leaves {
                        this.close_leaf(leaf, window, cx);
                    }
                });
            }
        })
        .detach();
    }

    fn drag_divider(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(divider) = self.dragging else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.dragging = None;
            self.save(cx);
            return;
        }
        let bounds = self.content_bounds;
        let ratio = match divider.axis {
            Axis::Horizontal => {
                let x = f32::from(event.position.x - bounds.origin.x) / f32::from(bounds.size.width);
                (x - divider.area.x) / divider.area.w
            }
            Axis::Vertical => {
                let y = f32::from(event.position.y - bounds.origin.y) / f32::from(bounds.size.height);
                (y - divider.area.y) / divider.area.h
            }
        };
        if ratio.is_finite() {
            self.workspace.set_ratio(divider.split, ratio);
            cx.notify();
        }
    }

    fn render_tab_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });
        let sessions = self.sidebar.read(cx).sessions();
        let tabs: Vec<(String, Option<SessionStatus>, bool)> = self
            .workspace
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let title = self.slots.get(&tab.focused).map_or_else(String::new, |slot| slot.view.read(cx).title());
                let status = self.workspace.tab_status(index, sessions).cloned();
                let unseen = tab.root.leaves().iter().any(|leaf| self.unseen.contains_key(leaf));
                (title, status, unseen)
            })
            .collect();
        div()
            .id("tab-bar")
            .h(px(TAB_BAR_HEIGHT))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(BORDER))
            .text_sm()
            .children(tabs.into_iter().enumerate().map(|(index, (title, status, unseen))| {
                let active = index == self.workspace.active;
                div()
                    .id(("tab", index))
                    .h_full()
                    .px_3()
                    .max_w(px(220.))
                    // Tabs shrink (titles truncate) so the buttons and window controls stay visible.
                    .min_w(px(48.))
                    .flex_shrink(1.)
                    .flex()
                    .items_center()
                    .border_r_1()
                    .border_color(rgb(BORDER))
                    .text_color(rgb(if active { TEXT } else { MUTED }))
                    .when(active, |tab| tab.bg(rgb(BACKGROUND)).border_t_2().border_color(rgb(ACCENT)))
                    .when_some(status, |tab, status| {
                        let (glyph, color) = status_glyph(Some(&status));
                        tab.child(div().flex_none().mr_1().text_color(rgb(color)).child(glyph))
                    })
                    .child(div().min_w_0().truncate().child(title))
                    .when(unseen, |tab| tab.child(div().flex_none().ml_1().text_color(rgb(AMBER)).child("•")))
                    .child(
                        div()
                            .id(("close-tab", index))
                            .flex_none()
                            .ml_2()
                            .text_color(rgb(MUTED))
                            .hover(|style| style.text_color(rgb(ACCENT)))
                            .child("✕")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                let leaves = this.workspace.tabs.get(index).map(|tab| tab.root.leaves()).unwrap_or_default();
                                this.confirm_close(leaves, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.select_tab(index, window, cx)))
            }))
            .child(tab_bar_button("new-claude", "+ Claude", cx.listener(|this, _, window, cx| this.open(true, None, window, cx))))
            .child(tab_bar_button("new-shell", "+ Shell", cx.listener(|this, _, window, cx| this.open(false, None, window, cx))))
            .when(!client_decorations, |bar| bar.child(div().flex_1().h_full()))
            .when(client_decorations, |bar| {
                bar.child(
                    div()
                        .id("drag-area")
                        .flex_1()
                        .min_w(px(40.))
                        .h_full()
                        .on_mouse_down(MouseButton::Left, |event, window, _| {
                            if event.click_count >= 2 {
                                window.zoom_window();
                            } else {
                                window.start_window_move();
                            }
                        })
                        .on_mouse_down(MouseButton::Right, |event, window, _| window.show_window_menu(event.position)),
                )
                .child(window_frame::window_controls(
                    window,
                    cx.listener(|this, _, window, cx| {
                        if this.request_quit(window, cx) {
                            cx.quit();
                        }
                    }),
                ))
            })
    }

    fn render_active_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(tab) = self.workspace.active_tab() else {
            return div()
                .flex_1()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .items_center()
                .justify_center()
                .text_color(rgb(TEXT))
                .child(tab_bar_button("empty-claude", "New Claude", cx.listener(|this, _, window, cx| this.open(true, None, window, cx))))
                .child(tab_bar_button("empty-shell", "New shell", cx.listener(|this, _, window, cx| this.open(false, None, window, cx))))
                .into_any_element();
        };
        let multiple = tab.root.leaves().len() > 1;
        div()
            .id("tab-content")
            .relative()
            .flex_1()
            .w_full()
            .bg(rgb(BORDER))
            .child({
                let view = cx.entity();
                canvas(move |bounds, _, cx| view.update(cx, |this, _| this.content_bounds = bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full()
            })
            .children(tab.root.layout(Rect::UNIT).into_iter().filter_map(|(leaf, rect)| {
                let view = self.slots.get(&leaf)?.view.clone();
                let focused = leaf == tab.focused;
                let title = view.read(cx).title();
                let status = self.workspace.leaf_status(leaf, self.sidebar.read(cx).sessions()).cloned();
                Some(
                    div()
                        .absolute()
                        .left(relative(rect.x))
                        .top(relative(rect.y))
                        .w(relative(rect.w))
                        .h(relative(rect.h))
                        .p(px(1.))
                        .child(
                            div()
                                .size_full()
                                .flex()
                                .flex_col()
                                .bg(rgb(BACKGROUND))
                                .when(multiple, |pane| pane.child(pane_header(title, focused, status)))
                                .child(div().flex_1().min_h_0().child(view)),
                        ),
                )
            }))
            .children(tab.root.dividers(Rect::UNIT).into_iter().map(|divider| {
                let handle = div().id(("divider", divider.split as usize)).absolute();
                let handle = match divider.axis {
                    Axis::Horizontal => handle
                        .left(relative(divider.area.x + divider.area.w * divider.ratio))
                        .top(relative(divider.area.y))
                        .h(relative(divider.area.h))
                        .w(px(6.))
                        .ml(px(-3.))
                        .cursor(CursorStyle::ResizeLeftRight),
                    Axis::Vertical => handle
                        .top(relative(divider.area.y + divider.area.h * divider.ratio))
                        .left(relative(divider.area.x))
                        .w(relative(divider.area.w))
                        .h(px(6.))
                        .mt(px(-3.))
                        .cursor(CursorStyle::ResizeUpDown),
                };
                handle.on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                    this.dragging = Some(divider);
                    cx.stop_propagation();
                }))
            }))
            .on_mouse_move(cx.listener(Self::drag_divider))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if this.dragging.take().is_some() {
                    this.save(cx);
                }
            }))
            .into_any_element()
    }
}

/// Glyph and color for a live session's status: ● needs input, ◐ busy, ✓ idle, · unknown.
pub(crate) fn status_glyph(status: Option<&SessionStatus>) -> (&'static str, u32) {
    match status {
        Some(SessionStatus::NeedsInput { .. }) => ("●", AMBER),
        Some(SessionStatus::Busy) => ("◐", ACCENT),
        Some(SessionStatus::Idle) => ("✓", TEXT),
        None => ("·", MUTED),
    }
}

/// A new Claude session (or the user's shell) in `cwd`.
pub(crate) fn new_launch(claude: bool, cwd: PathBuf) -> PaneLaunch {
    if claude { PaneLaunch::claude(cwd) } else { PaneLaunch { program: default_shell(), args: Vec::new(), cwd } }
}

fn pane_header(title: String, focused: bool, status: Option<SessionStatus>) -> impl IntoElement {
    div()
        .h(px(22.))
        .px_2()
        .flex()
        .items_center()
        .text_xs()
        .bg(rgb(PANEL))
        .text_color(rgb(if focused { ACCENT } else { MUTED }))
        .when_some(status, |header, status| {
            let (glyph, color) = status_glyph(Some(&status));
            header.child(div().flex_none().mr_1().text_color(rgb(color)).child(glyph))
        })
        .child(div().truncate().child(title))
}

pub(crate) fn tab_bar_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .h_full()
        .px_3()
        .flex()
        .items_center()
        .text_color(rgb(MUTED))
        .hover(|style| style.text_color(rgb(ACCENT)))
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = self.workspace.sidebar.clone();
        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(self.render_tab_bar(window, cx))
            .child(self.render_active_tab(cx));
        let root = div()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::new_claude_tab))
            .on_action(cx.listener(Self::new_shell_tab))
            .on_action(cx.listener(Self::split_right_claude))
            .on_action(cx.listener(Self::split_right_shell))
            .on_action(cx.listener(Self::split_down_claude))
            .on_action(cx.listener(Self::split_down_shell))
            .on_action(cx.listener(Self::close_pane))
            .on_action(cx.listener(Self::focus_left))
            .on_action(cx.listener(Self::focus_right))
            .on_action(cx.listener(Self::focus_up))
            .on_action(cx.listener(Self::focus_down))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::move_tab_left))
            .on_action(cx.listener(Self::move_tab_right))
            .on_action(cx.listener(Self::activate_tab_1))
            .on_action(cx.listener(Self::activate_tab_2))
            .on_action(cx.listener(Self::activate_tab_3))
            .on_action(cx.listener(Self::activate_tab_4))
            .on_action(cx.listener(Self::activate_tab_5))
            .on_action(cx.listener(Self::activate_tab_6))
            .on_action(cx.listener(Self::activate_tab_7))
            .on_action(cx.listener(Self::activate_tab_8))
            .on_action(cx.listener(Self::activate_tab_9))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::find_session))
            .on_action(cx.listener(Self::open_launcher))
            .on_mouse_move(cx.listener(Self::drag_sidebar))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if std::mem::take(&mut this.resizing_sidebar) {
                    this.save(cx);
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .bg(rgb(BACKGROUND))
            .when(sidebar.visible, |root| {
                root.child(
                    div()
                        .relative()
                        .flex_none()
                        .h_full()
                        .w(px(sidebar.width))
                        .child({
                            let view = cx.entity();
                            canvas(move |bounds, _, cx| view.update(cx, |this, _| this.sidebar_bounds = bounds), |_, _, _, _| {})
                                .absolute()
                                .size_full()
                        })
                        .child(self.sidebar.clone()),
                )
                .child(
                    div()
                        .id("sidebar-edge")
                        .flex_none()
                        .w(px(4.))
                        .h_full()
                        .bg(rgb(BORDER))
                        .cursor(CursorStyle::ResizeLeftRight)
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                            this.resizing_sidebar = true;
                            cx.stop_propagation();
                        })),
                )
            })
            .child(main)
            .when_some(self.launcher.as_ref().map(|(view, _)| view.clone()), |root, view| {
                root.child(
                    div().id("launcher-overlay").occlude().absolute().top_0().left_0().size_full().flex().flex_col().items_center().pt(px(72.)).child(view),
                )
            });
        window_frame::frame(window, root)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use gpui::Keystroke;

    fn action_for(keys: &str) -> Option<&'static str> {
        let keystroke = Keystroke::parse(keys).unwrap();
        key_bindings()
            .into_iter()
            .find(|binding| binding.match_keystrokes(std::slice::from_ref(&keystroke)) == Some(false))
            .map(|binding| binding.action().name())
    }

    #[test]
    fn workspace_shortcuts_match_the_spec() {
        for (keys, action) in [
            ("ctrl-shift-t", "workspace::NewClaudeTab"),
            ("ctrl-shift-alt-t", "workspace::NewShellTab"),
            ("ctrl-shift-d", "workspace::SplitRightClaude"),
            ("ctrl-shift-alt-d", "workspace::SplitRightShell"),
            ("ctrl-shift-e", "workspace::SplitDownClaude"),
            ("ctrl-shift-alt-e", "workspace::SplitDownShell"),
            ("ctrl-shift-w", "workspace::ClosePane"),
            ("ctrl-shift-left", "workspace::FocusLeft"),
            ("ctrl-shift-down", "workspace::FocusDown"),
            ("ctrl-pageup", "workspace::PreviousTab"),
            ("ctrl-pagedown", "workspace::NextTab"),
            ("ctrl-shift-pageup", "workspace::MoveTabLeft"),
            ("ctrl-shift-pagedown", "workspace::MoveTabRight"),
            ("alt-1", "workspace::ActivateTab1"),
            ("alt-9", "workspace::ActivateTab9"),
            ("ctrl-shift-b", "workspace::ToggleSidebar"),
            ("ctrl-shift-f", "workspace::FindSession"),
            ("ctrl-shift-o", "workspace::OpenLauncher"),
        ] {
            assert_eq!(action_for(keys), Some(action), "{keys}");
        }
    }

    #[test]
    fn plain_control_keys_stay_with_the_pane() {
        for keys in ["ctrl-c", "ctrl-r", "ctrl-d", "ctrl-w", "escape", "shift-tab"] {
            assert_eq!(action_for(keys), None, "{keys}");
        }
    }
}
