//! The sidebar: the Active list (every pane and every Claude session running outside Gantry, by project), coloured
//! by what each needs, with a fuzzy filter.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use app_core::menus::{self, MenuAction};
use app_core::session_tree::{
    self, ActiveInput, Activation, Arrivals, PaneEntry, ProjectRoots, Row, RowKey, RowKind, TabEntry, Tone, ToneCounts,
    session_title,
};
use app_core::sessions::SessionStore;
use app_core::split_tree::NodeId;
use app_core::text_field::TextField;
use gpui::{
    App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, Pixels, Point, Render, ScrollStrategy, Stateful, Task, UniformListScrollHandle, Window,
    div, prelude::*, px, rgb, rgba, uniform_list,
};
use protocol::message::SessionInfo;

use crate::context_menu::{self, OpenMenu};
use crate::field;
use crate::theme;
use crate::theme::{
    ACCENT, LINE, MUTED, ON_ACCENT, RADIUS, RADIUS_SMALL, RAISED, SURFACE, TEXT, UI_FONT, tone_colors,
};

/// Every row is this tall (the list is uniform); session rows draw a pill inside it.
const ROW_HEIGHT: f32 = 38.0;
/// Ages ("5m") and project roots are recomputed this often.
const REFRESH: Duration = Duration::from_secs(30);
#[cfg(target_os = "macos")]
const RECENTS_SHORTCUT: &str = "⌘⇧R";
#[cfg(not(target_os = "macos"))]
const RECENTS_SHORTCUT: &str = "Ctrl+Shift+R";

/// What the sidebar asks the workspace to do.
#[derive(Clone, Debug, PartialEq)]
pub enum SidebarEvent {
    /// Focus or resume what a row stands for; `split` = as a split of the focused pane.
    Activate {
        /// What to do.
        activation: Activation,
        /// Open as a split instead of a new tab.
        split: bool,
    },
    /// A right-click menu entry was picked.
    Menu(MenuAction),
    /// Open the right-click menu of tab `index` (a tab heading was right-clicked).
    TabMenu {
        /// The tab.
        index: usize,
        /// Where the menu opens.
        position: Point<Pixels>,
    },
    /// Open a new Claude (or shell) tab where the focused pane is.
    New {
        /// Claude (true) or a shell.
        claude: bool,
    },
    /// Open the recent sessions pop-up.
    OpenRecents,
    /// Give keyboard focus back to the pane.
    Dismiss,
}

/// The session sidebar.
pub struct SidebarView {
    focus_handle: FocusHandle,
    sessions: SessionStore,
    panes: Vec<PaneEntry>,
    tabs: Vec<TabEntry>,
    arrivals: Arrivals,
    filter: TextField,
    toggled: HashSet<PathBuf>,
    details: Option<String>,
    /// Sessions with an alert the user has not seen.
    unseen: HashSet<String>,
    rows: Vec<Row>,
    selected: Option<RowKey>,
    /// The workspace's focused leaf; its row is selected unless the user moved the ring.
    focused_leaf: Option<NodeId>,
    /// The open right-click menu of a row or heading.
    menu: Option<OpenMenu>,
    scroll: UniformListScrollHandle,
    home: Option<PathBuf>,
    /// Project of each directory seen (it stats parent directories; cleared on every refresh).
    roots: ProjectRoots,
    _refresh: Task<()>,
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl Focusable for SidebarView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SidebarView {
    /// An empty sidebar; sessions arrive with `set_sessions`.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                // Also forget project roots, so a repository created (or removed) since shows up.
                let refreshed = this.update(cx, |this, cx| {
                    this.roots.clear();
                    this.rebuild(cx);
                });
                if refreshed.is_err() {
                    return;
                }
            }
        });
        let mut sidebar = SidebarView {
            focus_handle: cx.focus_handle(),
            sessions: SessionStore::default(),
            panes: Vec::new(),
            tabs: Vec::new(),
            arrivals: Arrivals::default(),
            filter: TextField::default(),
            toggled: HashSet::new(),
            details: None,
            unseen: HashSet::new(),
            rows: Vec::new(),
            selected: None,
            focused_leaf: None,
            menu: None,
            scroll: UniformListScrollHandle::new(),
            home: std::env::home_dir(),
            roots: ProjectRoots::new(std::env::home_dir()),
            _refresh: refresh,
        };
        sidebar.rebuild(cx);
        sidebar
    }

    /// The daemon's sessions as last received.
    pub fn sessions(&self) -> &SessionStore {
        &self.sessions
    }

    /// Replace the session list.
    pub fn set_sessions(&mut self, sessions: Vec<SessionInfo>, cx: &mut Context<Self>) {
        self.sessions.replace(sessions);
        self.rebuild(cx);
    }

    /// Apply a session change.
    pub fn apply(&mut self, upserted: Vec<SessionInfo>, removed: &[String], cx: &mut Context<Self>) {
        self.sessions.apply(upserted, removed);
        self.rebuild(cx);
    }

    /// Sessions to mark with the unseen badge.
    pub fn set_unseen(&mut self, unseen: HashSet<String>, cx: &mut Context<Self>) {
        if unseen != self.unseen {
            self.unseen = unseen;
            self.rebuild(cx);
        }
    }

    /// The workspace's focused leaf: its row gets the selection ring.
    pub fn set_focused(&mut self, leaf: Option<NodeId>, cx: &mut Context<Self>) {
        self.focused_leaf = leaf;
        self.selected = leaf.and_then(|leaf| session_tree::row_of_leaf(&self.rows, leaf)).map(|index| self.rows[index].key());
        cx.notify();
    }

    /// The workspace's panes and tabs.
    pub fn set_panes(&mut self, panes: Vec<PaneEntry>, tabs: Vec<TabEntry>, cx: &mut Context<Self>) {
        if panes != self.panes || tabs != self.tabs {
            self.panes = panes;
            self.tabs = tabs;
            self.rebuild(cx);
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let rows = {
            let sessions: Vec<&SessionInfo> = self.sessions.all().collect();
            let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
            let roots = &self.roots;
            let project_of = |cwd: &Path| roots.of(cwd);
            let input = ActiveInput {
                sessions: &sessions,
                panes: &self.panes,
                home: self.home.as_deref(),
                filter: self.filter.text(),
                toggled: &self.toggled,
                details: self.details.as_deref(),
                unseen: &self.unseen,
                tabs: &self.tabs,
                project_of: &project_of,
                now_ms,
            };
            // Until the session list arrives, number nothing: panes and sessions must get their arrival
            // numbers together, so that at startup leaves land above outside sessions.
            let mut unloaded = Arrivals::default();
            let arrivals = if self.sessions.loaded() { &mut self.arrivals } else { &mut unloaded };
            session_tree::active(&input, arrivals)
        };
        self.rows = rows;
        // A selection whose row is gone falls back to the focused pane's row.
        if self.selected_index().is_none() {
            self.selected = self.focused_leaf.and_then(|leaf| session_tree::row_of_leaf(&self.rows, leaf)).map(|index| self.rows[index].key());
        }
        cx.notify();
    }

    fn selected_index(&self) -> Option<usize> {
        session_tree::find(&self.rows, self.selected.as_ref()?)
    }

    /// Move the selection to the next (or previous) selectable row; from nothing, to the first one.
    fn move_selection(&mut self, down: bool) {
        let selectable: Vec<usize> = (0..self.rows.len()).filter(|&i| self.rows[i].selectable()).collect();
        let Some(&first) = selectable.first() else { return };
        let next = match self.selected_index() {
            None => first,
            Some(current) if down => selectable.iter().copied().find(|&i| i > current).unwrap_or(current),
            Some(current) => selectable.iter().rev().copied().find(|&i| i < current).unwrap_or(current),
        };
        self.selected = Some(self.rows[next].key());
        self.scroll.scroll_to_item(next, ScrollStrategy::Center);
    }

    fn activate(&mut self, index: usize, split: bool, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else { return };
        self.selected = Some(row.key());
        match row.activation() {
            Activation::ToggleProject(dir) => {
                if !self.toggled.remove(&dir) {
                    self.toggled.insert(dir);
                }
                self.rebuild(cx);
            }
            Activation::ToggleDetails(id) => {
                self.details = if self.details.as_ref() == Some(&id) { None } else { Some(id) };
                self.rebuild(cx);
            }
            Activation::Nothing => cx.notify(),
            activation => cx.emit(SidebarEvent::Activate { activation, split }),
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "up" => self.move_selection(false),
            "down" => self.move_selection(true),
            "enter" => {
                if let Some(index) = self.selected_index() {
                    self.activate(index, keystroke.modifiers.secondary(), cx);
                }
            }
            "escape" if self.filter.text().is_empty() => cx.emit(SidebarEvent::Dismiss),
            "escape" => {
                self.filter.clear();
                self.rebuild(cx);
            }
            _ => {
                if !field::edit(&mut self.filter, keystroke) {
                    return;
                }
                self.rebuild(cx);
                // Enter right after typing opens the first matching session (not a directory row).
                self.selected = self.rows.iter().position(|row| matches!(row.kind, RowKind::Session { .. } | RowKind::Pane { .. })).map(|index| {
                    self.scroll.scroll_to_item(index, ScrollStrategy::Center);
                    self.rows[index].key()
                });
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Open the right-click menu of row `index` at `position`.
    fn open_menu(&mut self, index: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else { return };
        if let RowKind::Tab { index: tab, .. } = row.kind {
            // The workspace owns the tab menu; only one menu may be open.
            self.menu = None;
            cx.emit(SidebarEvent::TabMenu { index: tab, position });
            cx.notify();
            return;
        }
        let alone = |leaf: NodeId| self.panes.iter().find(|pane| pane.leaf == leaf).is_none_or(|pane| pane.alone);
        let items = match &row.kind {
            RowKind::Project { path, .. } => {
                // The dev container one of its listed sessions runs in.
                let container = self.rows[index + 1..]
                    .iter()
                    .take_while(|row| !matches!(row.kind, RowKind::Project { .. }))
                    .find_map(|row| match &row.kind {
                        RowKind::Session { session, .. } => session.container.as_ref().filter(|place| place.host_folder.is_some()),
                        _ => None,
                    });
                Some(menus::project_menu(path, container))
            }
            _ => menus::row_menu(row, &alone),
        };
        let Some(items) = items else { return };
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.menu = Some(OpenMenu { items, position, focus });
        cx.notify();
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let row = &self.rows[index];
        let key = row.key();
        let selected = self.selected.as_ref() == Some(&key);
        let menu_key = key.clone();
        let base = div()
            .id(index)
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(10.))
            .py(px(2.))
            .flex()
            .flex_row()
            .items_center()
            .text_sm()
            .overflow_hidden()
            .whitespace_nowrap()
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.focus_handle.focus(window, cx);
                // The list may have been rebuilt since it was drawn: act on the row that was
                // clicked, by identity, or on nothing if it is gone.
                if let Some(index) = session_tree::find(&this.rows, &key) {
                    this.activate(index, event.modifiers().secondary(), cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // By identity: the list may have been rebuilt since this row was drawn.
                    if let Some(index) = session_tree::find(&this.rows, &menu_key) {
                        this.open_menu(index, event.position, window, cx);
                        // Keep focus on the menu: the tracked-focus handlers above must not take it back.
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }),
            );
        // The visible part of a row: a rounded pill, ringed when selected.
        let pill = |fill: Option<u32>| {
            div()
                .size_full()
                .px_3()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .rounded(px(RADIUS))
                .border_2()
                .border_color(if selected { rgb(ACCENT) } else { rgba(0) })
                .cursor_pointer()
                .map(|pill| match fill {
                    Some(fill) => pill.bg(rgb(fill)),
                    None => pill.hover(|style| style.bg(rgb(RAISED))),
                })
        };
        let title = |text: String| div().flex_1().min_w_0().truncate().child(text);
        let aside = |text: String, ink: u32| div().flex_none().text_xs().text_color(rgba((ink << 8) | 0xcc)).child(text);
        let guide = row.in_tab.then(|| div().absolute().left(px(20.)).top_0().bottom_0().w(px(2.)).bg(rgb(LINE)));
        let base = base.when(row.in_tab, |base| base.relative().pl(px(32.))).children(guide);
        // A row listed under another project's tab heading names its own project.
        let project = || row.project_label.clone().map(|label| div().flex_none().text_xs().text_color(rgb(MUTED)).child(label));
        match &row.kind {
            RowKind::Project { label, expanded, counts, .. } => {
                base.child(
                    pill(None)
                        .child(div().flex_none().text_color(rgb(MUTED)).child(if *expanded { "▾" } else { "▸" }))
                        .child(title(label.clone()).font_weight(FontWeight::SEMIBOLD))
                        .children(count_chips(counts)),
                )
            }
            RowKind::Session { session, tone, note, .. } => {
                let colors = tone_colors(*tone);
                let unseen = self.unseen.contains(&session.id);
                let lead = match tone {
                    Tone::Elsewhere => div().flex_none().w(px(10.)).text_color(rgb(colors.dot)).child("↗"),
                    Tone::Closed => div().flex_none().size(px(8.)).rounded_full().border_1().border_color(rgb(colors.dot)),
                    _ => div().flex_none().size(px(8.)).rounded_full().bg(rgb(colors.dot)),
                };
                base.child(
                    pill(colors.fill)
                        .text_color(rgb(colors.ink))
                        .child(lead)
                        .child(title(session_title(session).to_string()).when(*tone == Tone::NeedsInput, |title| {
                            title.font_weight(FontWeight::SEMIBOLD)
                        }))
                        .when(unseen, |pill| pill.child(div().flex_none().size(px(7.)).rounded_full().bg(rgb(colors.ink))))
                        .when(session.container.is_some(), |pill| pill.child(theme::container_chip(colors.ink)))
                        .children(project())
                        .child(aside(note.clone(), colors.ink)),
                )
            }
            RowKind::Pane { entry, tone, note } if !entry.claude => base.child(
                pill(None)
                    .text_color(rgb(if *tone == Tone::Closed { MUTED } else { TEXT }))
                    .child(div().flex_none().text_xs().text_color(rgb(MUTED)).child(">_"))
                    .child(title(entry.title.clone()))
                    .children(project())
                    .child(aside(note.clone(), MUTED)),
            ),
            RowKind::Pane { tone, note, .. } => {
                let colors = tone_colors(*tone);
                let lead = match tone {
                    Tone::Closed => div().flex_none().size(px(8.)).rounded_full().border_1().border_color(rgb(colors.dot)),
                    _ => div().flex_none().size(px(8.)).rounded_full().bg(rgb(colors.dot)),
                };
                base.child(
                    pill(colors.fill)
                        .text_color(rgb(colors.ink))
                        .child(lead)
                        .child(title("New session".into()))
                        .children(project())
                        .child(aside(note.clone(), colors.ink)),
                )
            }
            RowKind::Tab { index, title: tab_title, panes, .. } => base.child(
                pill(None)
                    .h(px(28.))
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(18.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_color(rgb(LINE))
                            .rounded(px(RADIUS_SMALL - 3.))
                            .text_xs()
                            .child((index + 1).to_string()),
                    )
                    .child(title(tab_title.clone()).font_weight(FontWeight::SEMIBOLD))
                    .child(aside(format!("{panes} panes"), MUTED)),
            ),
            RowKind::Detail { text, .. } => {
                base.child(div().pl(px(34.)).text_xs().italic().text_color(rgb(MUTED)).child(text.clone()))
            }
            RowKind::TakeOver { .. } => base.child(pill(None).pl(px(32.)).text_color(rgb(ACCENT)).child("Take over…")),
        }
    }
}

/// Small rounded counters in the tones of a project's live sessions.
fn count_chips(counts: &ToneCounts) -> Vec<gpui::Div> {
    let tones = [
        (Tone::NeedsInput, counts.needs_input),
        (Tone::Working, counts.working),
        (Tone::Done, counts.done),
        (Tone::Idle, counts.idle),
        (Tone::Elsewhere, counts.elsewhere),
    ];
    tones
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(tone, count)| {
            let colors = tone_colors(tone);
            div()
                .flex_none()
                .h(px(20.))
                .min_w(px(20.))
                .px(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(colors.fill.unwrap_or(RAISED)))
                .text_color(rgb(colors.ink))
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .child(count.to_string())
        })
        .collect()
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        div()
            .key_context("Sidebar")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(SURFACE))
            .text_color(rgb(TEXT))
            .font_family(UI_FONT)
            .child(
                div()
                    .id("filter")
                    .h(px(36.))
                    .flex_none()
                    .mx_3()
                    .mt_3()
                    .mb_1()
                    .px_3()
                    .flex()
                    .items_center()
                    .text_sm()
                    .rounded(px(RADIUS))
                    .bg(rgb(RAISED))
                    .border_1()
                    .border_color(rgb(if focused { ACCENT } else { LINE }))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.focus_handle.focus(window, cx);
                        cx.notify();
                    }))
                    .child(field::render(&self.filter, "Filter active sessions", focused)),
            )
            .child(
                uniform_list(
                    "session-rows",
                    self.rows.len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| range.map(|index| this.render_row(index, cx)).collect::<Vec<_>>()),
                )
                .track_scroll(&self.scroll)
                .flex_1(),
            )
            .child(
                div()
                    .id("recent-sessions")
                    .flex_none()
                    .h(px(34.))
                    .mx_3()
                    .mt_2()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(RADIUS))
                    .border_1()
                    .border_color(rgb(LINE))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgb(TEXT)))
                    .child(div().flex_1().child("Recent sessions"))
                    .child(div().px(px(6.)).py(px(2.)).rounded(px(6.)).bg(rgb(RAISED)).child(RECENTS_SHORTCUT))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::OpenRecents))),
            )
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .flex()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .id("new-claude")
                            .flex_1()
                            .h(px(36.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(RADIUS))
                            .bg(rgb(ACCENT))
                            .text_color(rgb(ON_ACCENT))
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.9))
                            .child("New Claude session")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::New { claude: true }))),
                    )
                    .child(
                        div()
                            .id("new-shell")
                            .flex_none()
                            .size(px(36.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(RADIUS))
                            .border_1()
                            .border_color(rgb(LINE))
                            .text_color(rgb(MUTED))
                            .text_xs()
                            .cursor_pointer()
                            .hover(|style| style.text_color(rgb(TEXT)).bg(rgb(RAISED)))
                            .child(">_")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::New { claude: false }))),
                    ),
            )
            .when_some(self.menu.as_ref(), |el, menu| {
                let pick = cx.listener(|this, action: &MenuAction, _, cx| {
                    this.menu = None;
                    cx.emit(SidebarEvent::Dismiss);
                    cx.emit(SidebarEvent::Menu(action.clone()));
                    cx.notify();
                });
                let dismiss = cx.listener(|this, _: &(), _, cx| {
                    this.menu = None;
                    cx.emit(SidebarEvent::Dismiss);
                    cx.notify();
                });
                el.child(context_menu::render(
                    "sidebar-menu",
                    menu,
                    move |action, window, cx| pick(action, window, cx),
                    move |window, cx| dismiss(&(), window, cx),
                ))
            })
    }
}
