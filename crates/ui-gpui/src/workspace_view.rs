//! The window's root view: tabs of split panes, routing daemon events to them, and saving the layout.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use app_core::attention::{Alert, AlertKind, AttentionTracker, alert_channels, describe, without_claude_glyph};
use app_core::launcher::{self, OpenFolder, OpenPane};
use app_core::layout_store;
use app_core::settings::ThemeChoice;
use app_core::split_tree::{Axis, Direction, Divider, NodeId, Rect};
use app_core::theme_colors::{Preset, terminal_palette};
use app_core::workspace::{ClaudeDirs, DirBadge, PaneLaunch, Rerun, Restored, Workspace};
use daemon::client::{ClientEvent, Connection, DaemonClient, batch_events};
use daemon::containers::GONE as CONTAINER_GONE;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Decorations, Div, Entity, FocusHandle, Focusable, FontWeight,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, PathPromptOptions, Pixels, Point, PromptLevel, Render,
    Subscription, Task, Window, canvas, div, prelude::*, px, relative, rgb, rgba, svg,
};
use protocol::message::{
    ContainerChoice, PaletteColors, PaneId, SessionInfo, SessionState, SessionStatus, TakeOverOutcome,
};

use app_core::menus::{self, MenuAction, MenuContext, MenuEntry, MenuItem, Shortcut};
use app_core::session_tree::{
    Activation, PaneEntry, TabEntry, Tone, display_path, session_title, status_tone, tone_word,
};
use app_core::settings::Settings;
use app_core::shortcuts::{self, Platform};

use crate::alerts::{self, Notice};
use crate::assets::PROMPT_ICON;
use crate::context_menu::{self, OpenMenu};
use crate::hover_card::{self, HoverCard};
use crate::launcher_view::{LauncherBrowse, LauncherEvent, LauncherView};
use crate::recents_view::{RecentsEvent, RecentsView, SEARCH_LIMIT};
use crate::settings_store::{SettingsStore, dir_badge_for};
use crate::settings_view::{SettingsEvent, SettingsView};
use crate::setup_view::{Mode as SetupMode, SetupEvent, SetupView, opens_by_itself};
use crate::sidebar_view::{SidebarEvent, SidebarView};
use crate::terminal_view::{PaneEvent, TerminalView, terminal_binding};
use crate::theme::{
    RADIUS, RADIUS_LARGE, RADIUS_SMALL, Theme, UI_FONT, container_icon, dir_badge, side_line, sweeping, theme,
    working_sweep,
};
use crate::window_frame;

/// Height of the title bar.
pub(crate) const TITLE_BAR_HEIGHT: f32 = 36.0;
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
            NewClaudeTab,
            NewShellTab,
            SplitRightClaude,
            SplitRightShell,
            SplitDownClaude,
            SplitDownShell,
            ClosePane,
            FocusLeft,
            FocusRight,
            FocusUp,
            FocusDown,
            PreviousTab,
            NextTab,
            ToggleSidebar,
            FindSession,
            OpenLauncher,
            OpenRecents,
            OpenSettings
        ]
    );
}
use workspace_actions::*;

/// Every key binding: the workspace and terminal actions at their resolved keystrokes (defaults, the user's overrides
/// from `settings.json`, `null` = none). The text fields and menus handle their keys themselves, so nothing else is bound.
pub(crate) fn all_bindings(settings: &Settings, platform: Platform) -> Vec<gpui::KeyBinding> {
    shortcuts::bindings(&settings.shortcuts, platform)
        .into_iter()
        .filter_map(|(id, key)| workspace_binding(id, &key).or_else(|| terminal_binding(id, &key)))
        // Browse inside the launcher; its own context wins over the Workspace binding of the same chord (Cmd+O on macOS).
        .chain([gpui::KeyBinding::new("secondary-o", LauncherBrowse, Some("Launcher"))])
        .collect()
}

fn workspace_binding(id: &str, key: &str) -> Option<gpui::KeyBinding> {
    use gpui::KeyBinding;
    let context = Some("Workspace");
    Some(match id {
        "newClaudeTab" => KeyBinding::new(key, NewClaudeTab, context),
        "newShellTab" => KeyBinding::new(key, NewShellTab, context),
        "nextTab" => KeyBinding::new(key, NextTab, context),
        "previousTab" => KeyBinding::new(key, PreviousTab, context),
        "splitRightClaude" => KeyBinding::new(key, SplitRightClaude, context),
        "splitRightShell" => KeyBinding::new(key, SplitRightShell, context),
        "splitDownClaude" => KeyBinding::new(key, SplitDownClaude, context),
        "splitDownShell" => KeyBinding::new(key, SplitDownShell, context),
        "closePane" => KeyBinding::new(key, ClosePane, context),
        "focusLeft" => KeyBinding::new(key, FocusLeft, context),
        "focusRight" => KeyBinding::new(key, FocusRight, context),
        "focusUp" => KeyBinding::new(key, FocusUp, context),
        "focusDown" => KeyBinding::new(key, FocusDown, context),
        "openLauncher" => KeyBinding::new(key, OpenLauncher, context),
        "findSession" => KeyBinding::new(key, FindSession, context),
        "openRecents" => KeyBinding::new(key, OpenRecents, context),
        "toggleSidebar" => KeyBinding::new(key, ToggleSidebar, context),
        "openSettings" => KeyBinding::new(key, OpenSettings, context),
        _ => return None,
    })
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
    /// The config dir chosen for this one launch (launcher chip or menu), by name.
    pick: Option<String>,
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
    /// Percent of the conversation index built while the daemon builds it.
    search_indexing: Option<u8>,
    /// Whether the daemon said conversation search does not work.
    search_unavailable: bool,
    client: DaemonClient,
    layout_file: PathBuf,
    default_cwd: PathBuf,
    /// The daemon's own Claude config dir (its panes need no `CLAUDE_CONFIG_DIR`).
    daemon_dir: Option<PathBuf>,
    /// The Claude config dirs in effect, from the settings.
    dirs: ClaudeDirs,
    /// The paths last sent to the daemon with `SetClaudeDirs`.
    sent_dirs: Option<Vec<PathBuf>>,
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
    /// The open setup wizard overlay.
    setup: Option<(Entity<SetupView>, Subscription)>,
    /// The Settings page, while its tab is open.
    settings: Option<Entity<SettingsView>>,
    /// Listens to the open Settings page.
    _settings_events: Option<Subscription>,
    /// The leaf that had focus when Settings opened; closing Settings goes back to it.
    before_settings: Option<NodeId>,
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
    /// Leaves marked unread by hand: they keep the mark, even in view, until Claude's status changes or the user marks
    /// them read.
    marked_unread: HashSet<NodeId>,
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
        let Connection { client, events, claude_dir: daemon_dir, .. } = connection;
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
        // The title bar shows what the sidebar's rows say, so it redraws with them.
        cx.observe(&sidebar, |_, _, cx| cx.notify()).detach();
        // Coming back to the window counts as seeing its visible tab.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            this.clear_seen(window, cx);
            this.sidebar.update(cx, |_, cx| cx.notify());
        });
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
            search_indexing: None,
            search_unavailable: false,
            client,
            layout_file,
            default_cwd,
            dirs: ClaudeDirs::from_settings(
                cx.global::<SettingsStore>().settings(),
                daemon_dir.as_deref(),
                std::env::home_dir().as_deref(),
            ),
            daemon_dir,
            sent_dirs: None,
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
            setup: None,
            settings: None,
            _settings_events: None,
            before_settings: None,
            tab_menu: None,
            take_overs: HashMap::new(),
            pending_opens: HashMap::new(),
            attention: AttentionTracker::default(),
            unseen: HashMap::new(),
            marked_unread: HashSet::new(),
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
        let store = cx.global::<SettingsStore>();
        if opens_by_itself(store.settings(), store.broken()) {
            this.show_setup(SetupMode::FirstRun, window, cx);
        }
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
        let settings = cx.global::<SettingsStore>().settings();
        self.dirs = ClaudeDirs::from_settings(settings, self.daemon_dir.as_deref(), std::env::home_dir().as_deref());
        let configured: Vec<PathBuf> = if settings.claude_dirs.is_empty() {
            Vec::new()
        } else {
            self.dirs.dirs.iter().map(|dir| dir.path.clone()).collect()
        };
        if self.sent_dirs.as_ref() != Some(&configured) {
            self.client.set_claude_dirs(configured.clone());
            self.sent_dirs = Some(configured);
        }
        let seconds = cx.global::<SettingsStore>().settings().alerts.finished_after_seconds;
        self.attention.set_finished_after_ms(u64::from(seconds) * 1000);
        cx.notify();
    }

    fn add_slot(&mut self, leaf: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let launch = self.workspace.leaves[&leaf].launch.clone();
        let client = self.client.clone();
        let view = cx.new(|cx| TerminalView::new(client, launch, window, cx));
        let subscription = cx.subscribe_in(&view, window, move |this, _, event: &PaneEvent, window, cx| {
            this.on_pane_event(leaf, *event, window, cx)
        });
        self.slots.insert(leaf, Slot { view, _subscription: subscription });
    }

    fn spawn_leaf(&mut self, leaf: NodeId, cwd_from: Option<PaneId>, cx: &mut Context<Self>) {
        let Some(slot) = self.slots.get(&leaf) else { return };
        let (cols, rows) = slot.view.read(cx).grid_size();
        let scrollback = cx.global::<SettingsStore>().settings().window.scrollback;
        self.next_request += 1;
        self.pending.insert(self.next_request, leaf);
        self.client
            .spawn(self.next_request, self.workspace.leaves[&leaf].launch.request(cwd_from, cols, rows, scrollback));
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
                    self.drop_marks_of_changed(upserted, cx);
                    let alerts = self.attention.update(upserted, removed);
                    let upserted = upserted.clone();
                    self.sidebar.update(cx, |sidebar, cx| sidebar.apply(upserted, removed, cx));
                    for alert in alerts {
                        self.raise(alert, window, cx);
                    }
                }
                ClientEvent::TakeOverResult { session, outcome } => {
                    self.on_take_over(session.clone(), outcome.clone(), window, cx)
                }
                ClientEvent::ContainerFound { request, container } => {
                    self.on_container_found(*request, container.clone(), window, cx)
                }
                ClientEvent::SearchResults { request, hits } => {
                    if let Some((view, _)) = &self.recents {
                        let hits = hits.clone();
                        view.update(cx, |view, cx| view.set_hits(*request, hits, cx));
                    }
                }
                ClientEvent::SearchIndexing { done, total } => {
                    self.search_indexing = (done < total).then(|| (done * 100 / (*total).max(1)) as u8);
                    self.push_search_state(cx);
                }
                ClientEvent::SearchUnavailable { .. } => {
                    self.search_unavailable = true;
                    self.push_search_state(cx);
                }
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

    /// Drop the hand-made unread mark of every session whose status is no longer Idle.
    fn drop_marks_of_changed(&mut self, sessions: &[SessionInfo], cx: &mut Context<Self>) {
        if self.marked_unread.is_empty() {
            return;
        }
        for session in sessions {
            let SessionState::Managed { pane } = session.state else { continue };
            let Some(leaf) = self.workspace.leaf_for_pane(pane) else { continue };
            if status_tone(session.status.as_ref(), false) != Tone::Idle && self.marked_unread.remove(&leaf) {
                self.unseen.remove(&leaf);
                self.sync_unseen(cx);
            }
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
            .map(|session| {
                (session_title(session).to_string(), display_path(&session.cwd, std::env::home_dir().as_deref()))
            })
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
        forget_seen(&mut self.unseen, &self.marked_unread, |leaf| workspace.is_visible(leaf));
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

    /// The badge of the Claude config dir `leaf` runs in, when two or more are configured.
    fn leaf_dir_badge(&self, leaf: NodeId, cx: &App) -> Option<DirBadge> {
        let entry = self.workspace.leaves.get(&leaf)?;
        if !entry.launch.is_claude() || entry.launch.container.is_some() {
            return None;
        }
        let recorded = entry
            .session
            .as_deref()
            .and_then(|id| self.sidebar.read(cx).sessions().get(id))
            .and_then(|info| info.claude_dir.clone());
        let path = recorded.or_else(|| entry.launch.claude_dir.clone()).or_else(|| self.daemon_dir.clone());
        dir_badge_for(cx.global::<SettingsStore>().settings(), path.as_deref())
    }

    /// The colour of the project `leaf` belongs to (its session's directory, else the pane's), if project colours are on.
    fn leaf_project_color(&self, leaf: NodeId, cx: &App) -> Option<u32> {
        let cwd = self.leaf_cwd(leaf, cx)?;
        theme(cx).project(&self.sidebar.read(cx).project_of(&cwd))
    }

    /// The directory `leaf` works in: its session's, else the pane's.
    fn leaf_cwd(&self, leaf: NodeId, cx: &App) -> Option<PathBuf> {
        let entry = self.workspace.leaves.get(&leaf)?;
        let sessions = self.sidebar.read(cx).sessions();
        Some(
            entry
                .session
                .as_deref()
                .and_then(|id| sessions.get(id))
                .map_or(&entry.launch.cwd, |info| &info.cwd)
                .clone(),
        )
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
        match entry.rerun_with(self.sidebar.read(cx).sessions(), &self.dirs, self.daemon_dir.as_deref()) {
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
    fn confirm_take_over(
        &mut self,
        session: String,
        target: TakeOverTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                    let launch = self.resume_launch(&session, cwd, container, cx);
                    self.open_launch(launch, axis, None, window, cx);
                }
                Some(TakeOverTarget::Leaf(leaf)) => {
                    let session_dir =
                        self.sidebar.read(cx).sessions().get(&session).and_then(|info| info.claude_dir.clone());
                    let launch = self.workspace.leaves.get(&leaf).map(|entry| {
                        entry.rerun_resolved(true, session_dir.as_deref(), &self.dirs, self.daemon_dir.as_deref())
                    });
                    if let Some(launch) = launch {
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
                let answer =
                    window.prompt(PromptLevel::Critical, "Could not take over the session", Some(&error), &["OK"], cx);
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
        self.open_new(claude, cwd, axis, cwd_from, None);
    }

    /// The Settings tab is the active one (it has no pane to split).
    fn settings_active(&self) -> bool {
        self.workspace.settings_tab() == Some(self.workspace.active)
    }

    /// Start a new Claude or shell pane in `cwd` — after asking where, when a dev container mounts `cwd`.
    fn open_new(
        &mut self,
        claude: bool,
        cwd: PathBuf,
        axis: Option<Axis>,
        cwd_from: Option<PaneId>,
        pick: Option<String>,
    ) {
        self.next_request += 1;
        self.client.find_container(self.next_request, &cwd);
        self.pending_opens.insert(self.next_request, PendingOpen { claude, pick, cwd, axis, cwd_from });
    }

    /// Start a new Claude session (or shell) in `cwd`: Claude with the config dir `pick` names, else the one remembered
    /// for exactly this folder, else the default.
    fn new_launch(
        &self,
        claude: bool,
        cwd: PathBuf,
        pick: Option<&str>,
        container: Option<PathBuf>,
        cx: &App,
    ) -> PaneLaunch {
        let settings = cx.global::<SettingsStore>().settings();
        if !claude {
            return PaneLaunch::shell(cwd, &settings.shell).in_container(container);
        }
        let dir = self.dirs.for_folder(&cwd, pick, &settings.folder_claude_dirs);
        PaneLaunch::claude_with(cwd, dir, self.daemon_dir.as_deref(), container)
    }

    /// Resume `session` where it ran: with the command and config dir recorded for it.
    fn resume_launch(&self, session: &str, cwd: PathBuf, container: Option<PathBuf>, cx: &App) -> PaneLaunch {
        let session_dir = self.sidebar.read(cx).sessions().get(session).and_then(|info| info.claude_dir.clone());
        PaneLaunch::resume_with(session, cwd, container, session_dir.as_deref(), &self.dirs, self.daemon_dir.as_deref())
    }

    /// A new session from the launcher; an explicit config dir pick is remembered for the folder.
    fn open_launcher_pick(&mut self, claude: bool, dir: PathBuf, dir_pick: Option<String>, cx: &mut Context<Self>) {
        if let Some(name) = &dir_pick {
            self.remember_pick(&dir, name, cx);
        }
        self.open_new(claude, dir, None, None, dir_pick);
    }

    /// An explicit pick of a config dir for `folder` is remembered for it; picking the default forgets it.
    fn remember_pick(&self, folder: &Path, name: &str, cx: &mut App) {
        let is_default = self.dirs.dirs.get(self.dirs.default).is_some_and(|dir| dir.name == name);
        let (folder, name) = (folder.to_path_buf(), name.to_string());
        SettingsStore::update(cx, move |settings| {
            if is_default {
                settings.folder_claude_dirs.remove(&folder);
            } else {
                settings.folder_claude_dirs.insert(folder, name);
            }
        });
    }

    /// The daemon said which dev container (if any) mounts a waiting session's directory: open it locally, or ask.
    fn on_container_found(
        &mut self,
        request: u64,
        container: Option<ContainerChoice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(PendingOpen { claude, pick, cwd, axis, cwd_from }) = self.pending_opens.remove(&request) else {
            return;
        };
        let Some(container) = container else {
            let launch = self.new_launch(claude, cwd, pick.as_deref(), None, cx);
            self.open_launch(launch, axis, cwd_from, window, cx);
            return;
        };
        let folder = &container.host_folder;
        let project =
            folder.file_name().map_or_else(|| folder.display().to_string(), |name| name.to_string_lossy().into_owned());
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
                Ok(0) => {
                    // Splits of a container pane start in the session's directory, not the docker client's.
                    let launch = this.new_launch(claude, cwd, pick.as_deref(), Some(container.host_folder), cx);
                    this.open_launch(launch, axis, None, window, cx)
                }
                Ok(1) => {
                    let launch = this.new_launch(claude, cwd, pick.as_deref(), None, cx);
                    this.open_launch(launch, axis, cwd_from, window, cx)
                }
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
        self.marked_unread.remove(&leaf);
        if self.unseen.remove(&leaf).is_some() {
            self.sync_unseen(cx);
        }
        self.pending.retain(|_, waiting| *waiting != leaf);
        self.focus_current(window, cx);
        self.save(cx);
    }

    fn focus_leaf(&mut self, leaf: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.focus(leaf) {
            self.focus_current(window, cx);
            self.save(cx);
        }
    }

    /// Give keyboard focus to the workspace's focused leaf (or the empty window).
    fn focus_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((setup, _)) = &self.setup {
            setup.focus_handle(cx).focus(window, cx);
            return;
        }
        let (open, active) = (self.workspace.settings_tab().is_some(), self.settings_active());
        self.sidebar.update(cx, |sidebar, cx| sidebar.set_settings(open, active, cx));
        let settings =
            self.workspace.active_tab().is_some_and(|tab| tab.settings).then(|| self.settings.clone()).flatten();
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

    /// Write the layout (atomically) and refresh the sidebar's pane entries. With "Start with one new tab" the
    /// finished leaves dropped at startup are written back too, so switching back restores them.
    fn save(&self, cx: &mut Context<Self>) {
        if let Err(err) = layout_store::save(&self.layout_file, &self.workspace) {
            tracing::warn!("saving the layout failed: {err}");
        }
        self.sync_panes(cx);
    }

    /// Tab `index`'s title as the title bar and the sidebar show it: the focused pane's, without the Claude glyph
    /// while the tab has a session status.
    fn tab_title(&self, index: usize, cx: &App) -> String {
        let Some(tab) = self.workspace.tabs.get(index) else { return String::new() };
        if tab.settings {
            return "Settings".to_string();
        }
        let title = self.slots.get(&tab.focused).map_or_else(String::new, |slot| slot.view.read(cx).title());
        if self.workspace.tab_status(index, self.sidebar.read(cx).sessions()).is_some() {
            without_claude_glyph(&title).to_string()
        } else {
            title
        }
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
                    alone: self
                        .workspace
                        .tab_of(*id)
                        .is_some_and(|tab| self.workspace.tabs[tab].root.leaves().len() == 1),
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
            MenuAction::MarkUnread(leaf) => {
                self.unseen.insert(leaf, AlertKind::Finished);
                self.marked_unread.insert(leaf);
                self.sync_unseen(cx);
                cx.notify();
            }
            MenuAction::MarkRead(leaf) => {
                self.unseen.remove(&leaf);
                self.marked_unread.remove(&leaf);
                self.sync_unseen(cx);
                cx.notify();
            }
            MenuAction::ClosePane(leaf) => self.confirm_close(vec![leaf], window, cx),
            MenuAction::CloseTab(index) => self.close_tab_at(index, window, cx),
            MenuAction::NewHere { dir, claude } => self.open_new(claude, dir, None, None, None),
            MenuAction::NewClaudeWith { dir, dir_name } => {
                self.remember_pick(&dir, &dir_name, cx);
                self.open_new(true, dir, None, None, Some(dir_name));
            }
            MenuAction::PinFavorite(dir) => SettingsStore::update(cx, |settings| {
                if !settings.favorites.contains(&dir) {
                    settings.favorites.push(dir);
                }
            }),
            MenuAction::UnpinFavorite(dir) => {
                SettingsStore::update(cx, |settings| settings.favorites.retain(|favorite| *favorite != dir))
            }
            MenuAction::SetProjectColor { root, color } => SettingsStore::update(cx, |settings| match color {
                Some(color) => {
                    settings.project_colors.folders.insert(root, color);
                }
                None => {
                    settings.project_colors.folders.remove(&root);
                }
            }),
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
            vec![MenuItem::Entry(MenuEntry {
                label: "Close tab".into(),
                shortcut: Some(Shortcut::Close),
                accent: false,
                action: MenuAction::CloseTab(index),
                swatch: None,
                checked: false,
            })]
        } else {
            let Some(leaf) = self.workspace.leaves.get(&tab.focused) else { return };
            let sessions = self.sidebar.read(cx).sessions();
            let place = leaf.session.as_deref().and_then(|id| sessions.get(id)).and_then(|info| info.container.clone());
            let vscode = menus::vscode_args(&leaf.launch.cwd, place.as_ref(), leaf.launch.container.as_deref());
            let context = if leaf.launch.container.is_some() {
                MenuContext::default()
            } else {
                MenuContext::for_folder(cx.global::<SettingsStore>().settings(), &leaf.launch.cwd)
            };
            let idle = leaf.session.as_deref().and_then(|id| sessions.get(id)).is_some_and(|info| {
                matches!(info.state, SessionState::Managed { .. })
                    && status_tone(info.status.as_ref(), false) == Tone::Idle
            });
            let unread = self.unseen.contains_key(&tab.focused);
            let mark = (idle || unread).then_some((tab.focused, unread));
            menus::tab_menu(index, &leaf.launch.cwd, vscode, &context, mark)
        };
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.tab_menu = Some(OpenMenu::new(items, position, focus));
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
                            let container = sessions
                                .get(&session)
                                .and_then(|info| info.container.clone())
                                .and_then(|place| place.host_folder);
                            TakeOverTarget::NewPane { cwd, split, container }
                        }
                    };
                    self.confirm_take_over(session, target, window, cx);
                }
                Activation::ToggleDetails(_) | Activation::ToggleProject(_) | Activation::Nothing => {}
            },
            SidebarEvent::Menu(action) => self.run_menu_action(action, window, cx),
            SidebarEvent::TabMenu { index, position } => self.open_tab_menu(index, position, window, cx),
            SidebarEvent::New { claude } => self.show_launcher(claude, window, cx),
            SidebarEvent::OpenRecents => self.open_recents(&OpenRecents, window, cx),
            SidebarEvent::Dismiss => self.focus_current(window, cx),
            SidebarEvent::ShowSettings => self.open_settings(&OpenSettings, window, cx),
            SidebarEvent::CloseSettings => self.close_settings(window, cx),
        }
    }

    /// Resume `session`: show the leaf that already has it (starting it again if it finished), else open it in a
    /// new tab (or a split) where it ran — its dev container included.
    fn resume_session(
        &mut self,
        session: String,
        cwd: PathBuf,
        split: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                        let launch = self.resume_launch(&session, cwd, container, cx);
                        self.open_launch(launch, axis, None, window, cx);
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
        self.show_launcher(true, window, cx);
    }

    /// The folders with live panes, in sidebar order, for the launcher's "Open now".
    fn open_folders(&self, cx: &Context<Self>) -> Vec<OpenFolder> {
        let sessions = self.sidebar.read(cx).sessions();
        let order = self.sidebar.read(cx).leaf_order();
        let mut ids: Vec<NodeId> =
            self.workspace.tabs.iter().filter(|tab| !tab.settings).flat_map(|tab| tab.root.leaves()).collect();
        ids.sort_by_key(|id| order.iter().position(|ordered| ordered == id).unwrap_or(usize::MAX));
        let panes: Vec<OpenPane> = ids
            .into_iter()
            .filter_map(|id| {
                let leaf = self.workspace.leaves.get(&id).filter(|leaf| leaf.pane.is_some())?;
                let status = leaf
                    .session
                    .as_deref()
                    .and_then(|session| sessions.get(session))
                    .and_then(|info| info.status.as_ref());
                Some(OpenPane {
                    cwd: leaf.launch.cwd.clone(),
                    claude: leaf.launch.is_claude(),
                    tone: status_tone(status, self.unseen.contains_key(&id)),
                })
            })
            .collect();
        launcher::open_folders(&panes)
    }

    /// Open the launcher; `claude_first`: Enter starts Claude and Alt+Enter a shell, false swaps them.
    fn show_launcher(&mut self, claude_first: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.setup.is_some() {
            return;
        }
        self.recents = None;
        let sessions: Vec<SessionInfo> = self.sidebar.read(cx).sessions().all().cloned().collect();
        let open = self.open_folders(cx);
        let view = cx.new(|cx| LauncherView::new(open, sessions, claude_first, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &LauncherEvent, window, cx| {
            this.on_launcher_event(event.clone(), window, cx)
        });
        view.focus_handle(cx).focus(window, cx);
        self.launcher = Some((view, subscription));
        cx.notify();
    }

    /// Open the setup wizard; `mode`: it opened by itself on the first start, or from Settings.
    fn show_setup(&mut self, mode: SetupMode, window: &mut Window, cx: &mut Context<Self>) {
        self.launcher = None;
        self.recents = None;
        let view = cx.new(|cx| SetupView::new(mode, self.daemon_dir.clone(), cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &SetupEvent, window, cx| match event {
            SetupEvent::Close => {
                this.setup = None;
                this.focus_current(window, cx);
                cx.notify();
            }
        });
        view.focus_handle(cx).focus(window, cx);
        self.setup = Some((view, subscription));
        cx.notify();
    }

    fn open_recents(&mut self, _: &OpenRecents, window: &mut Window, cx: &mut Context<Self>) {
        self.show_recents(None, window, cx);
    }

    /// Open the recent sessions pop-up, on `project`'s sessions when given.
    fn show_recents(&mut self, project: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if self.setup.is_some() {
            return;
        }
        self.launcher = None;
        let sessions: Vec<SessionInfo> = self.sidebar.read(cx).sessions().all().cloned().collect();
        let open: HashSet<String> = self.workspace.leaves.values().filter_map(|leaf| leaf.session.clone()).collect();
        let view = cx.new(|cx| RecentsView::new(sessions, open, project, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &RecentsEvent, window, cx| {
            this.on_recents_event(event.clone(), window, cx)
        });
        view.focus_handle(cx).focus(window, cx);
        self.recents = Some((view, subscription));
        self.push_search_state(cx);
        cx.notify();
    }

    /// Tell an open recent sessions pop-up where conversation search stands.
    fn push_search_state(&self, cx: &mut Context<Self>) {
        if let Some((view, _)) = &self.recents {
            let (indexing, unavailable) = (self.search_indexing, self.search_unavailable);
            view.update(cx, |view, cx| view.set_index_state(indexing, unavailable, cx));
        }
    }

    fn on_recents_event(&mut self, event: RecentsEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            RecentsEvent::Search { request, query } => {
                self.client.search(request, &query, SEARCH_LIMIT);
                return;
            }
            RecentsEvent::Resume { session, cwd, split } => {
                self.recents = None;
                self.resume_session(session, cwd, split, window, cx);
            }
            RecentsEvent::Dismiss => {
                self.recents = None;
                self.focus_current(window, cx);
            }
        }
        cx.notify();
    }

    fn on_launcher_event(&mut self, event: LauncherEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.launcher = None;
        match event {
            LauncherEvent::Open { dir, claude, dir_pick } => self.open_launcher_pick(claude, dir, dir_pick, cx),
            LauncherEvent::Browse { claude, dir_pick } => {
                let picked = cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: None,
                });
                cx.spawn_in(window, async move |this, cx| {
                    let dir = match picked.await {
                        Ok(Ok(Some(paths))) => paths.into_iter().next(),
                        _ => None,
                    };
                    let _ = this.update_in(cx, |this, window, cx| match dir {
                        Some(dir) => this.open_launcher_pick(claude, dir, dir_pick, cx),
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

    fn new_claude_tab(&mut self, _: &NewClaudeTab, _: &mut Window, _: &mut Context<Self>) {
        self.open(true, None)
    }
    fn new_shell_tab(&mut self, _: &NewShellTab, _: &mut Window, _: &mut Context<Self>) {
        self.open(false, None)
    }
    fn split_right_claude(&mut self, _: &SplitRightClaude, _: &mut Window, _: &mut Context<Self>) {
        self.open(true, Some(Axis::Horizontal))
    }
    fn split_right_shell(&mut self, _: &SplitRightShell, _: &mut Window, _: &mut Context<Self>) {
        self.open(false, Some(Axis::Horizontal))
    }
    fn split_down_claude(&mut self, _: &SplitDownClaude, _: &mut Window, _: &mut Context<Self>) {
        self.open(true, Some(Axis::Vertical))
    }
    fn split_down_shell(&mut self, _: &SplitDownShell, _: &mut Window, _: &mut Context<Self>) {
        self.open(false, Some(Axis::Vertical))
    }
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
        let order = self.sidebar.read(cx).leaf_order();
        self.workspace.cycle_tab_in(&order, delta);
        self.focus_current(window, cx);
        self.save(cx);
    }
    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle(-1, window, cx)
    }
    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle(1, window, cx)
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(leaf) = self.workspace.focused() {
            self.before_settings = Some(leaf);
        }
        self.workspace.open_settings();
        if self.settings.is_none() {
            let view = cx.new(|cx| SettingsView::new(self.daemon_dir.clone(), window, cx));
            self._settings_events =
                Some(cx.subscribe_in(&view, window, |this, _, event: &SettingsEvent, window, cx| match event {
                    SettingsEvent::DetectClaudeDirs => this.show_setup(SetupMode::Rerun, window, cx),
                }));
            self.settings = Some(view);
        }
        self.focus_current(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// Close the Settings tab and drop its page; no confirmation, it runs nothing.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let showing = self.settings_active();
        self.workspace.close_settings();
        if let Some(leaf) = self.before_settings.take().filter(|_| showing) {
            self.workspace.focus(leaf);
        }
        self.settings = None;
        self._settings_events = None;
        self.focus_current(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// Close tab `index`: the Settings tab right away, any other after `confirm_close` of its leaves.
    fn close_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.workspace.tabs.get(index) {
            Some(tab) if tab.settings => self.close_settings(window, cx),
            Some(tab) => {
                let leaves = tab.root.leaves();
                self.confirm_close(leaves, window, cx);
            }
            None => {}
        }
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

    fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let client_decorations = matches!(window.window_decorations(), Decorations::Client { .. });
        let theme = theme(cx).clone();
        let waiting = self.sidebar.read(cx).needs_input().len();
        let pill_colors = theme.tone(Tone::NeedsInput);
        let sidebar = &self.workspace.sidebar;
        let spacer = div().id("drag-area").flex_1().min_w(theme.ui(40.)).h_full();
        div()
            .id("title-bar")
            .h(theme.ui(TITLE_BAR_HEIGHT))
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
                // The brand keeps the sidebar's width; the focused session starts where the panes do.
                let brand = div()
                    .id("brand")
                    .flex_none()
                    .h_full()
                    .flex()
                    .items_center()
                    .font_weight(FontWeight::BOLD)
                    .child("Gantry");
                if sidebar.visible {
                    let brand = brand.w(theme.ui(sidebar.width) + px(SIDEBAR_EDGE)).pl(theme.ui(18.));
                    bar.child(if client_decorations { moves_window(brand) } else { brand })
                } else {
                    bar.pl_2().child(if client_decorations { moves_window(brand) } else { brand })
                }
            })
            .child(self.render_title_session(window, cx))
            .child(if client_decorations { moves_window(spacer) } else { spacer })
            .when(waiting > 0, |bar| {
                bar.child(
                    div()
                        .id("need-input")
                        .flex_none()
                        .h(theme.ui(24.))
                        .px(theme.ui(10.))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(pill_colors.ink))
                        .when_some(pill_colors.fill, |pill, fill| pill.bg(rgb(fill)))
                        .cursor_pointer()
                        .child(format!("{} {}", waiting, if waiting == 1 { "needs input" } else { "need input" }))
                        .on_click(cx.listener(|this, _, window, cx| {
                            let next = this.sidebar.read(cx).next_needing_input(this.workspace.focused());
                            if let Some(leaf) = next {
                                this.focus_leaf(leaf, window, cx);
                            }
                        })),
                )
            })
            .child(round_button("new-claude", "+", &theme, cx.listener(|this, _, _, _| this.open(true, None))))
            .child(round_button(
                "open-settings",
                "⚙",
                &theme,
                cx.listener(|this, _, window, cx| this.open_settings(&OpenSettings, window, cx)),
            ))
            .when(client_decorations, |bar| {
                bar.child(window_frame::window_controls(
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

    /// The middle of the title bar: `Settings`, or the project and the focused session's chip.
    fn render_title_session(&self, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let theme = theme(cx).clone();
        let middle = div().min_w_0().flex_shrink(1.).flex().flex_row().items_center().gap(theme.ui(8.));
        let Some(tab) = self.workspace.active_tab() else { return middle };
        if tab.settings {
            // With the sidebar hidden its row is gone, so the bar closes Settings.
            return middle.font_weight(FontWeight::SEMIBOLD).child("Settings").when(
                !self.workspace.sidebar.visible,
                |middle| {
                    middle.child(
                        div()
                            .id("close-settings")
                            .flex_none()
                            .size(theme.ui(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .text_xs()
                            .font_weight(FontWeight::NORMAL)
                            .hover(|style| style.bg(rgb(theme.tokens.raised)))
                            .cursor_pointer()
                            .child("×")
                            .on_click(cx.listener(|this, _, window, cx| this.close_settings(window, cx))),
                    )
                },
            );
        }
        let index = self.workspace.active;
        let leaf = tab.focused;
        let animate = cx.global::<SettingsStore>().settings().animate_working && window.is_window_active();
        let title = self.sidebar.read(cx).leaf_title(leaf).unwrap_or_else(|| self.tab_title(index, cx));
        let status = self.workspace.leaf_status(leaf, self.sidebar.read(cx).sessions()).cloned();
        let unseen = self.unseen.contains_key(&leaf);
        let container = self.workspace.leaves.get(&leaf).is_some_and(|leaf| leaf.launch.container.is_some());
        let dir = self.leaf_dir_badge(leaf, cx);
        let cwd = self.leaf_cwd(leaf, cx);
        let colors = status.as_ref().map(|status| theme.tone(status_tone(Some(status), unseen)));
        let ink = colors.map_or(theme.tokens.text, |colors| colors.ink);
        let sweep = status.as_ref().is_some_and(|status| sweeping(status_tone(Some(status), unseen), animate, true));
        let card = HoverCard {
            title: title.clone(),
            status: status.as_ref().map(|status| tone_word(status_tone(Some(status), unseen)).to_string()),
            dir: dir.clone(),
            container,
            folder: cwd.as_deref().map(|cwd| display_path(cwd, std::env::home_dir().as_deref())),
        };
        let sidebar = self.sidebar.read(cx);
        let project = cwd.as_deref().map(|cwd| {
            let root = sidebar.project_of(cwd);
            let label = sidebar.project_label(&root).unwrap_or_else(|| {
                root.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned())
            });
            (label, theme.project(&root).unwrap_or(theme.tokens.muted))
        });
        middle
            .children(project.map(|(label, color)| {
                div()
                    .min_w_0()
                    .max_w(theme.ui(240.))
                    .flex()
                    .gap(theme.ui(8.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(color))
                            .child(label),
                    )
                    .child(div().flex_none().text_color(rgb(theme.tokens.muted)).child("›"))
            }))
            .child(
                div()
                    .id("title-chip")
                    .tooltip(hover_card::tooltip(card))
                    .relative()
                    .h(theme.ui(26.))
                    .px(theme.ui(10.))
                    .min_w_0()
                    .flex_shrink(1.)
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(RADIUS_SMALL))
                    .text_color(rgb(ink))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when_some(colors.and_then(|colors| colors.fill), |chip, fill| chip.bg(rgb(fill)))
                    .when(sweep, |chip| chip.child(working_sweep("sweep-title", px(RADIUS_SMALL), &theme)))
                    .child(div().min_w_0().truncate().child(title))
                    .when(container, |chip| chip.child(container_icon(ink, &theme)))
                    .children(dir.map(|badge| dir_badge(&badge, &theme)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.open_tab_menu(index, event.position, window, cx);
                            // Keep focus on the menu: the workspace's tracked focus must not take it back.
                            window.prevent_default();
                            cx.stop_propagation();
                        }),
                    ),
            )
    }

    fn render_active_tab(&self, animate: bool, cx: &mut Context<Self>) -> impl IntoElement {
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
                        .on_click(cx.listener(|this, _, window, cx| this.show_launcher(true, window, cx))),
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
                        .on_click(cx.listener(|this, _, window, cx| this.show_launcher(false, window, cx))),
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
            .children(tab.root.layout(Rect::UNIT).into_iter().enumerate().filter_map(|(index, (leaf, rect))| {
                let view = self.slots.get(&leaf)?.view.clone();
                let focused = leaf == tab.focused;
                let title = view.read(cx).title();
                let status = self.workspace.leaf_status(leaf, self.sidebar.read(cx).sessions()).cloned();
                let title = if status.is_some() { without_claude_glyph(&title).to_string() } else { title };
                let unseen = self.unseen.contains_key(&leaf);
                let container = self.workspace.leaves.get(&leaf).is_some_and(|entry| entry.launch.container.is_some());
                let dir = self.leaf_dir_badge(leaf, cx);
                let project = self.leaf_project_color(leaf, cx);
                let folder = self
                    .workspace
                    .leaves
                    .get(&leaf)
                    .map(|entry| display_path(&entry.launch.cwd, std::env::home_dir().as_deref()));
                let last_prompt = if cx.global::<SettingsStore>().settings().window.last_prompt {
                    self.workspace.leaf_last_prompt(leaf, self.sidebar.read(cx).sessions()).map(str::to_string)
                } else {
                    None
                };
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
                                .border_color(rgb(if focused && multiple {
                                    theme.tokens.accent
                                } else {
                                    theme.tokens.line
                                }))
                                .when(multiple, |pane| {
                                    pane.child(pane_header(
                                        index, title, status, unseen, animate, container, dir, folder, project, &theme,
                                    ))
                                })
                                .when_some(last_prompt, |pane, text| pane.child(prompt_card(text, multiple, &theme)))
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
                handle.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.dragging = Some(divider);
                        cx.stop_propagation();
                    }),
                )
            }))
            .on_mouse_move(cx.listener(Self::drag_divider))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.dragging.take().is_some() {
                        this.save(cx);
                    }
                }),
            )
            .into_any_element()
    }
}

/// Your last prompt in a card above a Claude pane's terminal: a fixed two lines, clipped with an ellipsis.
fn prompt_card(text: String, multiple: bool, theme: &Theme) -> Div {
    div()
        .flex_none()
        .h(theme.ui(56.))
        .mb(px(6.))
        .when(multiple, |card| card.mt(px(6.)))
        .px(theme.ui(14.))
        .py(theme.ui(9.))
        .flex()
        .items_start()
        .gap(theme.ui(10.))
        .rounded(px(RADIUS_SMALL))
        .bg(rgb(theme.tokens.surface))
        .border_1()
        .border_color(rgb(theme.tokens.line))
        .child(
            svg()
                .path(PROMPT_ICON)
                .flex_none()
                .mt(theme.ui(2.))
                .size(theme.ui(14.))
                .text_color(rgb(theme.tokens.accent)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(theme.ui(13.))
                .line_height(theme.ui(18.))
                .text_color(rgb(theme.tokens.text))
                .line_clamp(2)
                .text_ellipsis()
                .child(text),
        )
}

/// A split pane's header: filled like its tab, with the session's status in words.
#[allow(clippy::too_many_arguments)]
fn pane_header(
    index: usize,
    title: String,
    status: Option<SessionStatus>,
    unseen: bool,
    animate: bool,
    container: bool,
    dir: Option<DirBadge>,
    folder: Option<String>,
    project: Option<u32>,
    theme: &Theme,
) -> impl IntoElement {
    let colors = status.as_ref().map(|status| theme.tone(status_tone(Some(status), unseen)));
    let sweep = status.as_ref().is_some_and(|status| sweeping(status_tone(Some(status), unseen), animate, true));
    let card = HoverCard {
        title: title.clone(),
        status: status.as_ref().map(|status| tone_word(status_tone(Some(status), unseen)).to_string()),
        dir: dir.clone(),
        container,
        folder,
    };
    div()
        .id(("pane-header", index))
        .tooltip(hover_card::tooltip(card))
        .relative()
        .flex_none()
        .h(theme.ui(28.))
        .mb(theme.ui(4.))
        .when(project.is_some(), |header| header.pl(theme.ui(16.)))
        .when(project.is_none(), |header| header.pl_3())
        .pr_3()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(RADIUS_SMALL))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .bg(rgb(colors.and_then(|colors| colors.fill).unwrap_or(theme.tokens.raised)))
        .text_color(rgb(colors.map_or(theme.tokens.text, |colors| colors.ink)))
        .when(sweep, |header| header.child(working_sweep(("sweep-pane", index), px(RADIUS_SMALL), theme)))
        .when_some(project, |header, color| header.child(side_line(color, theme)))
        .child(div().flex_1().min_w_0().truncate().child(title))
        .when(container, |header| {
            header.child(container_icon(colors.map_or(theme.tokens.text, |colors| colors.ink), theme))
        })
        .children(dir.map(|badge| dir_badge(&badge, theme)))
}

/// Title-bar behaviour for an empty stretch of the title bar (client-side decorations): drag moves the window,
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

/// A small round icon button in the title bar.
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

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let animate = cx.global::<SettingsStore>().settings().animate_working && window.is_window_active();
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
                            canvas(
                                move |bounds, _, cx| view.update(cx, |this, _| this.sidebar_bounds = bounds),
                                |_, _, _, _| {},
                            )
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
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.resizing_sidebar = true;
                                    cx.stop_propagation();
                                }),
                            ),
                    ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .p(px(PANE_GAP / 2.))
                    .child(self.render_active_tab(animate, cx)),
            );
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
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::find_session))
            .on_action(cx.listener(Self::open_launcher))
            .on_action(cx.listener(Self::open_recents))
            .on_action(cx.listener(Self::open_settings))
            // Esc cancels a drag, and no pane or the sidebar sees it.
            .capture_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && cx.has_active_drag() {
                    cx.stop_active_drag(window);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_move(cx.listener(Self::drag_sidebar))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if std::mem::take(&mut this.resizing_sidebar) {
                        this.save(cx);
                    }
                }),
            )
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.tokens.ground))
            .text_color(rgb(theme.tokens.text))
            .font_family(UI_FONT)
            .child(self.render_title_bar(window, cx))
            .child(body)
            .when_some(self.launcher.as_ref().map(|(view, _)| view.clone()), |root, view| {
                root.child(
                    div()
                        .id("launcher-overlay")
                        .occlude()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .pt(theme.ui(72.))
                        .child(view),
                )
            })
            .when_some(self.recents.as_ref().map(|(view, _)| view.clone()), |root, view| {
                root.child(
                    div()
                        .id("recents-overlay")
                        .occlude()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .pt(theme.ui(72.))
                        .child(view),
                )
            })
            .when_some(self.setup.as_ref().map(|(view, _)| view.clone()), |root, view| {
                root.child(
                    div()
                        .id("setup-overlay")
                        .occlude()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(theme.ui(16.))
                        .bg(rgba((theme.tokens.ground << 8) | 0xcc))
                        .child(view),
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
                    cx.global::<SettingsStore>().settings(),
                    move |action, window, cx| pick(action, window, cx),
                    move |window, cx| dismiss(&(), window, cx),
                ))
            });
        window_frame::frame(window, theme.tokens.line, root)
    }
}

/// Drop the alerts of the leaves the user can see, except those marked unread by hand.
fn forget_seen(unseen: &mut HashMap<NodeId, AlertKind>, marked: &HashSet<NodeId>, visible: impl Fn(NodeId) -> bool) {
    unseen.retain(|leaf, _| !visible(*leaf) || marked.contains(leaf));
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use gpui::Keystroke;

    fn action_for(keys: &str) -> Option<&'static str> {
        action_in(&all_bindings(&Settings::default(), Platform::Linux), keys)
    }

    fn action_in(bindings: &[gpui::KeyBinding], keys: &str) -> Option<&'static str> {
        let keystroke = Keystroke::parse(keys).unwrap();
        bindings
            .iter()
            .find(|binding| binding.match_keystrokes(std::slice::from_ref(&keystroke)) == Some(false))
            .map(|binding| binding.action().name())
    }

    #[test]
    fn a_leaf_marked_unread_keeps_its_alert_in_view_while_others_are_forgotten() {
        let (marked, seen): (NodeId, NodeId) = (1, 2);
        let mut unseen = HashMap::from([(marked, AlertKind::Finished), (seen, AlertKind::Finished)]);
        forget_seen(&mut unseen, &HashSet::from([marked]), |_| true);
        assert_eq!(unseen.keys().copied().collect::<Vec<_>>(), vec![marked]);
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
            ("ctrl-tab", "workspace::NextTab"),
            ("ctrl-shift-tab", "workspace::PreviousTab"),
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
        for keys in ["ctrl-c", "ctrl-r", "ctrl-d", "ctrl-w", "escape", "shift-tab", "tab", "alt-1"] {
            assert_eq!(action_for(keys), None, "{keys}");
        }
    }

    #[test]
    fn ctrl_page_keys_no_longer_switch_tabs() {
        assert_eq!(action_for("ctrl-pageup"), None);
        assert_eq!(action_for("ctrl-pagedown"), None);
    }

    #[test]
    fn no_default_binding_is_reserved() {
        let bindings = shortcuts::bindings(&Default::default(), Platform::Linux);
        for (id, key) in &bindings {
            if key == "ctrl-+" {
                continue;
            }
            assert!(!matches!(shortcuts::check(key, id, &[]), shortcuts::Check::Reserved(_)), "{id} {key}");
        }
    }

    #[test]
    fn an_override_replaces_the_default_and_the_old_keystroke_is_gone() {
        let mut settings = Settings::default();
        settings.shortcuts.insert("toggleSidebar".into(), Some("ctrl-alt-s".into()));
        let bindings = all_bindings(&settings, Platform::Linux);
        assert_eq!(action_in(&bindings, "ctrl-alt-s"), Some("workspace::ToggleSidebar"));
        assert_eq!(action_in(&bindings, "ctrl-shift-b"), None);
        assert_eq!(action_in(&bindings, "ctrl-shift-f"), Some("workspace::FindSession"), "others keep theirs");
    }

    #[test]
    fn null_removes_the_binding() {
        let mut settings = Settings::default();
        settings.shortcuts.insert("closePane".into(), None);
        settings.shortcuts.insert("copy".into(), None);
        let bindings = all_bindings(&settings, Platform::Linux);
        assert_eq!(action_in(&bindings, "ctrl-shift-w"), None);
        assert_eq!(action_in(&bindings, "ctrl-shift-c"), None);
        assert_eq!(action_in(&bindings, "ctrl-shift-v"), Some("terminal::Paste"));
    }

    #[test]
    fn terminal_actions_are_bound() {
        let bindings = all_bindings(&Settings::default(), Platform::Linux);
        assert_eq!(action_in(&bindings, "ctrl-shift-c"), Some("terminal::Copy"));
        assert_eq!(action_in(&bindings, "ctrl-="), Some("terminal::ZoomIn"));
    }
}
