//! The recent sessions overlay (`Ctrl+Shift+R`): closed sessions by project, searchable, with project chips.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use app_core::recents::{
    self, Chip, ChipTarget, ContentState, ProjectItem, RecentItem, RecentsInput, Snippet, searches_content,
};
use app_core::session_tree::{ProjectRoots, session_title};
use app_core::text_field::TextField;
use gpui::{
    App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle, IntoElement,
    KeyDownEvent, ListAlignment, ListState, Render, Stateful, StyledText, Window, div, list, prelude::*, px, rgb,
};
use protocol::host::HostId;
use protocol::message::{SearchHit, SessionInfo, Speaker};

use crate::field;
use crate::settings_store::{SettingsStore, dir_badge_for};
use crate::theme::{RADIUS, RADIUS_LARGE, RADIUS_SMALL, Theme, container_icon, dir_badge, side_line, ssh_icon, theme};

const ROW_HEIGHT: f32 = 36.0;
/// Height of a conversation hit's row (two lines).
const HIT_ROW_HEIGHT: f32 = 56.0;
/// Quiet time after typing before the query is sent.
const SEARCH_DELAY: Duration = Duration::from_millis(150);
/// Most sessions asked for (the project chips filter them here).
pub const SEARCH_LIMIT: u32 = 200;
/// Request ids, unique across pop-ups (a late reply to a closed pop-up must not match a new one's).
static NEXT_SEARCH: AtomicU64 = AtomicU64::new(1);

/// What the overlay asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecentsEvent {
    /// Resume a session.
    Resume {
        /// The host it ran on.
        host: HostId,
        /// Session id.
        session: String,
        /// Its directory.
        cwd: PathBuf,
        /// As a split of the focused pane.
        split: bool,
    },
    /// Connect an SSH host that is not connected.
    Connect(HostId),
    /// Close without opening anything.
    Dismiss,
    /// Search the conversations for `query`; the reply goes to `set_hits` with the same `request`.
    Search {
        /// Request id.
        request: u64,
        /// The query.
        query: String,
    },
}

/// What the list shows.
#[derive(Clone, Debug)]
enum List {
    Sessions(Vec<RecentItem>),
    /// The "More…" chooser.
    Projects(Vec<ProjectItem>),
}

/// The recent sessions overlay.
pub struct RecentsView {
    focus_handle: FocusHandle,
    sessions: Vec<SessionInfo>,
    open: HashSet<(HostId, String)>,
    home: Option<PathBuf>,
    roots: ProjectRoots,
    query: TextField,
    project: Option<PathBuf>,
    expanded: HashSet<PathBuf>,
    choosing: bool,
    list: List,
    chips: Vec<Chip>,
    counts: (usize, usize),
    selected: usize,
    list_state: ListState,
    per_project: usize,
    chip_count: usize,
    /// The last conversation search sent (0 = none).
    request: u64,
    /// Whether the reply to `request` arrived.
    answered: bool,
    hits: Vec<SearchHit>,
    /// Percent of the index built while a large pass runs.
    indexing: Option<u8>,
    /// The hosts whose conversation search does not work.
    unavailable: Vec<HostId>,
    /// SSH hosts that are not connected: each adds a "not connected" line with a Connect button.
    offline: Vec<String>,
    search_task: Option<gpui::Task<()>>,
}

impl EventEmitter<RecentsEvent> for RecentsView {}

impl Focusable for RecentsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl RecentsView {
    /// An overlay over `sessions`; `open` = (host, session) a leaf of the layout shows; `project` = the project to
    /// start on; `offline` = names of the SSH hosts that are not connected.
    pub fn new(
        sessions: Vec<SessionInfo>,
        open: HashSet<(HostId, String)>,
        project: Option<PathBuf>,
        offline: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let home = std::env::home_dir();
        let recents = cx.global::<SettingsStore>().settings().recents;
        let mut view = RecentsView {
            focus_handle: cx.focus_handle(),
            sessions,
            open,
            roots: ProjectRoots::new(home.clone()),
            home,
            query: TextField::default(),
            project,
            expanded: HashSet::new(),
            choosing: false,
            list: List::Sessions(Vec::new()),
            chips: Vec::new(),
            counts: (0, 0),
            selected: 0,
            list_state: ListState::new(0, ListAlignment::Top, px(200.)),
            per_project: recents.per_project,
            chip_count: recents.chips,
            request: 0,
            answered: false,
            hits: Vec::new(),
            indexing: None,
            unavailable: Vec::new(),
            offline,
            search_task: None,
        };
        view.refresh();
        view
    }

    /// Rebuild the list after the user changed what it shows (query, chip): back to the top, first row selected.
    fn refresh(&mut self) {
        self.rebuild();
        self.selected = self.first_selectable();
        self.list_state.reset(self.len());
    }

    fn rebuild(&mut self) {
        let sessions: Vec<&SessionInfo> = self.sessions.iter().collect();
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let roots = &self.roots;
        let project_of = |host: &HostId, cwd: &Path| roots.of(host, cwd);
        let input = RecentsInput {
            sessions: &sessions,
            open: &self.open,
            home: self.home.as_deref(),
            query: self.query.text(),
            project: self.project.as_deref(),
            expanded: &self.expanded,
            project_of: &project_of,
            now_ms,
            per_project: self.per_project,
            chips: self.chip_count,
            hits: &self.hits,
            content: self.content_state(),
        };
        self.list = if self.choosing {
            List::Projects(recents::projects(&input))
        } else {
            List::Sessions(recents::items(&input))
        };
        self.chips = recents::chips(&input);
        self.counts = recents::counts(&input);
    }

    fn content_state(&self) -> ContentState {
        if self.unavailable.iter().any(HostId::is_local) {
            ContentState::Unavailable
        } else if let Some(percent) = self.indexing {
            ContentState::Indexing(percent)
        } else if self.answered {
            ContentState::Ready
        } else {
            ContentState::Waiting
        }
    }

    /// The query changed: send it after `SEARCH_DELAY` when it is long enough (the last hits stay until the reply,
    /// so the section does not flicker while typing).
    fn query_changed(&mut self, cx: &mut Context<Self>) {
        self.search_task = None;
        self.request = 0;
        self.answered = false;
        if self.choosing || !searches_content(self.query.text()) {
            self.hits.clear();
            return;
        }
        let query = self.query.text().to_string();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DELAY).await;
            let _ = this.update(cx, |this, cx| this.send_search(query, cx));
        }));
    }

    fn send_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.request = NEXT_SEARCH.fetch_add(1, Ordering::Relaxed);
        cx.emit(RecentsEvent::Search { request: self.request, query });
    }

    /// Hits for `request`; replies to older requests are ignored.
    pub fn set_hits(&mut self, request: u64, hits: Vec<SearchHit>, cx: &mut Context<Self>) {
        if request == 0 || request != self.request {
            return;
        }
        self.hits = hits;
        self.answered = true;
        self.refresh_keeping_selection();
        cx.notify();
    }

    /// The index's state: `indexing` = percent while a large pass runs. When indexing ends, the query is sent again.
    pub fn set_index_state(&mut self, indexing: Option<u8>, cx: &mut Context<Self>) {
        let finished = self.indexing.is_some() && indexing.is_none();
        self.indexing = indexing;
        if finished && !self.choosing && searches_content(self.query.text()) {
            self.send_search(self.query.text().to_string(), cx);
        }
        self.refresh_keeping_selection();
        cx.notify();
    }

    /// The hosts whose conversation search does not work: this machine's shows in the list, the others as a line
    /// under it.
    pub fn set_search_unavailable(&mut self, hosts: Vec<HostId>, cx: &mut Context<Self>) {
        self.unavailable = hosts;
        self.refresh_keeping_selection();
        cx.notify();
    }

    /// New sessions (a host connected) and the hosts still not connected.
    pub fn set_sessions(&mut self, sessions: Vec<SessionInfo>, offline: Vec<String>, cx: &mut Context<Self>) {
        self.sessions = sessions;
        self.offline = offline;
        self.refresh_keeping_selection();
        cx.notify();
    }

    /// Rebuild after a change the user didn't make (hits, the index's state, "Show N more"): only the rows that
    /// changed are re-measured, so the scroll position stays and the selected row can still be revealed.
    fn refresh_keeping_selection(&mut self) {
        let keep = self.selected;
        let old = std::mem::replace(&mut self.list, List::Sessions(Vec::new()));
        self.rebuild();
        let (same_start, same_end) = match (&old, &self.list) {
            (List::Sessions(old), List::Sessions(new)) => unchanged_ends(old, new),
            (List::Projects(old), List::Projects(new)) => unchanged_ends(old, new),
            _ => (0, 0),
        };
        let old_len = match &old {
            List::Sessions(items) => items.len(),
            List::Projects(projects) => projects.len(),
        };
        self.list_state.splice(same_start..old_len - same_end, self.len() - same_start - same_end);
        self.selected = if self.selectable(keep) { keep } else { self.first_selectable() };
    }

    fn len(&self) -> usize {
        match &self.list {
            List::Sessions(items) => items.len(),
            List::Projects(projects) => projects.len(),
        }
    }

    fn selectable(&self, index: usize) -> bool {
        match &self.list {
            List::Sessions(items) => items.get(index).is_some_and(RecentItem::selectable),
            List::Projects(projects) => index < projects.len(),
        }
    }

    fn first_selectable(&self) -> usize {
        (0..self.len()).find(|&i| self.selectable(i)).unwrap_or(0)
    }

    fn move_selection(&mut self, down: bool) {
        let candidates: Vec<usize> = (0..self.len()).filter(|&i| self.selectable(i)).collect();
        let next = if down {
            candidates.iter().copied().find(|&i| i > self.selected)
        } else {
            candidates.iter().rev().copied().find(|&i| i < self.selected)
        };
        if let Some(next) = next {
            self.selected = next;
        }
    }

    /// Pick chip `target`.
    fn pick(&mut self, target: ChipTarget, cx: &mut Context<Self>) {
        match target {
            ChipTarget::All => (self.project, self.choosing) = (None, false),
            ChipTarget::Project(path) => (self.project, self.choosing) = (Some(path), false),
            ChipTarget::More => self.choosing = true,
        }
        self.query.clear();
        self.query_changed(cx);
        self.refresh();
    }

    /// Move to the next (or previous) chip.
    fn cycle_chip(&mut self, forward: bool, cx: &mut Context<Self>) {
        let current = self.chips.iter().position(|chip| self.is_current(&chip.target)).unwrap_or(0);
        let len = self.chips.len().max(1);
        let next = if forward { (current + 1) % len } else { (current + len - 1) % len };
        if let Some(chip) = self.chips.get(next) {
            self.pick(chip.target.clone(), cx);
        }
    }

    fn is_current(&self, target: &ChipTarget) -> bool {
        match target {
            ChipTarget::All => self.project.is_none() && !self.choosing,
            ChipTarget::Project(path) => self.project.as_ref() == Some(path) && !self.choosing,
            ChipTarget::More => self.choosing,
        }
    }

    fn choose(&mut self, index: usize, split: bool, cx: &mut Context<Self>) {
        match &self.list {
            List::Sessions(items) => match items.get(index) {
                Some(RecentItem::Session { session, .. }) if session.cwd_exists => {
                    cx.emit(RecentsEvent::Resume {
                        host: session.host.clone(),
                        session: session.id.clone(),
                        cwd: session.cwd.clone(),
                        split,
                    });
                }
                Some(RecentItem::More { project, .. }) => {
                    self.expanded.insert(project.clone());
                    self.refresh_keeping_selection();
                }
                _ => {}
            },
            List::Projects(projects) => {
                if let Some(project) = projects.get(index) {
                    let target = ChipTarget::Project(project.path.clone());
                    self.pick(target, cx);
                }
            }
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "up" => self.move_selection(false),
            "down" => self.move_selection(true),
            "enter" => self.choose(self.selected, keystroke.modifiers.secondary(), cx),
            "tab" => self.cycle_chip(!keystroke.modifiers.shift, cx),
            "escape" if self.choosing => {
                self.choosing = false;
                self.query.clear();
                self.query_changed(cx);
                self.refresh();
            }
            "escape" => cx.emit(RecentsEvent::Dismiss),
            _ => {
                let before = self.query.text().to_string();
                if !field::edit(&mut self.query, keystroke, cx) {
                    return;
                }
                // Cursor moves and copying change nothing to search for.
                if self.query.text() != before {
                    self.query_changed(cx);
                    self.refresh();
                }
            }
        }
        self.list_state.scroll_to_reveal_item(self.selected);
        cx.stop_propagation();
        cx.notify();
    }

    fn render_item(&self, index: usize, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let selected = index == self.selected;
        let theme = theme(cx).clone();
        let row = div().id(index).w_full().h(theme.ui(ROW_HEIGHT)).px_2().py(px(2.));
        let pill = || {
            div()
                .size_full()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(RADIUS))
                .border_2()
                .border_color(if selected { rgb(theme.tokens.accent) } else { gpui::rgba(0) })
                .when(selected, |pill| pill.bg(rgb(theme.tokens.raised)))
                .cursor_pointer()
                .overflow_hidden()
                .whitespace_nowrap()
        };
        let row = match &self.list {
            List::Sessions(items) => match &items[index] {
                RecentItem::Heading { path, label, display } => {
                    let color = theme.project(path);
                    return row.child(
                        div()
                            .size_full()
                            .px_3()
                            .flex()
                            .items_end()
                            .gap_2()
                            .pb(theme.ui(4.))
                            .children(color.map(|color| {
                                div().flex_none().mb(theme.ui(5.)).size(theme.ui(8.)).rounded(px(3.)).bg(rgb(color))
                            }))
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .when_some(color, |label, color| label.text_color(rgb(color)))
                                    .child(label.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(theme.tokens.muted))
                                    .child(display.clone()),
                            ),
                    );
                }
                // While searching the list is one ranked list without headings, so each row names its project.
                RecentItem::Session { session, age, project, marks, snippet: None, .. } => row.child(
                    pill()
                        .when_some(theme.project(project), |pill, color| {
                            pill.relative().pl(theme.ui(18.)).child(side_line(color, &theme))
                        })
                        .child(
                            div()
                                .flex_none()
                                .size(theme.ui(8.))
                                .rounded_full()
                                .border_1()
                                .border_color(rgb(theme.tokens.muted)),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(title_text(session, marks)))
                        .when(session.container.is_some(), |pill| pill.child(container_icon(theme.tokens.text, &theme)))
                        .when(!session.host.is_local(), |pill| pill.child(ssh_icon(theme.tokens.text, &theme)))
                        .children(
                            dir_badge_for(cx.global::<SettingsStore>().settings(), session.claude_dir.as_deref())
                                .map(|badge| dir_badge(&badge, &theme)),
                        )
                        .when(!self.query.text().trim().is_empty(), |pill| {
                            let name =
                                project.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                            let name = match session.host.remote_name() {
                                Some(host) => format!("{host} - {name}"),
                                None => name,
                            };
                            pill.child(div().flex_none().text_xs().text_color(rgb(theme.tokens.muted)).child(name))
                        })
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(rgb(theme.tokens.muted))
                                .child(if session.cwd_exists { age.clone() } else { "Folder missing".into() }),
                        ),
                ),
                RecentItem::Session { session, age, project, marks, snippet: Some(snippet), open } => {
                    let project_name =
                        project.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                    let project_name = match session.host.remote_name() {
                        Some(host) => format!("{host} - {project_name}"),
                        None => project_name,
                    };
                    row.h(theme.ui(HIT_ROW_HEIGHT)).child(
                        pill()
                            .flex_col()
                            .items_start()
                            .justify_center()
                            .gap(px(2.))
                            .when_some(theme.project(project), |pill, color| {
                                pill.relative().pl(theme.ui(18.)).child(side_line(color, &theme))
                            })
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().flex_1().min_w_0().truncate().child(title_text(session, marks)))
                                    .when(*open, |line| {
                                        line.child(
                                            div()
                                                .flex_none()
                                                .text_xs()
                                                .text_color(rgb(theme.tokens.accent))
                                                .child("open"),
                                        )
                                    })
                                    .when(session.container.is_some(), |line| {
                                        line.child(container_icon(theme.tokens.text, &theme))
                                    })
                                    .when(!session.host.is_local(), |line| {
                                        line.child(ssh_icon(theme.tokens.text, &theme))
                                    })
                                    .children(
                                        dir_badge_for(
                                            cx.global::<SettingsStore>().settings(),
                                            session.claude_dir.as_deref(),
                                        )
                                        .map(|badge| dir_badge(&badge, &theme)),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(rgb(theme.tokens.muted))
                                            .child(project_name),
                                    )
                                    .child(
                                        div().flex_none().text_xs().text_color(rgb(theme.tokens.muted)).child(
                                            if session.cwd_exists { age.clone() } else { "Folder missing".into() },
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(theme.tokens.muted))
                                    .child(snippet_text(snippet, &theme)),
                            ),
                    )
                }
                RecentItem::Note { text } => {
                    return row.child(
                        div()
                            .size_full()
                            .px_3()
                            .flex()
                            .items_center()
                            .text_xs()
                            .text_color(rgb(theme.tokens.muted))
                            .child(text.clone()),
                    );
                }
                RecentItem::More { hidden, .. } => row.child(
                    pill().pl(theme.ui(30.)).text_color(rgb(theme.tokens.accent)).child(format!("Show {hidden} more")),
                ),
            },
            List::Projects(projects) => {
                let project = &projects[index];
                row.child(
                    pill()
                        .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(project.label.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(rgb(theme.tokens.muted))
                                .child(project.display.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(rgb(theme.tokens.muted))
                                .child(project.count.to_string()),
                        ),
                )
            }
        };
        row.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            this.selected = index;
            this.choose(index, event.modifiers().secondary(), cx);
            cx.notify();
        }))
    }

    /// Under the list: a muted line per host whose conversation search is unavailable, and one with a Connect button
    /// per SSH host that is not connected.
    fn render_host_lines(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let theme = theme(cx).clone();
        let line = || {
            div()
                .h(theme.ui(30.))
                .px_4()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(rgb(theme.tokens.muted))
                .border_t_1()
                .border_color(rgb(theme.tokens.line))
        };
        let searching = !self.choosing && searches_content(self.query.text());
        let unavailable = self
            .unavailable
            .iter()
            .filter(|host| !host.is_local() && searching)
            .map(|host| line().child(format!("{}: search unavailable", host.name())).into_any_element());
        let offline = self.offline.iter().enumerate().map(|(index, name)| {
            let host = HostId::named(name);
            line()
                .child(div().size(theme.ui(8.)).rounded_full().border_1().border_color(rgb(theme.tokens.muted)))
                .child(div().flex_1().child(format!("{name} is not connected")))
                .child(
                    div()
                        .id(("connect", index))
                        .h(theme.ui(22.))
                        .px(theme.ui(10.))
                        .flex()
                        .items_center()
                        .rounded(px(RADIUS_SMALL))
                        .border_1()
                        .border_color(rgb(theme.tokens.line))
                        .bg(rgb(theme.tokens.raised))
                        .text_color(rgb(theme.tokens.text))
                        .hover(|style| style.border_color(rgb(theme.tokens.accent)))
                        .cursor_pointer()
                        .child("Connect")
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(RecentsEvent::Connect(host.clone())))),
                )
                .into_any_element()
        });
        unavailable.chain(offline).collect()
    }

    fn render_chip(&self, index: usize, chip: &Chip, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.is_current(&chip.target);
        let theme = theme(cx).clone();
        let target = chip.target.clone();
        // A project's chip is drawn in its color (the other chips in the accent); without colors, as before.
        let color = match &chip.target {
            ChipTarget::Project(path) => theme.project(path),
            _ => None,
        }
        .unwrap_or(theme.tokens.accent);
        let colored = matches!(&chip.target, ChipTarget::Project(path) if theme.project(path).is_some());
        div()
            .id(("chip", index))
            .h(theme.ui(28.))
            .px(theme.ui(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded_full()
            .border_1()
            .border_color(if on {
                rgb(color)
            } else if colored {
                gpui::rgba((color << 8) | 0x80)
            } else {
                rgb(theme.tokens.line)
            })
            .when(on, |chip| {
                chip.bg(rgb(color))
                    .text_color(rgb(if colored { theme.tokens.ground } else { theme.tokens.on_accent }))
                    .font_weight(FontWeight::SEMIBOLD)
            })
            .when(!on, |chip| {
                chip.text_color(rgb(if colored { color } else { theme.tokens.text }))
                    .hover(|style| style.bg(rgb(theme.tokens.raised)))
            })
            .cursor_pointer()
            .child(chip.label.clone())
            .when(chip.target != ChipTarget::More, |el| {
                el.child(div().text_xs().opacity(0.8).child(chip.count.to_string()))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.pick(target.clone(), cx);
                cx.notify();
            }))
    }
}

impl Render for RecentsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let (matching, total) = self.counts;
        let count = if self.query.text().is_empty() {
            format!("{total} closed sessions")
        } else {
            format!("{matching} of {total}")
        };
        let chips: Vec<_> = self
            .chips
            .clone()
            .iter()
            .enumerate()
            .map(|(index, chip)| self.render_chip(index, chip, cx).into_any_element())
            .collect();
        let hint = |key: &'static str, what: &'static str| {
            div()
                .flex()
                .gap_1()
                .child(div().text_color(rgb(theme.tokens.text)).font_weight(FontWeight::SEMIBOLD).child(key))
                .child(what)
        };
        div()
            .key_context("Recents")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(RecentsEvent::Dismiss)))
            .w(theme.ui(640.))
            .flex()
            .flex_col()
            .bg(rgb(theme.tokens.surface))
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .rounded(px(RADIUS_LARGE))
            .overflow_hidden()
            .shadow_lg()
            .text_sm()
            .text_color(rgb(theme.tokens.text))
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(if self.choosing {
                                "Choose a project"
                            } else {
                                "Recent sessions"
                            }))
                            .child(div().text_xs().text_color(rgb(theme.tokens.muted)).child(count)),
                    )
                    .child(
                        div()
                            .h(theme.ui(38.))
                            .px_3()
                            .flex()
                            .items_center()
                            .rounded(px(RADIUS))
                            .bg(rgb(theme.tokens.raised))
                            .border_1()
                            .border_color(rgb(theme.tokens.accent))
                            .child(field::render(
                                &self.query,
                                if self.choosing { "Search projects" } else { "Search sessions and conversations" },
                                true,
                                &theme,
                                field::on_press(cx, |this, offset, clicks, _, cx| {
                                    this.query.press(offset, clicks);
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(div().flex().flex_wrap().gap(px(6.)).children(chips)),
            )
            .child({
                let view = cx.entity().downgrade();
                list(self.list_state.clone(), move |index, _window, cx| {
                    view.update(cx, |this, cx| this.render_item(index, cx).into_any_element())
                        .unwrap_or_else(|_| div().into_any_element())
                })
                .h(theme.ui(380.))
            })
            .children(self.render_host_lines(cx))
            .child(
                div()
                    .h(theme.ui(34.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_4()
                    .text_xs()
                    .text_color(rgb(theme.tokens.muted))
                    .border_t_1()
                    .border_color(rgb(theme.tokens.line))
                    .child(hint("Enter", "resume"))
                    .child(hint(SPLIT_KEY, "resume as split"))
                    .child(hint("↑↓", "move"))
                    .child(hint("Tab", "next project"))
                    .child(div().flex_1())
                    .child(hint("Esc", if self.choosing { "back" } else { "close" })),
            )
    }
}

/// "You: …" / "Claude: …" with the matched ranges in bold.
/// A session's title with the typed word starts in bold.
fn title_text(session: &SessionInfo, marks: &[Range<usize>]) -> StyledText {
    let bold = HighlightStyle { font_weight: Some(FontWeight::BOLD), ..HighlightStyle::default() };
    StyledText::new(session_title(session).to_string()).with_highlights(marks.iter().map(|r| (r.clone(), bold)))
}

fn snippet_text(snippet: &Snippet, theme: &Theme) -> StyledText {
    let prefix = match snippet.speaker {
        Speaker::You => "You: ",
        Speaker::Claude => "Claude: ",
    };
    let bold = HighlightStyle {
        font_weight: Some(FontWeight::BOLD),
        color: Some(rgb(theme.tokens.text).into()),
        ..HighlightStyle::default()
    };
    StyledText::new(format!("{prefix}{}", snippet.text))
        .with_highlights(snippet.highlights.iter().map(|r| (r.start + prefix.len()..r.end + prefix.len(), bold)))
}

#[cfg(target_os = "macos")]
const SPLIT_KEY: &str = "⌘Enter";
#[cfg(not(target_os = "macos"))]
const SPLIT_KEY: &str = "Ctrl+Enter";

/// How many items at the start and at the end of `old` and `new` are the same (never overlapping).
fn unchanged_ends<T: PartialEq>(old: &[T], new: &[T]) -> (usize, usize) {
    let start = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - start;
    let end = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_ends_are_found_on_both_sides() {
        assert_eq!(unchanged_ends(&[1, 2, 3, 4], &[1, 2, 9, 4]), (2, 1));
        assert_eq!(unchanged_ends(&[1, 2], &[1, 2, 3]), (2, 0), "rows added at the end");
        assert_eq!(unchanged_ends(&[1, 2, 3], &[1, 3]), (1, 1), "a row removed in the middle");
        assert_eq!(unchanged_ends(&[1, 1], &[1, 1, 1]), (2, 0), "prefix and suffix never overlap");
        assert_eq!(unchanged_ends::<u8>(&[], &[]), (0, 0));
    }
}
