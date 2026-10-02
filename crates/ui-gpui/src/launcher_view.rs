//! The directory launcher overlay (`Ctrl+Shift+O`): recent Claude directories, fuzzy filtered, then Browse….

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::PathBuf;

use app_core::launcher::{self, LauncherItem};
use app_core::session_tree::display_path;
use app_core::text_field::TextField;
use app_core::workspace::ClaudeDirs;
use gpui::{
    App, ClickEvent, Context, Div, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, Render, ScrollStrategy,
    Stateful, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};
use protocol::message::SessionInfo;

use crate::field;
use crate::settings_store::SettingsStore;
use crate::theme::{RADIUS, RADIUS_LARGE, RADIUS_SMALL, theme};

/// What the launcher asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LauncherEvent {
    /// Open a new tab in `dir`.
    Open {
        /// Directory.
        dir: PathBuf,
        /// Claude (true) or a shell.
        claude: bool,
        /// The Claude config dir (by name) chosen with the chip, when it differs from the one the folder resolves to.
        dir_pick: Option<String>,
    },
    /// Pick a directory with the system dialog, then open it.
    Browse {
        /// Claude (true) or a shell.
        claude: bool,
        /// As for `Open`.
        dir_pick: Option<String>,
    },
    /// Close without opening anything.
    Dismiss,
}

/// The launcher overlay.
pub struct LauncherView {
    focus_handle: FocusHandle,
    sessions: Vec<SessionInfo>,
    home: Option<PathBuf>,
    query: TextField,
    items: Vec<LauncherItem>,
    selected: usize,
    scroll: UniformListScrollHandle,
    favorites: Vec<PathBuf>,
    /// The Claude config dirs, with the default one's index.
    dirs: ClaudeDirs,
    /// Folder → dir name remembered for it.
    memory: BTreeMap<PathBuf, String>,
    /// The dir chosen with the chip (Tab or the list); `None` = whatever the highlighted folder resolves to.
    pick: Option<usize>,
    /// The chip's list of dirs is open.
    chip_open: bool,
}

impl EventEmitter<LauncherEvent> for LauncherView {}

impl Focusable for LauncherView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl LauncherView {
    /// A launcher over the directories of `sessions`.
    pub fn new(sessions: Vec<SessionInfo>, cx: &mut Context<Self>) -> Self {
        let settings = cx.global::<SettingsStore>().settings();
        let (favorites, memory) = (settings.favorites.clone(), settings.folder_claude_dirs.clone());
        let dirs = ClaudeDirs::from_settings(settings, None, None);
        let mut launcher = LauncherView {
            focus_handle: cx.focus_handle(),
            sessions,
            home: std::env::home_dir(),
            query: TextField::default(),
            items: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            favorites,
            dirs,
            memory,
            pick: None,
            chip_open: false,
        };
        launcher.refresh();
        launcher
    }

    fn refresh(&mut self) {
        let sessions: Vec<&SessionInfo> = self.sessions.iter().collect();
        self.items = launcher::items(&sessions, self.query.text(), self.home.as_deref(), &self.favorites);
        self.selected = 0;
    }

    /// The folder the highlighted item opens, if it is one.
    fn highlighted_folder(&self) -> Option<&PathBuf> {
        match self.items.get(self.selected) {
            Some(LauncherItem::Dir { path, .. }) => Some(path),
            _ => None,
        }
    }

    /// The index of the dir the highlighted folder resolves to without a pick (the default for Browse).
    fn resolved_dir(&self, folder: Option<&PathBuf>) -> usize {
        let default = &self.dirs.dirs[self.dirs.default].name;
        let name = folder.and_then(|folder| self.memory.get(folder)).unwrap_or(default);
        self.dirs.dirs.iter().position(|dir| dir.name == *name).unwrap_or(self.dirs.default)
    }

    /// The dir the chip shows: the pick, else the highlighted folder's own.
    fn chip_dir(&self) -> usize {
        self.pick.unwrap_or_else(|| self.resolved_dir(self.highlighted_folder()))
    }

    /// The pick to hand to the workspace for `folder`: only an explicit choice that differs from what it resolves to.
    fn dir_pick_for(&self, folder: Option<&PathBuf>, claude: bool) -> Option<String> {
        let pick = self.pick.filter(|pick| *pick != self.resolved_dir(folder))?;
        claude.then(|| self.dirs.dirs[pick].name.clone())
    }

    fn choose(&mut self, index: usize, claude: bool, cx: &mut Context<Self>) {
        match self.items.get(index) {
            Some(LauncherItem::Dir { path, exists: true, .. }) => {
                let dir_pick = self.dir_pick_for(Some(path), claude);
                cx.emit(LauncherEvent::Open { dir: path.clone(), claude, dir_pick });
            }
            Some(LauncherItem::Dir { .. }) => {}
            Some(LauncherItem::Browse) => {
                let dir_pick = self.dir_pick_for(None, claude);
                cx.emit(LauncherEvent::Browse { claude, dir_pick });
            }
            None => {}
        }
    }

    /// Move the chip's pick one dir on (or back).
    fn cycle_dir(&mut self, backwards: bool) {
        let count = self.dirs.dirs.len();
        if count < 2 {
            return;
        }
        let current = self.chip_dir();
        self.pick = Some(if backwards { (current + count - 1) % count } else { (current + 1) % count });
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "tab" if self.dirs.dirs.len() > 1 => self.cycle_dir(keystroke.modifiers.shift),
            "up" => self.selected = self.selected.saturating_sub(1),
            "down" => self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1)),
            "enter" => self.choose(self.selected, !keystroke.modifiers.alt, cx),
            "escape" if self.chip_open => self.chip_open = false,
            "escape" => cx.emit(LauncherEvent::Dismiss),
            _ => {
                if !field::edit(&mut self.query, keystroke, cx) {
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
        let (label, star, note, muted) = match &self.items[index] {
            LauncherItem::Dir { path, favorite, exists } => {
                let shown = display_path(path, self.home.as_deref());
                let note = match (favorite, exists) {
                    (true, true) => "favorite",
                    (true, false) => "not found",
                    (false, _) => "",
                };
                (shown, *favorite, note, !exists)
            }
            LauncherItem::Browse => ("Browse…".to_string(), false, "", false),
        };
        let selected = index == self.selected;
        let theme = theme(cx).clone();
        div()
            .id(index)
            .w_full()
            .h(theme.ui(34.))
            .px_2()
            .py(px(2.))
            .child(
                div()
                    .size_full()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap(theme.ui(10.))
                    .rounded(px(RADIUS_SMALL))
                    .cursor_pointer()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .when(selected, |el| el.bg(rgb(theme.tokens.raised)))
                    .when(!selected || muted, |el| el.text_color(rgb(theme.tokens.muted)))
                    .when(!selected && !muted, |el| el.hover(|style| style.text_color(rgb(theme.tokens.text))))
                    .child(div().flex_none().w(theme.ui(14.)).text_color(rgb(theme.tokens.accent)).child(if star { "★" } else { "" }))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(div().flex_none().text_xs().text_color(rgb(theme.tokens.muted)).child(note)),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| this.choose(index, !event.modifiers().alt, cx)))
    }

    /// "Claude: <name> ▾" and where the choice comes from; with a list of the dirs under it while open.
    fn render_chip(&self, cx: &mut Context<Self>) -> Div {
        let theme = theme(cx).clone();
        let current = self.chip_dir();
        let folder = self.highlighted_folder();
        let remembered = self.pick.is_none() && folder.is_some_and(|folder| self.memory.contains_key(folder));
        let note = match (remembered, folder) {
            (true, Some(folder)) => format!("remembered for {}", display_path(folder, self.home.as_deref())),
            _ => String::new(),
        };
        let chip = div()
            .id("launcher-dir-chip")
            .flex_none()
            .h(theme.ui(26.))
            .px(theme.ui(10.))
            .flex()
            .items_center()
            .gap(theme.ui(6.))
            .rounded(px(RADIUS_SMALL))
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .bg(rgb(theme.tokens.raised))
            .text_color(rgb(theme.tokens.text))
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .child(format!("Claude: {}", self.dirs.dirs[current].name))
            .child(div().text_color(rgb(theme.tokens.muted)).child("▾"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.chip_open = !this.chip_open;
                cx.notify();
            }));
        let list = self.chip_open.then(|| {
            let folder_dir = self.resolved_dir(folder);
            let rows = self.dirs.dirs.iter().enumerate().map(|(position, dir)| {
                let tag = if position == self.dirs.default {
                    "default"
                } else if remembered && position == folder_dir {
                    "this folder"
                } else {
                    ""
                };
                div()
                    .id(("launcher-dir", position))
                    .h(theme.ui(30.))
                    .px(theme.ui(10.))
                    .flex()
                    .items_center()
                    .gap(theme.ui(8.))
                    .rounded(px(RADIUS_SMALL))
                    .cursor_pointer()
                    .when(position == current, |row| row.bg(rgb(theme.tokens.line)))
                    .hover(|style| style.bg(rgb(theme.tokens.line)))
                    .child(div().flex_1().child(dir.name.clone()))
                    .child(div().text_xs().text_color(rgb(theme.tokens.muted)).child(tag))
                    .child(div().w(theme.ui(14.)).text_color(rgb(theme.tokens.accent)).child(if position == current { "✓" } else { "" }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pick = Some(position);
                        this.chip_open = false;
                        cx.notify();
                    }))
            });
            div()
                .p(px(6.))
                .flex()
                .flex_col()
                .rounded(px(RADIUS))
                .bg(rgb(theme.tokens.raised))
                .border_1()
                .border_color(rgb(theme.tokens.line))
                .text_sm()
                .text_color(rgb(theme.tokens.text))
                .children(rows)
        });
        div()
            .px_4()
            .py(theme.ui(6.))
            .flex()
            .flex_col()
            .gap(theme.ui(6.))
            .border_b_1()
            .border_color(rgb(theme.tokens.line))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(theme.ui(8.))
                    .text_xs()
                    .text_color(rgb(theme.tokens.muted))
                    .child(chip)
                    .child(div().min_w_0().child(note)),
            )
            .children(list)
    }
}

impl Render for LauncherView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        div()
            .key_context("Launcher")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(LauncherEvent::Dismiss)))
            .w(theme.ui(560.))
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
                    .h(theme.ui(46.))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(rgb(theme.tokens.line))
                    .child(field::render(&self.query, "Open a directory…", true, &theme, field::on_press(cx, |this, offset, clicks, _, cx| { this.query.press(offset, clicks); cx.notify(); }))),
            )
            .when(self.dirs.dirs.len() > 1, |launcher| launcher.child(self.render_chip(cx)))
            .child(
                uniform_list(
                    "launcher-items",
                    self.items.len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| range.map(|index| this.render_item(index, cx)).collect::<Vec<_>>()),
                )
                .track_scroll(&self.scroll)
                .h(theme.ui(320.)),
            )
            .child(
                div()
                    .h(theme.ui(32.))
                    .px_4()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(rgb(theme.tokens.muted))
                    .border_t_1()
                    .border_color(rgb(theme.tokens.line))
                    .child(if self.dirs.dirs.len() > 1 { "Enter: Claude · Alt+Enter: shell · Tab: config dir · Esc: close" } else { "Enter: Claude · Alt+Enter: shell · Esc: close" }),
            )
    }
}
