//! The window's root view: tabs of split panes, routing daemon events to them, and saving the layout.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use app_core::attention::{Alert, AlertKind, AttentionTracker, alert_channels, describe, without_claude_glyph};
use app_core::layout_store;
use app_core::settings::ThemeChoice;
use app_core::split_tree::{Axis, Direction, Divider, NodeId, Rect};
use app_core::theme_colors::{Preset, terminal_palette};
use app_core::workspace::{PaneLaunch, Rerun, Restored, Workspace};
use daemon::client::{ClientEvent, Connection, DaemonClient, batch_events};
use daemon::containers::GONE as CONTAINER_GONE;
use daemon::pty::default_shell;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Decorations, Entity, FocusHandle, Focusable, FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels,
    PathPromptOptions, Point, PromptLevel, Render, Subscription, Task, Window, canvas, div, prelude::*, px, relative, rgb, rgba,
};
use protocol::message::{ContainerChoice, PaletteColors, PaneId, SessionInfo, SessionState, SessionStatus, TakeOverOutcome};

use app_core::menus::{self, MenuAction, MenuEntry, MenuItem, Shortcut};
use app_core::session_tree::{Activation, PaneEntry, TabEntry, Tone, display_path, session_title, status_tone};

use crate::alerts::{self, Notice};
use crate::context_menu::{self, OpenMenu};
use crate::launcher_view::{LauncherEvent, LauncherView};
use crate::recents_view::{RecentsEvent, RecentsView};
use crate::sidebar_view::{SidebarEvent, SidebarView};
use crate::terminal_view::{PaneEvent, TerminalView};
use crate::settings_store::SettingsStore;
use crate::settings_view::SettingsView;
use crate::theme::{RADIUS, RADIUS_LARGE, RADIUS_SMALL, Theme, UI_FONT, container_chip, theme};
use crate::window_frame;

pub(crate) const TAB_BAR_HEIGHT: f32 = 48.0;
/// Space between split panes (and around them).
const PANE_GAP: f32 = 8.0;
/// Width of the sidebar's drag edge.
const SIDEBAR_EDGE: f32 = 1.0;
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
            ActivateTab8, ActivateTab9, ToggleSidebar, FindSession, OpenLauncher,
            OpenRecents, OpenSettings
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
    let (m, prev_tab, next_tab, tab_n, settings_key) = ("cmd", "cmd-shift-[", "cmd-shift-]", "cmd", "cmd-,");
    #[cfg(not(target_os = "macos"))]
    let (m, prev_tab, next_tab, tab_n, settings_key) = ("ctrl-shift", "ctrl-pageup", "ctrl-pagedown", "alt", "ctrl-,");
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
        // Every platform, alongside the platform's own tab keys.
        KeyBinding::new("ctrl-tab", NextTab, context),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, context),
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
        KeyBinding::new(&format!("{m}-r"), OpenRecents, context),
        KeyBinding::new(settings_key, OpenSettings, context),
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
        /// Host folder of the dev container it ran in.
        container: Option<PathBuf>,
    },
    /// The finished leaf that last ran it (Continue).
    Leaf(NodeId),
}

/// A new session waiting to learn whether a dev container mounts its directory.
struct PendingOpen {
    claude: bool,
    cwd: PathBuf,
    axis: Option<Axis>,
    cwd_from: Option<PaneId>,
}

/// Root view of the Gantry window.
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
    /// The open recent sessions overlay.
    recents: Option<(Entity<RecentsView>, Subscription)>,
    /// The Settings page, while its tab is open.
    settings: Option<Entity<SettingsView>>,
    /// The open right-click menu of a tab.
    tab_menu: Option<OpenMenu>,
    /// Take-over requests waiting for the daemon, by session.
    take_overs: HashMap<String, TakeOverTarget>,
    /// New sessions waiting for `ContainerFound`, by request id.
    pending_opens: HashMap<u64, PendingOpen>,
    /// Status changes seen so far, to tell which ones alert.
    attention: AttentionTracker,
    /// Leaves with an alert the user has not seen yet.
    unseen: HashMap<NodeId, AlertKind>,
    _activation: Subscription,
    /// The theme whose terminal colours the daemon was last told.
    sent_palette: Option<Preset>,
    _theme: Subscription,
    _appearance: Subscription,
    /// Leaves whose notification was clicked.
    clicked: async_channel::Sender<NodeId>,
    _clicks: Task<()>,
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
        let sidebar = cx.new(SidebarView::new);
        let sidebar_events = cx.subscribe_in(&sidebar, window, |this, _, event: &SidebarEvent, window, cx| {
            this.on_sidebar_event(event.clone(), window, cx)
        });
        // Coming back to the window counts as seeing its visible tab.
        let activation = cx.observe_window_activation(window, |this, window, cx| this.clear_seen(window, cx));
        let theme_changed = cx.observe_global_in::<Theme>(window, |this, _, cx| this.apply_settings(cx));
        // Following the system: a light/dark switch resolves the theme again.
        let appearance = cx.observe_window_appearance(window, |_, _, cx| {
            if cx.global::<SettingsStore>().settings().theme == ThemeChoice::System {
                SettingsStore::reapply(cx);
            }
        });
        let (clicked, clicks) = async_channel::unbounded::<NodeId>();
        let clicks_task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(leaf) = clicks.recv().await {
                let _ = this.update_in(cx, |this, window, cx| {
                    window.activate_window();
                    this.focus_leaf(leaf, window, cx);
                });
            }
        });
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
            recents: None,
            settings: None,
            tab_menu: None,
            take_overs: HashMap::new(),
            pending_opens: HashMap::new(),
            attention: AttentionTracker::default(),
            unseen: HashMap::new(),
            _activation: activation,
            sent_palette: None,
            _theme: theme_changed,
            _appearance: appearance,
            clicked,
            _clicks: clicks_task,
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
        // Every connection starts with the GUI's colours: the daemon outlives the GUI.
        this.apply_settings(cx);
        this.save(cx);
        this
    }

    /// Take over what the settings say that is not drawn by `render`: pane colours, the daemon's palette, and the
    /// alert threshold.
    fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let preset = theme(cx).preset;
        for slot in self.slots.values() {
            slot.view.update(cx, |view, cx| view.apply_theme(cx));
        }
        if self.sent_palette != Some(preset) {
            self.client.set_palette(PaletteColors::from(terminal_palette(preset)));
            self.sent_palette = Some(preset);
        }
        let seconds = cx.global::<SettingsStore>().settings().alerts.finished_after_seconds;
        self.attention.set_finished_after_ms(u64::from(seconds) * 1000);
        cx.notify();
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
                ClientEvent::ContainerFound { request, container } => self.on_container_found(*request, container.clone(), window, cx),
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

    /// Raise `alert` on the channels the settings allow for its pane (badge, notification, sound).
    fn raise(&mut self, alert: Alert, window: &mut Window, cx: &mut Context<Self>) {
        let Some(leaf) = self.workspace.leaf_for_pane(alert.pane) else { return };
        let settings = cx.global::<SettingsStore>().settings().clone();
        let visible = self.workspace.is_visible(leaf);
        let channels = alert_channels(&settings.alerts, &alert.kind, visible, window.is_window_active());
        if channels.badge {
            self.unseen.insert(leaf, alert.kind.clone());
            self.sync_unseen(cx);
            self.clear_seen(window, cx);
        }
        if !channels.notification && channels.sound.is_none() {
            return;
        }
        let (title, dir) = self
            .sidebar
            .read(cx)
            .sessions()
            .get(&alert.session)
            .map(|session| (session_title(session).to_string(), display_path(&session.cwd, std::env::home_dir().as_deref())))
            .unwrap_or_default();
        let (summary, body) = describe(&alert.kind, &title, &dir);
        alerts::deliver(Notice { leaf, summary, body }, channels, settings.alerts.volume, self.clicked.clone());
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
    /// outside Gantry on this host, offer to take it over.
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
            Some("Unsent input outside Gantry is lost."),
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
                Some(TakeOverTarget::NewPane { cwd, split, container }) => {
                    let axis = split.then_some(Axis::Horizontal);
                    self.open_launch(PaneLaunch::resume(&session, cwd, container), axis, None, window, cx);
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
                    Some("It ignored the stop request for 5 s."),
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
                self.sync_panes(cx);
                cx.notify()
            }
        }
    }

    /// Open a new Claude or shell pane — as a new tab, or splitting the focused pane along `axis` —
    /// in the focused pane's current directory.
    fn open(&mut self, claude: bool, axis: Option<Axis>) {
        if axis.is_some() && self.settings_active() {
            return;
        }
        let source = self.workspace.focused().and_then(|leaf| self.workspace.leaves.get(&leaf));
        let cwd = source.map_or_else(|| self.default_cwd.clone(), |leaf| leaf.launch.cwd.clone());
        let cwd_from = source.and_then(|leaf| leaf.pane);
        self.open_new(claude, cwd, axis, cwd_from);
    }

    /// The Settings tab is the active one (it has no pane to split).
    fn settings_active(&self) -> bool {
        self.workspace.settings_tab() == Some(self.workspace.active)
    }

    /// Start a new Claude or shell pane in `cwd` — after asking where, when a dev container mounts `cwd`.
    fn open_new(&mut self, claude: bool, cwd: PathBuf, axis: Option<Axis>, cwd_from: Option<PaneId>) {
        self.next_request += 1;
        self.client.find_container(self.next_request, &cwd);
        self.pending_opens.insert(self.next_request, PendingOpen { claude, cwd, axis, cwd_from });
    }

    /// The daemon said which dev container (if any) mounts a waiting session's directory: open it locally, or ask.
    fn on_container_found(&mut self, request: u64, container: Option<ContainerChoice>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(PendingOpen { claude, cwd, axis, cwd_from }) = self.pending_opens.remove(&request) else { return };
        let Some(container) = container else {
            self.open_launch(new_launch(claude, cwd), axis, cwd_from, window, cx);
            return;
        };
        let folder = &container.host_folder;
        let project = folder.file_name().map_or_else(|| folder.display().to_string(), |name| name.to_string_lossy().into_owned());
        let state = if container.running { "running" } else { "stopped, starts on open" };
        let detail = format!("{} · {state}", container.name);
        let answer = window.prompt(
            PromptLevel::Info,
            &format!("Open {project} in"),
            Some(&detail),
            &["Dev container", "This machine", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await;
            let _ = this.update_in(cx, |this, window, cx| match answer {
                // Splits of a container pane start in the session's directory, not the docker client's.
                Ok(0) => this.open_launch(new_launch(claude, cwd).in_container(Some(container.host_folder)), axis, None, window, cx),
                Ok(1) => this.open_launch(new_launch(claude, cwd), axis, cwd_from, window, cx),
                _ => this.focus_current(window, cx),
            });
        })
        .detach();
    }

    /// Say why a container session cannot be opened.
    fn container_gone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(PromptLevel::Info, "Cannot open this session", Some(CONTAINER_GONE), &["OK"], cx);
        cx.spawn(async move |_, _| {
            let _ = answer.await;
        })
        .detach();
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
        if axis.is_some() && self.settings_active() {
            return;
        }
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
        let settings = self.workspace.active_tab().is_some_and(|tab| tab.settings).then(|| self.settings.clone()).flatten();
        if let Some(settings) = settings {
            settings.focus_handle(cx).focus(window, cx);
            self.sidebar.update(cx, |sidebar, cx| sidebar.set_focused(None, cx));
            self.clear_seen(window, cx);
            return;
        }
        match self.workspace.focused().and_then(|leaf| self.slots.get(&leaf).map(|slot| (leaf, slot))) {
            Some((leaf, slot)) => {
                slot.view.focus_handle(cx).focus(window, cx);
                if let Some(pane) = self.workspace.leaves[&leaf].pane {
                    self.client.focus(pane);
                }
            }
            None => self.focus_handle.focus(window, cx),
        }
        let focused = self.workspace.focused();
        self.sidebar.update(cx, |sidebar, cx| sidebar.set_focused(focused, cx));
        self.clear_seen(window, cx);
    }

    /// Write the layout (atomically) and refresh the sidebar's pane entries.
    fn save(&self, cx: &mut Context<Self>) {
        if let Err(err) = layout_store::save(&self.layout_file, &self.workspace) {
            tracing::warn!("saving the layout failed: {err}");
        }
        self.sync_panes(cx);
    }

    /// Tab `index`'s title as the tab bar and the sidebar show it: the focused pane's, without the Claude glyph
    /// while the tab has a session status.
    fn tab_title(&self, index: usize, cx: &App) -> String {
        let Some(tab) = self.workspace.tabs.get(index) else { return String::new() };
        if tab.settings {
            return "Settings".to_string();
        }
        let title = self.slots.get(&tab.focused).map_or_else(String::new, |slot| slot.view.read(cx).title());
        if self.workspace.tab_status(index, self.sidebar.read(cx).sessions()).is_some() { without_claude_glyph(&title).to_string() } else { title }
    }

    fn sync_panes(&self, cx: &mut Context<Self>) {
        let panes: Vec<PaneEntry> = self
            .workspace
            .leaves
            .iter()
            .map(|(id, leaf)| {
                let view = self.slots.get(id).map(|slot| slot.view.read(cx));
                PaneEntry {
                    leaf: *id,
                    cwd: leaf.launch.cwd.clone(),
                    title: view.map_or_else(|| leaf.launch.label(), |view| view.title()),
                    session: leaf.session.clone(),
                    claude: leaf.launch.is_claude(),
                    ended: view.is_some_and(|view| view.has_ended()),
                    alone: self.workspace.tab_of(*id).is_some_and(|tab| self.workspace.tabs[tab].root.leaves().len() == 1),
                    container: leaf.launch.container.clone(),
                }
            })
            .collect();
        let tabs: Vec<TabEntry> = self
            .workspace
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| !tab.settings)
            .map(|(index, tab)| TabEntry {
                index,
                title: self.tab_title(index, cx),
                focused: tab.focused,
                leaves: tab.root.leaves(),
            })
            .collect();
        self.sidebar.update(cx, |sidebar, cx| sidebar.set_panes(panes, tabs, cx));
    }

    /// Do what a right-click menu entry says.
    fn run_menu_action(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            MenuAction::Split { tab, axis } => {
                // The right-clicked tab, which becomes the visible one.
                if self.workspace.select_tab(tab) {
                    self.focus_current(window, cx);
                    self.open(true, Some(axis));
                }
            }
            MenuAction::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            MenuAction::OpenFolder(dir) => cx.open_with_system(&dir),
            MenuAction::OpenInVsCode(args) => self.open_in_vscode(args, window, cx),
            MenuAction::ClosePane(leaf) => self.confirm_close(vec![leaf], window, cx),
            MenuAction::CloseTab(index) => {
                match self.workspace.tabs.get(index) {
                    Some(tab) if tab.settings => self.close_settings(window, cx),
                    Some(tab) => {
                        let leaves = tab.root.leaves();
                        self.confirm_close(leaves, window, cx);
                    }
                    None => {}
                }
            }
            MenuAction::NewHere { dir, claude } => self.open_new(claude, dir, None, None),
            MenuAction::RecentsFor(project) => self.show_recents(Some(project), window, cx),
            MenuAction::Continue(leaf) => {
                self.focus_leaf(leaf, window, cx);
                self.rerun_leaf(leaf, window, cx);
            }
            MenuAction::TakeOver { session, cwd } => self.on_sidebar_event(
                SidebarEvent::Activate { activation: Activation::TakeOver { session, cwd }, split: false },
                window,
                cx,
            ),
        }
    }

    /// Start VS Code (`code <args>`), detached; say so if it cannot start.
    fn open_in_vscode(&mut self, args: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let started = std::process::Command::new("code")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match started {
            // `code` hands off to VS Code and exits; reap it so it does not linger as a zombie.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(error) => {
                let detail = format!("Could not start VS Code (`code`): {error}");
                let answer = window.prompt(PromptLevel::Warning, "Open in VS Code", Some(&detail), &["OK"], cx);
                cx.spawn(async move |_, _| {
                    let _ = answer.await;
                })
                .detach();
            }
        }
    }

    /// Open tab `index`'s right-click menu at `position` (it acts on that tab, not the visible one).
    fn open_tab_menu(&mut self, index: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.workspace.tabs.get(index) else { return };
        let items = if tab.settings {
            vec![MenuItem::Entry(MenuEntry { label: "Close tab", shortcut: Some(Shortcut::Close), accent: false, action: MenuAction::CloseTab(index) })]
        } else {
            let Some(leaf) = self.workspace.leaves.get(&tab.focused) else { return };
            let sessions = self.sidebar.read(cx).sessions();
            let place = leaf.session.as_deref().and_then(|id| sessions.get(id)).and_then(|info| info.container.clone());
            let vscode = menus::vscode_args(&leaf.launch.cwd, place.as_ref(), leaf.launch.container.as_deref());
            menus::tab_menu(index, &leaf.launch.cwd, vscode)
        };
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.tab_menu = Some(OpenMenu { items, position, focus });
        cx.notify();
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
                Activation::Resume { session, cwd } => self.resume_session(session, cwd, split, window, cx),
                Activation::TakeOver { session, cwd } => {
                    let target = match self.workspace.leaf_for_session(&session) {
                        Some(leaf) if self.workspace.leaves[&leaf].pane.is_none() => TakeOverTarget::Leaf(leaf),
                        _ => {
                            let sessions = self.sidebar.read(cx).sessions();
                            let container = sessions.get(&session).and_then(|info| info.container.clone()).and_then(|place| place.host_folder);
                            TakeOverTarget::NewPane { cwd, split, container }
                        }
                    };
                    self.confirm_take_over(session, target, window, cx);
                }
                Activation::ToggleDetails(_) | Activation::ToggleProject(_) | Activation::Nothing => {}
            },
            SidebarEvent::Menu(action) => self.run_menu_action(action, window, cx),
            SidebarEvent::TabMenu { index, position } => self.open_tab_menu(index, position, window, cx),
            SidebarEvent::New { claude } => self.open(claude, None),
            SidebarEvent::OpenRecents => self.open_recents(&OpenRecents, window, cx),
            SidebarEvent::Dismiss => self.focus_current(window, cx),
        }
    }

    /// Resume `session`: show the leaf that already has it (starting it again if it finished), else open it in a
    /// new tab (or a split) where it ran — its dev container included.
    fn resume_session(&mut self, session: String, cwd: PathBuf, split: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A session already in the layout is shown there, so one session never runs in two panes.
        match self.workspace.leaf_for_session(&session) {
            Some(leaf) => {
                self.focus_leaf(leaf, window, cx);
                let starting = self.pending.values().any(|waiting| *waiting == leaf);
                if self.workspace.leaves[&leaf].pane.is_none() && !starting {
                    self.rerun_leaf(leaf, window, cx);
                }
            }
            None => {
                let axis = split.then_some(Axis::Horizontal);
                let place = self.sidebar.read(cx).sessions().get(&session).and_then(|info| info.container.clone());
                match place {
                    Some(place) if place.host_folder.is_none() => self.container_gone(window, cx),
                    place => {
                        let container = place.and_then(|place| place.host_folder);
                        self.open_launch(PaneLaunch::resume(&session, cwd, container), axis, None, window, cx);
                    }
                }
            }
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
        let ui_scale = theme(cx).ui_scale;
        if !self.resizing_sidebar {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.resizing_sidebar = false;
            self.save(cx);
            return;
        }
        let width = f32::from(event.position.x - self.sidebar_bounds.origin.x) / ui_scale;
        self.workspace.sidebar.width = width.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        cx.notify();
    }

    fn open_launcher(&mut self, _: &OpenLauncher, window: &mut Window, cx: &mut Context<Self>) {
        self.recents = None;
        let sessions: Vec<SessionInfo> = self.sidebar.read(cx).sessions().all().cloned().collect();
        let view = cx.new(|cx| LauncherView::new(sessions, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &LauncherEvent, window, cx| {
            this.on_launcher_event(event.clone(), window, cx)
        });
        view.focus_handle(cx).focus(window, cx);
        self.launcher = Some((view, subscription));
        cx.notify();
    }

    fn open_recents(&mut self, _: &OpenRecents, window: &mut Window, cx: &mut Context<Self>) {
        self.show_recents(None, window, cx);
    }

    /// Open the recent sessions pop-up, on `project`'s sessions when given.
    fn show_recents(&mut self, project: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.launcher = None;
        let sessions: Vec<SessionInfo> = self.sidebar.read(cx).sessions().all().cloned().collect();
        let open: HashSet<String> = self.workspace.leaves.values().filter_map(|leaf| leaf.session.clone()).collect();
        let view = cx.new(|cx| RecentsView::new(sessions, open, project, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &RecentsEvent, window, cx| {
            this.on_recents_event(event.clone(), window, cx)
        });
        view.focus_handle(cx).focus(window, cx);
        self.recents = Some((view, subscription));
        cx.notify();
    }

    fn on_recents_event(&mut self, event: RecentsEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.recents = None;
        match event {
            RecentsEvent::Resume { session, cwd, split } => self.resume_session(session, cwd, split, window, cx),
            RecentsEvent::Dismiss => self.focus_current(window, cx),
        }
        cx.notify();
    }

    fn on_launcher_event(&mut self, event: LauncherEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.launcher = None;
        match event {
            LauncherEvent::Open { dir, claude } => self.open_new(claude, dir, None, None),
            LauncherEvent::Browse { claude } => {
                let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
                cx.spawn_in(window, async move |this, cx| {
                    let dir = match picked.await {
                        Ok(Ok(Some(paths))) => paths.into_iter().next(),
                        _ => None,
                    };
                    let _ = this.update_in(cx, |this, window, cx| match dir {
                        Some(dir) => this.open_new(claude, dir, None, None),
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
            "Quit Gantry?",
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

    fn new_claude_tab(&mut self, _: &NewClaudeTab, _: &mut Window, _: &mut Context<Self>) { self.open(true, None) }
    fn new_shell_tab(&mut self, _: &NewShellTab, _: &mut Window, _: &mut Context<Self>) { self.open(false, None) }
    fn split_right_claude(&mut self, _: &SplitRightClaude, _: &mut Window, _: &mut Context<Self>) { self.open(true, Some(Axis::Horizontal)) }
    fn split_right_shell(&mut self, _: &SplitRightShell, _: &mut Window, _: &mut Context<Self>) { self.open(false, Some(Axis::Horizontal)) }
    fn split_down_claude(&mut self, _: &SplitDownClaude, _: &mut Window, _: &mut Context<Self>) { self.open(true, Some(Axis::Vertical)) }
    fn split_down_shell(&mut self, _: &SplitDownShell, _: &mut Window, _: &mut Context<Self>) { self.open(false, Some(Axis::Vertical)) }
    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.active_tab().is_some_and(|tab| tab.settings) {
            self.close_settings(window, cx);
        } else if let Some(leaf) = self.workspace.focused() {
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

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.open_settings();
        if self.settings.is_none() {
            self.settings = Some(cx.new(|cx| SettingsView::new(window, cx)));
        }
        self.focus_current(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// Close the Settings tab and drop its page; no confirmation, it runs nothing.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace.close_settings();
        self.settings = None;
        self.focus_current(window, cx);
        self.save(cx);
        cx.notify();
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

    /// A dragged tab was dropped on tab `to`: it takes that place.
    fn drop_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.workspace.move_tab_to(from, to);
        self.save(cx);
        cx.notify();
    }

    fn render_tab_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });
        let theme = theme(cx).clone();
        let sessions = self.sidebar.read(cx).sessions();
        let tabs: Vec<(String, Option<SessionStatus>, bool, bool, bool)> = self
            .workspace
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let title = self.tab_title(index, cx);
                let status = self.workspace.tab_status(index, sessions).cloned();
                let unseen = tab.root.leaves().iter().any(|leaf| self.unseen.contains_key(leaf));
                let container = self.workspace.leaves.get(&tab.focused).is_some_and(|leaf| leaf.launch.container.is_some());
                (title, status, unseen, container, tab.settings)
            })
            .collect();
        let sidebar = &self.workspace.sidebar;
        div()
            .id("tab-bar")
            .h(theme.ui(TAB_BAR_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(theme.ui(6.))
            .bg(rgb(theme.tokens.surface))
            .border_b_1()
            .border_color(rgb(theme.tokens.line))
            .text_sm()
            .map(|bar| {
                if sidebar.visible {
                    // The brand sits above the sidebar; the tabs start where the panes do.
                    let brand = div()
                        .id("brand")
                        .flex_none()
                        .h_full()
                        .w(theme.ui(sidebar.width) + px(SIDEBAR_EDGE))
                        .pl(theme.ui(18.))
                        .flex()
                        .items_center()
                        .font_weight(FontWeight::BOLD)
                        .child("Gantry");
                    bar.child(if client_decorations { moves_window(brand) } else { brand })
                } else {
                    bar.pl_2()
                }
            })
            .children(tabs.into_iter().enumerate().map(|(index, (title, status, unseen, container, settings))| {
                let dragged = DraggedTab { index, title: title.clone() };
                let line = theme.tokens.line;
                let active = index == self.workspace.active;
                // Claude tabs are filled with what their most urgent session needs; shells stay plain.
                let colors = status.as_ref().map(|status| theme.tone(status_tone(Some(status), unseen)));
                let ink = colors.map_or(if active || settings { theme.tokens.text } else { theme.tokens.muted }, |colors| colors.ink);
                let fill = colors.and_then(|colors| colors.fill).or((active || settings).then_some(theme.tokens.raised));
                div()
                    .id(("tab", index))
                    .h(theme.ui(32.))
                    .pl_3()
                    .pr_1()
                    .max_w(theme.ui(220.))
                    // Tabs shrink (titles truncate) so the buttons and window controls stay visible.
                    .min_w(theme.ui(56.))
                    .flex_shrink(1.)
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(RADIUS - 2.))
                    .border_2()
                    .border_color(if active { rgb(theme.tokens.accent) } else { rgba(0) })
                    .text_color(rgb(ink))
                    .when(active, |tab| tab.font_weight(FontWeight::SEMIBOLD))
                    .when_some(fill, |tab, fill| tab.bg(rgb(fill)))
                    .when(fill.is_none(), |tab| tab.hover(|style| style.bg(rgb(theme.tokens.raised))))
                    .cursor_pointer()
                    .when_some(colors, |tab, colors| tab.child(div().flex_none().size(theme.ui(7.)).rounded_full().bg(rgb(colors.dot))))
                    .when(settings, |tab| tab.child(div().flex_none().child("⚙")))
                    .child(div().min_w_0().truncate().child(title))
                    .when(container, |tab| tab.child(container_chip(ink)))
                    .when(unseen, |tab| tab.child(div().flex_none().size(theme.ui(6.)).rounded_full().bg(rgb(ink))))
                    .child(
                        div()
                            .id(("close-tab", index))
                            .flex_none()
                            .size(theme.ui(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .text_xs()
                            .hover(|style| style.bg(rgba(0x00000026)))
                            .child("×")
                            // Pressing × must not start dragging the tab.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                match this.workspace.tabs.get(index) {
                                    Some(tab) if tab.settings => this.close_settings(window, cx),
                                    Some(tab) => {
                                        let leaves = tab.root.leaves();
                                        this.confirm_close(leaves, window, cx);
                                    }
                                    None => {}
                                }
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.select_tab(index, window, cx)))
                    .on_drag(dragged, |tab: &DraggedTab, _offset, _window, cx: &mut App| cx.new(|_| tab.clone()))
                    .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(rgb(line)))
                    .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| this.drop_tab(dragged.index, index, cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_tab_menu(index, event.position, window, cx);
                            // Keep focus on the menu: the workspace's tracked focus must not take it back.
                            window.prevent_default();
                            cx.stop_propagation();
                        }),
                    )
            }))
            .child(round_button("new-claude", "+", &theme, cx.listener(|this, _, _, _| this.open(true, None))))
            .child(round_button("open-settings", "⚙", &theme, cx.listener(|this, _, window, cx| this.open_settings(&OpenSettings, window, cx))))
            .when(!client_decorations, |bar| bar.child(div().flex_1().h_full()))
            .when(client_decorations, |bar| {
                bar.child(
                    moves_window(div().id("drag-area").flex_1().min_w(theme.ui(40.)).h_full()),
                )
                .child(window_frame::window_controls(
                    window,
                    &theme,
                    cx.listener(|this, _, window, cx| {
                        if this.request_quit(window, cx) {
                            cx.quit();
                        }
                    }),
                ))
            })
    }

    fn render_active_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let Some(tab) = self.workspace.active_tab() else {
            return div()
                .flex_1()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .items_center()
                .justify_center()
                .text_sm()
                .child(
                    div()
                        .id("empty-claude")
                        .w(theme.ui(220.))
                        .h(theme.ui(40.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(RADIUS))
                        .bg(rgb(theme.tokens.accent))
                        .text_color(rgb(theme.tokens.on_accent))
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .child("New Claude session")
                        .on_click(cx.listener(|this, _, _, _| this.open(true, None))),
                )
                .child(
                    div()
                        .id("empty-shell")
                        .w(theme.ui(220.))
                        .h(theme.ui(40.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(RADIUS))
                        .border_1()
                        .border_color(rgb(theme.tokens.line))
                        .text_color(rgb(theme.tokens.muted))
                        .cursor_pointer()
                        .hover(|style| style.text_color(rgb(theme.tokens.text)).bg(rgb(theme.tokens.raised)))
                        .child("New shell")
                        .on_click(cx.listener(|this, _, _, _| this.open(false, None))),
                )
                .into_any_element();
        };
        if tab.settings {
            return match self.settings.clone() {
                Some(page) => div().id("tab-content").flex_1().min_h_0().w_full().child(page).into_any_element(),
                None => div().flex_1().into_any_element(),
            };
        }
        let multiple = tab.root.leaves().len() > 1;
        div()
            .id("tab-content")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
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
                let title = if status.is_some() { without_claude_glyph(&title).to_string() } else { title };
                let unseen = self.unseen.contains_key(&leaf);
                let container = self.workspace.leaves.get(&leaf).is_some_and(|entry| entry.launch.container.is_some());
                Some(
                    div()
                        .absolute()
                        .left(relative(rect.x))
                        .top(relative(rect.y))
                        .w(relative(rect.w))
                        .h(relative(rect.h))
                        .p(px(PANE_GAP / 2.))
                        .child(
                            // A rounded card; the terminal sits inside its padding, clear of the corners.
                            div()
                                .size_full()
                                .flex()
                                .flex_col()
                                .p(px(6.))
                                .rounded(px(RADIUS_LARGE))
                                .bg(rgb(theme.tokens.terminal))
                                .border_2()
                                .border_color(rgb(if focused && multiple { theme.tokens.accent } else { theme.tokens.line }))
                                .when(multiple, |pane| pane.child(pane_header(title, status, unseen, container, &theme)))
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

/// A split pane's header: filled like its tab, with the session's status in words.
fn pane_header(title: String, status: Option<SessionStatus>, unseen: bool, container: bool, theme: &Theme) -> impl IntoElement {
    let colors = status.as_ref().map(|status| theme.tone(status_tone(Some(status), unseen)));
    let word = status.as_ref().map(|status| match status_tone(Some(status), unseen) {
        Tone::NeedsInput => "Needs input",
        Tone::Working => "Working",
        Tone::Done => "Done",
        _ => "Idle",
    });
    div()
        .flex_none()
        .h(theme.ui(28.))
        .mb(theme.ui(4.))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(RADIUS_SMALL))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .bg(rgb(colors.and_then(|colors| colors.fill).unwrap_or(theme.tokens.raised)))
        .text_color(rgb(colors.map_or(theme.tokens.text, |colors| colors.ink)))
        .when_some(colors, |header, colors| header.child(div().flex_none().size(theme.ui(7.)).rounded_full().bg(rgb(colors.dot))))
        .child(div().flex_1().min_w_0().truncate().child(title))
        .when(container, |header| header.child(container_chip(colors.map_or(theme.tokens.text, |colors| colors.ink))))
        .when_some(word, |header, word| header.child(div().flex_none().font_weight(FontWeight::NORMAL).child(word)))
}

/// Title-bar behaviour for an empty stretch of the tab bar (client-side decorations): drag moves the window,
/// double-click maximizes, right-click opens the window menu.
fn moves_window(area: gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div> {
    area.on_mouse_down(MouseButton::Left, |event, window, _| {
        if event.click_count >= 2 {
            window.zoom_window();
        } else {
            window.start_window_move();
        }
    })
    .on_mouse_down(MouseButton::Right, |event, window, _| window.show_window_menu(event.position))
}

/// A small round icon button in the tab bar.
pub(crate) fn round_button(
    id: &'static str,
    label: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .size(theme.ui(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_color(rgb(theme.tokens.muted))
        .hover(|style| style.text_color(rgb(theme.tokens.text)).bg(rgb(theme.tokens.raised)))
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

/// A new Claude session (or the user's shell) in `cwd`.
pub(crate) fn new_launch(claude: bool, cwd: PathBuf) -> PaneLaunch {
    if claude { PaneLaunch::claude(cwd) } else { PaneLaunch { program: default_shell(), args: Vec::new(), cwd, container: None } }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        // The interface scales with `uiFontSize`; GPUI's rem is 16 px at the default of 14.
        window.set_rem_size(px(16.0 * theme.ui_scale));
        let sidebar = self.workspace.sidebar.clone();
        let body = div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_row()
            .when(sidebar.visible, |body| {
                body.child(
                    div()
                        .relative()
                        .flex_none()
                        .h_full()
                        .w(theme.ui(sidebar.width))
                        .child({
                            let view = cx.entity();
                            canvas(move |bounds, _, cx| view.update(cx, |this, _| this.sidebar_bounds = bounds), |_, _, _, _| {})
                                .absolute()
                                .size_full()
                        })
                        .child(self.sidebar.clone()),
                )
                .child(
                    // A hairline, grabbed through a wider invisible handle that overlaps the sidebar.
                    div().relative().flex_none().w(px(SIDEBAR_EDGE)).h_full().bg(rgb(theme.tokens.line)).child(
                        div()
                            .id("sidebar-edge")
                            .absolute()
                            .top_0()
                            .left(px(-5.))
                            .w(px(7.))
                            .h_full()
                            .cursor(CursorStyle::ResizeLeftRight)
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                                this.resizing_sidebar = true;
                                cx.stop_propagation();
                            })),
                    ),
                )
            })
            .child(div().flex_1().min_w_0().h_full().flex().flex_col().p(px(PANE_GAP / 2.)).child(self.render_active_tab(cx)));
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
            .on_action(cx.listener(Self::open_recents))
            .on_action(cx.listener(Self::open_settings))
            .on_mouse_move(cx.listener(Self::drag_sidebar))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                if std::mem::take(&mut this.resizing_sidebar) {
                    this.save(cx);
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.tokens.ground))
            .text_color(rgb(theme.tokens.text))
            .font_family(UI_FONT)
            .child(self.render_tab_bar(window, cx))
            .child(body)
            .when_some(self.launcher.as_ref().map(|(view, _)| view.clone()), |root, view| {
                root.child(
                    div().id("launcher-overlay").occlude().absolute().top_0().left_0().size_full().flex().flex_col().items_center().pt(theme.ui(72.)).child(view),
                )
            })
            .when_some(self.recents.as_ref().map(|(view, _)| view.clone()), |root, view| {
                root.child(
                    div().id("recents-overlay").occlude().absolute().top_0().left_0().size_full().flex().flex_col().items_center().pt(theme.ui(72.)).child(view),
                )
            })
            .when_some(self.tab_menu.as_ref(), |root, menu| {
                let pick = cx.listener(|this, action: &MenuAction, window, cx| {
                    this.tab_menu = None;
                    this.focus_current(window, cx);
                    this.run_menu_action(action.clone(), window, cx);
                    cx.notify();
                });
                let dismiss = cx.listener(|this, _: &(), window, cx| {
                    this.tab_menu = None;
                    this.focus_current(window, cx);
                    cx.notify();
                });
                root.child(context_menu::render(
                    "tab-menu",
                    menu,
                    &theme,
                    move |action, window, cx| pick(action, window, cx),
                    move |window, cx| dismiss(&(), window, cx),
                ))
            });
        window_frame::frame(window, theme.tokens.line, root)
    }
}

/// A tab being dragged: its index, and the title its floating copy shows.
#[derive(Clone)]
struct DraggedTab {
    index: usize,
    title: String,
}

impl Render for DraggedTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        div()
            .h(theme.ui(32.))
            .px_3()
            .flex()
            .items_center()
            .rounded(px(RADIUS - 2.))
            .border_2()
            .border_color(rgb(theme.tokens.accent))
            .bg(rgb(theme.tokens.raised))
            .text_sm()
            .font_family(UI_FONT)
            .text_color(rgb(theme.tokens.text))
            .child(self.title.clone())
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
            ("ctrl-tab", "workspace::NextTab"),
            ("ctrl-shift-tab", "workspace::PreviousTab"),
            ("ctrl-shift-pageup", "workspace::MoveTabLeft"),
            ("ctrl-shift-pagedown", "workspace::MoveTabRight"),
            ("alt-1", "workspace::ActivateTab1"),
            ("alt-9", "workspace::ActivateTab9"),
            ("ctrl-shift-b", "workspace::ToggleSidebar"),
            ("ctrl-shift-f", "workspace::FindSession"),
            ("ctrl-shift-o", "workspace::OpenLauncher"),
            ("ctrl-shift-r", "workspace::OpenRecents"),
            ("ctrl-,", "workspace::OpenSettings"),
        ] {
            assert_eq!(action_for(keys), Some(action), "{keys}");
        }
    }

    #[test]
    fn plain_control_keys_stay_with_the_pane() {
        for keys in ["ctrl-c", "ctrl-r", "ctrl-d", "ctrl-w", "escape", "shift-tab", "tab"] {
            assert_eq!(action_for(keys), None, "{keys}");
        }
    }
}
