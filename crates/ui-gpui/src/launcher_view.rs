//! The directory launcher overlay (`Ctrl+Shift+O`): recent Claude directories, fuzzy filtered, then Browse….

use std::ops::Range;
use std::path::PathBuf;

use app_core::launcher::{self, LauncherItem};
use app_core::session_tree::display_path;
use app_core::text_field::TextField;
use gpui::{
    App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, Render, ScrollStrategy,
    Stateful, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};
use protocol::message::SessionInfo;

use crate::field;
use crate::theme::{RADIUS_LARGE, RADIUS_SMALL, theme};

/// What the launcher asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LauncherEvent {
    /// Open a new tab in `dir`.
    Open {
        /// Directory.
        dir: PathBuf,
        /// Claude (true) or a shell.
        claude: bool,
    },
    /// Pick a directory with the system dialog, then open it.
    Browse {
        /// Claude (true) or a shell.
        claude: bool,
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
        let mut launcher = LauncherView {
            focus_handle: cx.focus_handle(),
            sessions,
            home: std::env::home_dir(),
            query: TextField::default(),
            items: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
        };
        launcher.refresh();
        launcher
    }

    fn refresh(&mut self) {
        let sessions: Vec<&SessionInfo> = self.sessions.iter().collect();
        self.items = launcher::items(&sessions, self.query.text(), self.home.as_deref());
        self.selected = 0;
    }

    fn choose(&mut self, index: usize, claude: bool, cx: &mut Context<Self>) {
        match self.items.get(index) {
            Some(LauncherItem::Dir(dir)) => cx.emit(LauncherEvent::Open { dir: dir.clone(), claude }),
            Some(LauncherItem::Browse) => cx.emit(LauncherEvent::Browse { claude }),
            None => {}
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "up" => self.selected = self.selected.saturating_sub(1),
            "down" => self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1)),
            "enter" => self.choose(self.selected, !keystroke.modifiers.alt, cx),
            "escape" => cx.emit(LauncherEvent::Dismiss),
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
        let label = match &self.items[index] {
            LauncherItem::Dir(dir) => display_path(dir, self.home.as_deref()),
            LauncherItem::Browse => "Browse…".into(),
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
                    .rounded(px(RADIUS_SMALL))
                    .cursor_pointer()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .when(selected, |el| el.bg(rgb(theme.tokens.raised)))
                    .when(!selected, |el| el.text_color(rgb(theme.tokens.muted)).hover(|style| style.text_color(rgb(theme.tokens.text))))
                    .child(label),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| this.choose(index, !event.modifiers().alt, cx)))
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
                    .child(field::render(&self.query, "Open a directory…", true, &theme)),
            )
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
                    .child("Enter: Claude · Alt+Enter: shell · Esc: close"),
            )
    }
}
