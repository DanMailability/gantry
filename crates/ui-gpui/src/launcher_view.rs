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
use crate::workspace_view::{ACCENT, BORDER, MUTED, PANEL, SELECTED, TEXT};

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
        div()
            .id(index)
            .h(px(24.))
            .px_3()
            .flex()
            .items_center()
            .cursor_pointer()
            .overflow_hidden()
            .whitespace_nowrap()
            .when(index == self.selected, |el| el.bg(rgb(SELECTED)))
            .child(label)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| this.choose(index, !event.modifiers().alt, cx)))
    }
}

impl Render for LauncherView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Launcher")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(LauncherEvent::Dismiss)))
            .w(px(560.))
            .flex()
            .flex_col()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(ACCENT))
            .rounded_md()
            .shadow_lg()
            .text_sm()
            .text_color(rgb(TEXT))
            .child(
                div()
                    .h(px(34.))
                    .px_3()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(field::render(&self.query, "Open a directory…", true)),
            )
            .child(
                uniform_list(
                    "launcher-items",
                    self.items.len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| range.map(|index| this.render_item(index, cx)).collect::<Vec<_>>()),
                )
                .track_scroll(&self.scroll)
                .h(px(320.)),
            )
            .child(
                div()
                    .h(px(24.))
                    .px_3()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .child("Enter: Claude · Alt+Enter: shell · Esc: close"),
            )
    }
}
