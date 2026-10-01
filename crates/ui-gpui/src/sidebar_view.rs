//! The sidebar: Claude sessions (and shell panes) by folder or by state, with a fuzzy filter.

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use app_core::session_tree::{self, Activation, Row, RowKey, RowKind, ShellEntry, TreeInput, session_title};
use app_core::sessions::SessionStore;
use app_core::text_field::TextField;
use app_core::workspace::TreeView;
use gpui::{
    App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    Pixels, Point, Render, ScrollStrategy, Stateful, Task, UniformListScrollHandle, Window, anchored, deferred, div,
    prelude::*, px, rgb, uniform_list,
};
use protocol::message::{SessionInfo, SessionState};

use crate::field;
use crate::workspace_view::{ACCENT, AMBER, BORDER, MUTED, PANEL, SELECTED, TAB_BAR_HEIGHT, TEXT, status_glyph};

const ROW_HEIGHT: f32 = 22.0;
const INDENT: f32 = 12.0;
const FIELD_BACKGROUND: u32 = 0x11141a;
/// Ages ("5m") are recomputed this often.
const REFRESH: Duration = Duration::from_secs(30);

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
    /// Open a new Claude (or shell) tab in `dir`.
    NewHere {
        /// Directory.
        dir: PathBuf,
        /// Claude (true) or a shell.
        claude: bool,
    },
    /// The user switched between Folders and Status.
    ViewChanged(TreeView),
    /// Give keyboard focus back to the pane.
    Dismiss,
}

/// The session sidebar.
pub struct SidebarView {
    focus_handle: FocusHandle,
    sessions: SessionStore,
    shells: Vec<ShellEntry>,
    view: TreeView,
    filter: TextField,
    toggled: HashSet<PathBuf>,
    show_all: HashSet<PathBuf>,
    details: Option<String>,
    /// Sessions with an alert the user has not seen.
    unseen: HashSet<String>,
    rows: Vec<Row>,
    selected: Option<RowKey>,
    /// Directory context menu: its directory and where it opened.
    menu: Option<(PathBuf, Point<Pixels>)>,
    scroll: UniformListScrollHandle,
    home: Option<PathBuf>,
    _refresh: Task<()>,
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl Focusable for SidebarView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SidebarView {
    /// An empty sidebar showing `view`; sessions arrive with `set_sessions`.
    pub fn new(view: TreeView, cx: &mut Context<Self>) -> Self {
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update(cx, |this, cx| this.rebuild(cx)).is_err() {
                    return;
                }
            }
        });
        let mut sidebar = SidebarView {
            focus_handle: cx.focus_handle(),
            sessions: SessionStore::default(),
            shells: Vec::new(),
            view,
            filter: TextField::default(),
            toggled: HashSet::new(),
            show_all: HashSet::new(),
            details: None,
            unseen: HashSet::new(),
            rows: Vec::new(),
            selected: None,
            menu: None,
            scroll: UniformListScrollHandle::new(),
            home: std::env::home_dir(),
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
            cx.notify();
        }
    }

    /// The workspace's shell panes.
    pub fn set_shells(&mut self, shells: Vec<ShellEntry>, cx: &mut Context<Self>) {
        if shells != self.shells {
            self.shells = shells;
            self.rebuild(cx);
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let rows = {
            let sessions: Vec<&SessionInfo> = self.sessions.all().collect();
            let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
            let input = TreeInput {
                sessions: &sessions,
                shells: &self.shells,
                home: self.home.as_deref(),
                filter: self.filter.text(),
                toggled: &self.toggled,
                show_all: &self.show_all,
                details: self.details.as_deref(),
                now_ms,
            };
            match self.view {
                TreeView::Folders => session_tree::folders(&input),
                TreeView::Status => session_tree::status(&input),
            }
        };
        self.rows = rows;
        if self.selected_index().is_none() {
            self.selected = None;
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
            Activation::ToggleDir(dir) => {
                if !self.toggled.remove(&dir) {
                    self.toggled.insert(dir);
                }
                self.rebuild(cx);
            }
            Activation::ShowAll(dir) => {
                self.show_all.insert(dir);
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

    fn set_view(&mut self, view: TreeView, cx: &mut Context<Self>) {
        if self.view != view {
            self.view = view;
            self.selected = None;
            self.rebuild(cx);
            cx.emit(SidebarEvent::ViewChanged(view));
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
                self.selected = session_tree::first_entry(&self.rows).map(|index| {
                    self.scroll.scroll_to_item(index, ScrollStrategy::Center);
                    self.rows[index].key()
                });
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn menu_choice(&mut self, claude: bool, cx: &mut Context<Self>) {
        if let Some((dir, _)) = self.menu.take() {
            cx.emit(SidebarEvent::NewHere { dir, claude });
        }
        cx.notify();
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let row = &self.rows[index];
        let key = row.key();
        let selected = self.selected.as_ref() == Some(&key);
        let base = div()
            .id(index)
            .h(px(ROW_HEIGHT))
            .w_full()
            .pl(px(8. + INDENT * row.depth as f32))
            .pr_2()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .text_sm()
            .overflow_hidden()
            .whitespace_nowrap()
            .cursor_pointer()
            .when(selected, |el| el.bg(rgb(SELECTED)))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.focus_handle.focus(window, cx);
                // The list may have been rebuilt since it was drawn: act on the row that was
                // clicked, by identity, or on nothing if it is gone.
                if let Some(index) = session_tree::find(&this.rows, &key) {
                    this.activate(index, event.modifiers().secondary(), cx);
                }
            }));
        let muted = |text: String| div().flex_none().text_xs().text_color(rgb(MUTED)).child(text);
        let count = |text: String, color: u32| div().flex_none().text_xs().text_color(rgb(color)).child(text);
        let title = |text: String, color: u32| div().min_w_0().truncate().text_color(rgb(color)).child(text);
        // Paths give way to titles: they shrink first and truncate.
        let path_label = |text: String| div().min_w_0().flex_shrink(4.).truncate().text_xs().text_color(rgb(MUTED)).child(text);
        match &row.kind {
            RowKind::Host { name } => base.text_color(rgb(MUTED)).child(format!("▾ {name}")),
            RowKind::Group { label, count } => base.text_xs().text_color(rgb(MUTED)).child(format!("{} · {count}", label.to_uppercase())),
            RowKind::Dir { path, label, expanded, needs_input, busy, live, closed } => {
                let path = path.clone();
                let rest = live - needs_input - busy;
                base.child(div().flex_none().text_color(rgb(MUTED)).child(if *expanded { "▾" } else { "▸" }))
                    .child(title(label.clone(), TEXT))
                    .child(div().flex_1())
                    .when(*needs_input > 0, |el| el.child(count(format!("●{needs_input}"), AMBER)))
                    .when(*busy > 0, |el| el.child(count(format!("◐{busy}"), ACCENT)))
                    .when(rest > 0, |el| el.child(muted(format!("○{rest}"))))
                    .when(*closed > 0, |el| el.child(muted(format!("◌{closed}"))))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.menu = Some((path.clone(), event.position));
                            cx.notify();
                        }),
                    )
            }
            RowKind::Session { session, age, path } => {
                let closed = session.state == SessionState::Closed;
                let (glyph, color) = if closed { ("◌", MUTED) } else { status_glyph(session.status.as_ref()) };
                let external = matches!(session.state, SessionState::External { .. });
                let unseen = self.unseen.contains(&session.id);
                let right = if closed && !session.cwd_exists { "dir missing".to_string() } else { age.clone() };
                base.child(div().flex_none().text_color(rgb(color)).child(glyph))
                    .child(title(session_title(session).to_string(), if closed { MUTED } else { TEXT }))
                    .when(external, |el| el.child(muted("↗".to_string())))
                    .when_some(path.clone(), |el, path| el.child(path_label(path)))
                    .child(div().flex_1())
                    .when(unseen, |el| el.child(count("•".to_string(), AMBER)))
                    .child(muted(right))
            }
            RowKind::Shell { entry, path } => base
                .child(div().flex_none().text_color(rgb(MUTED)).child("○"))
                .child(title(entry.title.clone(), TEXT))
                .when_some(path.clone(), |el, path| el.child(path_label(path))),
            RowKind::More { hidden, .. } => base.text_color(rgb(MUTED)).child(format!("… {hidden} more")),
            RowKind::Detail { text, .. } => base.text_xs().italic().text_color(rgb(MUTED)).child(text.clone()),
            RowKind::TakeOver { .. } => base.text_color(rgb(ACCENT)).child("Take over…"),
        }
    }
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let current = self.view;
        let view_button = |id: &'static str, label: &'static str, view: TreeView, cx: &mut Context<Self>| {
            let active = current == view;
            div()
                .id(id)
                .px_2()
                .h_full()
                .flex()
                .items_center()
                .cursor_pointer()
                .text_color(rgb(if active { ACCENT } else { MUTED }))
                .when(active, |el| el.border_b_2().border_color(rgb(ACCENT)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.set_view(view, cx)))
        };
        div()
            .key_context("Sidebar")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(PANEL))
            .text_color(rgb(TEXT))
            .child(
                div()
                    .h(px(TAB_BAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .px_1()
                    .text_sm()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(view_button("view-folders", "Folders", TreeView::Folders, cx))
                    .child(view_button("view-status", "Status", TreeView::Status, cx)),
            )
            .child(
                div()
                    .id("filter")
                    .h(px(28.))
                    .flex_none()
                    .mx_2()
                    .my_1()
                    .px_2()
                    .flex()
                    .items_center()
                    .text_sm()
                    .rounded_sm()
                    .bg(rgb(FIELD_BACKGROUND))
                    .border_1()
                    .border_color(rgb(if focused { ACCENT } else { BORDER }))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.focus_handle.focus(window, cx);
                        cx.notify();
                    }))
                    .child(field::render(&self.filter, "Filter sessions", focused)),
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
            .when_some(self.menu.clone(), |el, (_, position)| {
                el.child(deferred(
                    anchored().position(position).child(
                        div()
                            .id("dir-menu")
                            // A floating layer: clicks must not reach the rows beneath it.
                            .occlude()
                            .py_1()
                            .text_sm()
                            .bg(rgb(PANEL))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .rounded_md()
                            .shadow_lg()
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                cx.notify();
                            }))
                            .child(menu_item("menu-claude", "New Claude here", cx.listener(|this, _, _, cx| this.menu_choice(true, cx))))
                            .child(menu_item("menu-shell", "New shell here", cx.listener(|this, _, _, cx| this.menu_choice(false, cx)))),
                    ),
                ))
            })
    }
}

fn menu_item(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div().id(id).px_3().py_1().cursor_pointer().hover(|style| style.bg(rgb(SELECTED))).child(label).on_click(on_click)
}
