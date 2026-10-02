//! The recent sessions overlay (`Ctrl+Shift+R`): closed sessions by project, searchable, with project chips.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use app_core::recents::{self, Chip, ChipTarget, ProjectItem, RecentItem, RecentsInput};
use app_core::session_tree::{ProjectRoots, session_title};
use app_core::text_field::TextField;
use gpui::{
    App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, Render,
    ScrollStrategy, Stateful, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};
use protocol::message::SessionInfo;

use crate::field;
use crate::settings_store::{SettingsStore, dir_badge_for};
use crate::theme::{RADIUS, RADIUS_LARGE, container_icon, dir_badge, side_line, theme};

const ROW_HEIGHT: f32 = 36.0;

/// What the overlay asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecentsEvent {
    /// Resume a session.
    Resume {
        /// Session id.
        session: String,
        /// Its directory.
        cwd: PathBuf,
        /// As a split of the focused pane.
        split: bool,
    },
    /// Close without opening anything.
    Dismiss,
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
    open: HashSet<String>,
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
    scroll: UniformListScrollHandle,
    per_project: usize,
    chip_count: usize,
}

impl EventEmitter<RecentsEvent> for RecentsView {}

impl Focusable for RecentsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl RecentsView {
    /// An overlay over `sessions`; `open` = sessions a leaf of the layout shows; `project` = the project to start on.
    pub fn new(sessions: Vec<SessionInfo>, open: HashSet<String>, project: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
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
            scroll: UniformListScrollHandle::new(),
            per_project: recents.per_project,
            chip_count: recents.chips,
        };
        view.refresh();
        view
    }

    fn refresh(&mut self) {
        let sessions: Vec<&SessionInfo> = self.sessions.iter().collect();
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let roots = &self.roots;
        let project_of = |cwd: &Path| roots.of(cwd);
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
        };
        self.list = if self.choosing { List::Projects(recents::projects(&input)) } else { List::Sessions(recents::items(&input)) };
        self.chips = recents::chips(&input);
        self.counts = recents::counts(&input);
        let first = self.first_selectable();
        self.selected = first;
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
    fn pick(&mut self, target: ChipTarget) {
        match target {
            ChipTarget::All => (self.project, self.choosing) = (None, false),
            ChipTarget::Project(path) => (self.project, self.choosing) = (Some(path), false),
            ChipTarget::More => self.choosing = true,
        }
        self.query.clear();
        self.refresh();
    }

    /// Move to the next (or previous) chip.
    fn cycle_chip(&mut self, forward: bool) {
        let current = self.chips.iter().position(|chip| self.is_current(&chip.target)).unwrap_or(0);
        let len = self.chips.len().max(1);
        let next = if forward { (current + 1) % len } else { (current + len - 1) % len };
        if let Some(chip) = self.chips.get(next) {
            self.pick(chip.target.clone());
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
                    cx.emit(RecentsEvent::Resume { session: session.id.clone(), cwd: session.cwd.clone(), split });
                }
                Some(RecentItem::More { project, .. }) => {
                    self.expanded.insert(project.clone());
                    let keep = index;
                    self.refresh();
                    self.selected = keep;
                }
                _ => {}
            },
            List::Projects(projects) => {
                if let Some(project) = projects.get(index) {
                    let target = ChipTarget::Project(project.path.clone());
                    self.pick(target);
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
            "tab" => self.cycle_chip(!keystroke.modifiers.shift),
            "escape" if self.choosing => {
                self.choosing = false;
                self.query.clear();
                self.refresh();
            }
            "escape" => cx.emit(RecentsEvent::Dismiss),
            _ => {
                if !field::edit(&mut self.query, keystroke) {
                    return;
                }
                self.refresh();
            }
        }
        self.scroll.scroll_to_item(self.selected, ScrollStrategy::Center);
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
                            .children(color.map(|color| div().flex_none().mb(theme.ui(5.)).size(theme.ui(8.)).rounded(px(3.)).bg(rgb(color))))
                            .child(
                                div()
                                    .flex_none()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .when_some(color, |label, color| label.text_color(rgb(color)))
                                    .child(label.clone()),
                            )
                            .child(div().min_w_0().truncate().text_xs().text_color(rgb(theme.tokens.muted)).child(display.clone())),
                    );
                }
                RecentItem::Session { session, age, project } => row.child(
                    pill()
                        .when_some(theme.project(project), |pill, color| pill.relative().pl(theme.ui(18.)).child(side_line(color, &theme)))
                        .child(div().flex_none().size(theme.ui(8.)).rounded_full().border_1().border_color(rgb(theme.tokens.muted)))
                        .child(div().flex_1().min_w_0().truncate().child(session_title(session).to_string()))
                        .when(session.container.is_some(), |pill| pill.child(container_icon(theme.tokens.text, &theme)))
                        .children(dir_badge_for(cx.global::<SettingsStore>().settings(), session.claude_dir.as_deref()).map(|badge| dir_badge(&badge, &theme)))
                        .child(div().flex_none().text_xs().text_color(rgb(theme.tokens.muted)).child(if session.cwd_exists { age.clone() } else { "Folder missing".into() })),
                ),
                RecentItem::More { hidden, .. } => row.child(pill().pl(theme.ui(30.)).text_color(rgb(theme.tokens.accent)).child(format!("Show {hidden} more"))),
            },
            List::Projects(projects) => {
                let project = &projects[index];
                row.child(
                    pill()
                        .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(project.label.clone()))
                        .child(div().flex_1().min_w_0().truncate().text_xs().text_color(rgb(theme.tokens.muted)).child(project.display.clone()))
                        .child(div().flex_none().text_xs().text_color(rgb(theme.tokens.muted)).child(project.count.to_string())),
                )
            }
        };
        row.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
            this.selected = index;
            this.choose(index, event.modifiers().secondary(), cx);
            cx.notify();
        }))
    }

    fn render_chip(&self, index: usize, chip: &Chip, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.is_current(&chip.target);
        let theme = theme(cx).clone();
        let target = chip.target.clone();
        // A project's chip is drawn in its colour (the other chips in the accent); without colours, as before.
        let color = match &chip.target {
            ChipTarget::Project(path) => theme.project(path),
            _ => None,
        }
        .unwrap_or(theme.tokens.accent);
        let coloured = matches!(&chip.target, ChipTarget::Project(path) if theme.project(path).is_some());
        div()
            .id(("chip", index))
            .h(theme.ui(28.))
            .px(theme.ui(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded_full()
            .border_1()
            .border_color(if on { rgb(color) } else if coloured { gpui::rgba((color << 8) | 0x80) } else { rgb(theme.tokens.line) })
            .when(on, |chip| chip.bg(rgb(color)).text_color(rgb(if coloured { theme.tokens.ground } else { theme.tokens.on_accent })).font_weight(FontWeight::SEMIBOLD))
            .when(!on, |chip| {
                chip.text_color(rgb(if coloured { color } else { theme.tokens.text })).hover(|style| style.bg(rgb(theme.tokens.raised)))
            })
            .cursor_pointer()
            .child(chip.label.clone())
            .when(chip.target != ChipTarget::More, |el| el.child(div().text_xs().opacity(0.8).child(chip.count.to_string())))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.pick(target.clone());
                cx.notify();
            }))
    }
}

impl Render for RecentsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let (matching, total) = self.counts;
        let count = if self.query.text().is_empty() { format!("{total} closed sessions") } else { format!("{matching} of {total}") };
        let chips: Vec<_> = self.chips.clone().iter().enumerate().map(|(index, chip)| self.render_chip(index, chip, cx).into_any_element()).collect();
        let hint = |key: &'static str, what: &'static str| {
            div().flex().gap_1().child(div().text_color(rgb(theme.tokens.text)).font_weight(FontWeight::SEMIBOLD).child(key)).child(what)
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
                            .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(if self.choosing { "Choose a project" } else { "Recent sessions" }))
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
                            .child(field::render(&self.query, if self.choosing { "Search projects" } else { "Search recent sessions" }, true, &theme)),
                    )
                    .child(div().flex().flex_wrap().gap(px(6.)).children(chips)),
            )
            .child(
                uniform_list(
                    "recent-items",
                    self.len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| range.map(|index| this.render_item(index, cx)).collect::<Vec<_>>()),
                )
                .track_scroll(&self.scroll)
                .h(theme.ui(380.)),
            )
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

#[cfg(target_os = "macos")]
const SPLIT_KEY: &str = "⌘Enter";
#[cfg(not(target_os = "macos"))]
const SPLIT_KEY: &str = "Ctrl+Enter";
