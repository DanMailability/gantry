//! The directory launcher overlay (`Ctrl+Shift+O`, and the New buttons): folders open now, then favorites and recent
//! Claude directories, fuzzy filtered, with a Browse… button. A host chip picks this machine or an SSH host; on a
//! host the typed path completes from the host's folders.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use app_core::dir_complete::{self, Completion, TabAction};
use app_core::host_state::HostDot;
use app_core::host_state::not_connected_notice;
use app_core::launcher::{self, LauncherItem, OpenFolder};
use app_core::session_tree::display_path;
use app_core::text_field::TextField;
use app_core::workspace::ClaudeDirs;
use gpui::{
    App, ClickEvent, Context, Div, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, Render,
    ScrollStrategy, Stateful, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};
use protocol::host::HostId;
use protocol::message::{DirEntry, SessionInfo};

use crate::context_menu::host_dot_color;
use crate::field;
use crate::settings_store::SettingsStore;
use crate::theme::{FadeKind, RADIUS, RADIUS_LARGE, RADIUS_SMALL, fade_in, ssh_icon, theme};

/// Quiet time after typing before a folder listing is asked for.
const LIST_DELAY: Duration = Duration::from_millis(120);
/// Listing request ids, unique across launchers (a late reply to a closed launcher must not match a new one's).
static NEXT_LISTING: AtomicU64 = AtomicU64::new(1);

/// One entry of the host chip's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherHost {
    /// The host.
    pub id: HostId,
    /// Its row in the list: the name and where its connection stands.
    pub label: String,
    /// The state dot.
    pub dot: HostDot,
    /// The host's daemon is connected (always true for this machine).
    pub connected: bool,
    /// Where its connection stands in a few words ("not connected", "disconnected", "needs sign-in", …).
    pub state: String,
}

#[allow(missing_docs)]
mod launcher_actions {
    gpui::actions!(launcher, [LauncherBrowse]);
}
pub(crate) use launcher_actions::LauncherBrowse;

/// What the launcher asks the workspace to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LauncherEvent {
    /// Open a new tab in `dir`.
    Open {
        /// The host the directory is on.
        host: HostId,
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
    /// List the folders in `path` on `host` (path completion); the reply goes to `set_listing` or
    /// `set_listing_failed` with the same `request`.
    ListDir {
        /// The host to ask.
        host: HostId,
        /// Request id.
        request: u64,
        /// The folder to list.
        path: String,
    },
    /// Close without opening anything.
    Dismiss,
}

/// The launcher overlay.
pub struct LauncherView {
    focus_handle: FocusHandle,
    open: Vec<OpenFolder>,
    sessions: Vec<SessionInfo>,
    /// Enter starts Claude and Alt+Enter a shell; false swaps them.
    claude_first: bool,
    home: Option<PathBuf>,
    query: TextField,
    items: Vec<LauncherItem>,
    selected: usize,
    scroll: UniformListScrollHandle,
    favorites: Vec<PathBuf>,
    /// This machine and the SSH hosts, for the host chip (only one entry: no chip).
    hosts: Vec<LauncherHost>,
    /// The host whose folders are listed.
    host: HostId,
    /// The host chip's list is open.
    host_chip_open: bool,
    /// The newest folder listing of the chosen host.
    completion: Completion,
    /// The highlighted completion row; `None` = the typed path itself.
    completion_selected: Option<usize>,
    /// The pause before a listing request.
    list_task: Option<gpui::Task<()>>,
    scroll_completions: UniformListScrollHandle,
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
    /// A launcher over the `open` folders and the directories of `sessions`, starting on `start` (one of `hosts`).
    /// `claude_first`: Enter starts Claude, Alt+Enter a shell; false swaps them.
    pub fn new(
        open: Vec<OpenFolder>,
        sessions: Vec<SessionInfo>,
        claude_first: bool,
        hosts: Vec<LauncherHost>,
        start: HostId,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = cx.global::<SettingsStore>().settings();
        let (favorites, memory) = (settings.favorites.clone(), settings.folder_claude_dirs.clone());
        let dirs = ClaudeDirs::from_settings(settings, None, None);
        let mut launcher = LauncherView {
            focus_handle: cx.focus_handle(),
            open,
            sessions,
            claude_first,
            home: std::env::home_dir(),
            query: TextField::default(),
            items: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            favorites,
            hosts,
            host: start,
            host_chip_open: false,
            completion: Completion::default(),
            completion_selected: None,
            list_task: None,
            scroll_completions: UniformListScrollHandle::new(),
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
        let known: Vec<HostId> = self.hosts.iter().map(|host| host.id.clone()).collect();
        let favorites = launcher::visible_favorites(&self.favorites, &known);
        let home = if self.host.is_local() { self.home.as_deref() } else { None };
        self.items = launcher::items(&self.open, &sessions, self.query.text(), home, &favorites, &self.host);
        self.selected = launcher::first_selectable(&self.items);
    }

    /// Update the host chip's list (a connection changed). A chosen host that left the settings gives way to this
    /// machine; one that just connected lists the typed path's parent again.
    pub fn set_hosts(&mut self, hosts: Vec<LauncherHost>, cx: &mut Context<Self>) {
        let was_away = self.host_disconnected();
        self.hosts = hosts;
        let kept = (self.items.len(), self.selected);
        let mut same_list = true;
        if !self.host.is_local() && !self.hosts.iter().any(|host| host.id == self.host) {
            self.choose_host(HostId::local());
            same_list = false;
        } else if was_away && !self.host_disconnected() {
            self.completion = Completion::default();
            self.typed_changed(cx);
            same_list = false;
        }
        self.refresh();
        if same_list && self.items.len() == kept.0 {
            self.selected = kept.1; // only a host's state changed: the highlight stays
        }
        cx.notify();
    }

    /// The home folder the rows abbreviate to `~`: this machine's, and none on an SSH host (matching uses the same).
    fn shown_home(&self) -> Option<&Path> {
        if self.host.is_local() { self.home.as_deref() } else { None }
    }

    /// The chosen host is an SSH host whose daemon is not connected.
    fn host_disconnected(&self) -> bool {
        self.hosts.iter().any(|host| host.id == self.host && !host.connected)
    }

    /// Where the chosen host's connection stands, in a few words.
    fn host_state(&self) -> &str {
        self.hosts.iter().find(|host| host.id == self.host).map_or("not connected", |host| host.state.as_str())
    }

    /// A typed path on an SSH host (it starts with `/` or `~`) completes from the host; anything else filters the list.
    fn path_mode(&self) -> bool {
        !self.host.is_local() && self.query.text().starts_with(['/', '~'])
    }

    /// The folders of the typed path's parent that match what follows its last `/`.
    fn completions(&self) -> Vec<DirEntry> {
        let (parent, prefix) = dir_complete::split_typed(self.query.text());
        if !self.path_mode() || self.completion.parent != parent {
            return Vec::new();
        }
        dir_complete::filter(&self.completion.dirs, &prefix)
    }

    /// Why the typed path's parent could not be listed, while it is the typed one.
    fn listing_error(&self) -> Option<&str> {
        let (parent, _) = dir_complete::split_typed(self.query.text());
        self.completion.error.as_deref().filter(|_| self.path_mode() && self.completion.requested == parent)
    }

    /// Choose `host`: the query, the pick, the list and the completion start over.
    fn choose_host(&mut self, host: HostId) {
        self.host = host;
        self.host_chip_open = false;
        self.query = TextField::default();
        self.pick = None;
        self.completion = Completion::default();
        self.completion_selected = None;
        self.list_task = None;
        self.refresh();
    }

    /// The text changed on an SSH host: ask for the typed path's parent after a short pause, unless it is listed.
    fn typed_changed(&mut self, cx: &mut Context<Self>) {
        self.completion_selected = None;
        self.list_task = None;
        let (parent, _) = dir_complete::split_typed(self.query.text());
        if !self.path_mode() || !self.completion.wants_request(self.query.text()) {
            return;
        }
        self.list_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LIST_DELAY).await;
            let _ = this.update(cx, |this, cx| this.request_listing(parent, cx));
        }));
    }

    fn request_listing(&mut self, parent: String, cx: &mut Context<Self>) {
        let request = NEXT_LISTING.fetch_add(1, Ordering::Relaxed);
        self.completion.start_request(request, parent.clone());
        cx.emit(LauncherEvent::ListDir { host: self.host.clone(), request, path: parent });
    }

    /// The folders of the newest listing request; replies to older requests are ignored.
    pub fn set_listing(&mut self, request: u64, dirs: Vec<DirEntry>, cx: &mut Context<Self>) {
        if request != self.completion.request {
            return;
        }
        self.completion.accept_listing(request, dirs);
        self.completion_selected = None;
        cx.notify();
    }

    /// The newest listing request failed; replies to older requests are ignored.
    pub fn set_listing_failed(&mut self, request: u64, reason: String, cx: &mut Context<Self>) {
        if request != self.completion.request {
            return;
        }
        self.completion.accept_failure(request, reason);
        cx.notify();
    }

    /// Complete the typed path with completion row `index`: its folder and a `/`, which asks for the next listing.
    fn accept_completion(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(dir) = self.completions().get(index).cloned() else { return };
        let (parent, _) = dir_complete::split_typed(self.query.text());
        self.query = TextField::default();
        self.query.insert(&format!("{parent}{}/", dir.name));
        self.typed_changed(cx);
    }

    /// Open the typed path (or the highlighted completion row) on the chosen SSH host.
    fn open_typed(&mut self, alt: bool, cx: &mut Context<Self>) {
        if self.listing_error().is_some() {
            return;
        }
        let claude = self.claude_first != alt;
        let typed = match self.completion_selected.and_then(|index| self.completions().get(index).cloned()) {
            Some(dir) => format!("{}{}", dir_complete::split_typed(self.query.text()).0, dir.name),
            None => self.query.text().to_string(),
        };
        let dir = if typed.len() > 1 { typed.trim_end_matches('/') } else { typed.as_str() };
        cx.emit(LauncherEvent::Open { host: self.host.clone(), dir: PathBuf::from(dir), claude, dir_pick: None });
    }

    /// The folder the highlighted item opens, if it is one.
    fn highlighted_folder(&self) -> Option<&PathBuf> {
        self.items.get(self.selected).and_then(LauncherItem::folder)
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

    fn choose(&mut self, index: usize, alt: bool, cx: &mut Context<Self>) {
        let claude = self.claude_first != alt;
        let folder = match self.items.get(index) {
            Some(LauncherItem::Open(folder)) => &folder.path,
            Some(LauncherItem::Dir { path, exists: true, .. }) => path,
            _ => return,
        };
        let dir_pick = if self.host.is_local() { self.dir_pick_for(Some(folder), claude) } else { None };
        cx.emit(LauncherEvent::Open { host: self.host.clone(), dir: folder.clone(), claude, dir_pick });
    }

    fn browse(&mut self, alt: bool, cx: &mut Context<Self>) {
        if !self.host.is_local() {
            return;
        }
        let claude = self.claude_first != alt;
        let dir_pick = self.dir_pick_for(None, claude);
        cx.emit(LauncherEvent::Browse { claude, dir_pick });
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
        let path_mode = self.path_mode();
        match keystroke.key.as_str() {
            "tab" if path_mode => {
                let rows = self.completions().len();
                match dir_complete::tab_action(
                    self.query.text(),
                    &self.completion.requested,
                    rows,
                    self.completion_selected,
                ) {
                    TabAction::Accept(index) => self.accept_completion(index, cx),
                    TabAction::Request(parent) => {
                        self.list_task = None;
                        self.request_listing(parent, cx);
                    }
                    TabAction::Nothing => {}
                }
            }
            "tab" if self.host.is_local() && self.dirs.dirs.len() > 1 => self.cycle_dir(keystroke.modifiers.shift),
            "tab" => {}
            "up" if path_mode => {
                self.completion_selected = self.completion_selected.and_then(|index| index.checked_sub(1));
            }
            "down" if path_mode => {
                let last = self.completions().len().saturating_sub(1);
                self.completion_selected = Some(self.completion_selected.map_or(0, |index| (index + 1).min(last)));
            }
            "up" => self.selected = launcher::step(&self.items, self.selected, false),
            "down" => self.selected = launcher::step(&self.items, self.selected, true),
            "enter" if path_mode => self.open_typed(keystroke.modifiers.alt, cx),
            "enter" => self.choose(self.selected, keystroke.modifiers.alt, cx),
            "escape" if self.chip_open || self.host_chip_open => (self.chip_open, self.host_chip_open) = (false, false),
            "escape" => cx.emit(LauncherEvent::Dismiss),
            _ => {
                if !field::edit(&mut self.query, keystroke, cx) {
                    return;
                }
                self.refresh();
                self.typed_changed(cx);
            }
        }
        if let Some(index) = self.completion_selected.filter(|_| path_mode) {
            self.scroll_completions.scroll_to_item(index, ScrollStrategy::Center);
        }
        self.scroll.scroll_to_item(self.selected, ScrollStrategy::Center);
        cx.stop_propagation();
        cx.notify();
    }

    fn render_item(&self, index: usize, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let theme = theme(cx).clone();
        if let LauncherItem::Header { label, count } = &self.items[index] {
            return div()
                .id(index)
                .w_full()
                .h(theme.ui(34.))
                .pl(px(20.))
                .pr(px(20.))
                .flex()
                .items_center()
                .text_xs()
                .text_color(rgb(theme.tokens.muted))
                .child(div().flex_1().child(label.to_uppercase()))
                .child(div().flex_none().children(count.map(|count| count.to_string())));
        }
        let (label, lead, note, muted, folder_open) = match &self.items[index] {
            LauncherItem::Dir { path, favorite, exists } => {
                let shown = display_path(path, self.shown_home());
                let note = match (favorite, exists) {
                    (true, true) => "favorite",
                    (true, false) => "not found",
                    (false, _) => "",
                };
                let star = div().text_color(rgb(theme.tokens.accent)).child(if *favorite { "★" } else { "" });
                (shown, star, note.to_string(), !exists, false)
            }
            LauncherItem::Open(folder) => {
                let dot = div().size(theme.ui(8.)).rounded_full().map(|dot| match folder.tone {
                    Some(tone) => dot.bg(rgb(theme.tone(tone).dot)),
                    None => dot.border_1().border_color(rgb(theme.tokens.muted)),
                });
                (display_path(&folder.path, self.shown_home()), dot, folder.note(), false, true)
            }
            LauncherItem::Header { .. } => unreachable!("headers return above"),
        };
        let selected = index == self.selected;
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
                    .when((!selected && !folder_open) || muted, |el| el.text_color(rgb(theme.tokens.muted)))
                    .when(!selected && !muted, |el| el.hover(|style| style.text_color(rgb(theme.tokens.text))))
                    .child(div().flex_none().w(theme.ui(14.)).flex().items_center().child(lead))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(div().flex_none().text_xs().text_color(rgb(theme.tokens.muted)).child(note)),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| this.choose(index, event.modifiers().alt, cx)))
    }

    /// What shows under the field on an SSH host: why the typed path cannot open (error), or that the host is away.
    fn notice(&self) -> Option<(String, bool)> {
        if let Some(reason) = self.listing_error() {
            return Some((reason.to_string(), true));
        }
        self.host_disconnected().then(|| (not_connected_notice(self.host.name(), self.host_state()), false))
    }

    /// One folder of the typed path's parent.
    fn render_completion(&self, index: usize, dir: &DirEntry, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let theme = theme(cx).clone();
        let selected = self.completion_selected == Some(index);
        div()
            .id(("launcher-completion", index))
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
                    .when(!selected, |el| el.hover(|style| style.bg(rgb(theme.tokens.raised))))
                    .child(div().flex_1().min_w_0().truncate().child(format!("{}/", dir.name)))
                    .child(div().flex_none().text_xs().text_color(rgb(theme.tokens.muted)).child(if dir.symlink {
                        "(symlink)"
                    } else {
                        ""
                    })),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.accept_completion(index, cx);
                cx.notify();
            }))
    }

    /// The host chip ("Local ▾" or "<host> ▾", with the SSH icon on a host) and its list of hosts while open.
    fn render_host_chip(&self, cx: &mut Context<Self>) -> (Stateful<Div>, Option<Div>) {
        let theme = theme(cx).clone();
        let remote = !self.host.is_local();
        let name = if remote { self.host.name().to_string() } else { "Local".to_string() };
        let dot = self.hosts.iter().find(|host| host.id == self.host).map_or(HostDot::None, |host| host.dot);
        let chip = div()
            .id("launcher-host-chip")
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
            .when(remote, |chip| chip.child(ssh_icon(theme.tokens.text, &theme)))
            .child(div().size(theme.ui(8.)).rounded_full().bg(rgb(host_dot_color(&theme, dot))))
            .child(name)
            .child(div().text_color(rgb(theme.tokens.muted)).child("▾"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.host_chip_open = !this.host_chip_open;
                this.chip_open = false;
                cx.notify();
            }));
        let list = self.host_chip_open.then(|| {
            let rows = self.hosts.iter().enumerate().map(|(position, host)| {
                let current = host.id == self.host;
                let id = host.id.clone();
                div()
                    .id(("launcher-host", position))
                    .h(theme.ui(30.))
                    .px(theme.ui(10.))
                    .flex()
                    .items_center()
                    .gap(theme.ui(8.))
                    .rounded(px(RADIUS_SMALL))
                    .cursor_pointer()
                    .when(current, |row| row.bg(rgb(theme.tokens.line)))
                    .hover(|style| style.bg(rgb(theme.tokens.line)))
                    .child(div().size(theme.ui(8.)).rounded_full().bg(rgb(host_dot_color(&theme, host.dot))))
                    .child(div().flex_1().child(host.label.clone()))
                    .child(div().w(theme.ui(14.)).text_color(rgb(theme.tokens.accent)).child(if current {
                        "✓"
                    } else {
                        ""
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_host(id.clone());
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
        (chip, list)
    }

    /// The chip row: the host chip (with SSH hosts defined) and, on this machine, "Claude: <name> ▾" with where the
    /// choice comes from; the lists of hosts and dirs under it while open.
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
                this.host_chip_open = false;
                cx.notify();
            }));
        let claude_chip = self.host.is_local() && self.dirs.dirs.len() > 1;
        let (host_chip, host_list) = self.render_host_chip(cx);
        let has_hosts = self.hosts.len() > 1;
        let list = (self.chip_open && claude_chip).then(|| {
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
                    .child(div().w(theme.ui(14.)).text_color(rgb(theme.tokens.accent)).child(if position == current {
                        "✓"
                    } else {
                        ""
                    }))
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
                    .when(has_hosts, |row| row.child(host_chip))
                    .when(claude_chip, |row| row.child(chip).child(div().min_w_0().child(note))),
            )
            .children(host_list.filter(|_| has_hosts))
            .children(list)
    }
}

impl Render for LauncherView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let card = div()
            .key_context("Launcher")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|this, _: &LauncherBrowse, _, cx| this.browse(false, cx)))
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
                    .gap(theme.ui(10.))
                    .child(div().flex_1().min_w_0().child(field::render(
                        &self.query,
                        "Open a directory…",
                        true,
                        &theme,
                        field::on_press(cx, |this, offset, clicks, _, cx| {
                            this.query.press(offset, clicks);
                            cx.notify();
                        }),
                    )))
                    .when(!self.host.is_local(), |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(rgb(theme.tokens.muted))
                                .child("Type a path, e.g. ~/projects"),
                        )
                    })
                    .when(self.host.is_local(), |row| {
                        row.child(
                            div()
                                .id("launcher-browse")
                                .flex_none()
                                .h(theme.ui(28.))
                                .px(theme.ui(12.))
                                .flex()
                                .items_center()
                                .gap(theme.ui(8.))
                                .rounded(px(RADIUS_SMALL))
                                .border_1()
                                .border_color(rgb(theme.tokens.line))
                                .bg(rgb(theme.tokens.raised))
                                .hover(|style| style.border_color(rgb(theme.tokens.accent)))
                                .cursor_pointer()
                                .child("Browse…")
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(theme.tokens.muted))
                                        .child(if cfg!(target_os = "macos") { "Cmd+O" } else { "Ctrl+O" }),
                                )
                                .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                                    this.browse(event.modifiers().alt, cx)
                                })),
                        )
                    }),
            )
            .when(self.hosts.len() > 1 || self.dirs.dirs.len() > 1, |launcher| launcher.child(self.render_chip(cx)))
            .when_some(self.notice(), |launcher, (text, error)| {
                launcher.child(
                    div()
                        .px_4()
                        .py(theme.ui(6.))
                        .text_xs()
                        .text_color(rgb(if error { theme.tokens.danger } else { theme.tokens.muted }))
                        .border_b_1()
                        .border_color(rgb(theme.tokens.line))
                        .child(text),
                )
            })
            .child(if self.path_mode() {
                uniform_list(
                    "launcher-completions",
                    self.completions().len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        let rows = this.completions();
                        range
                            .filter_map(|index| rows.get(index).map(|dir| this.render_completion(index, dir, cx)))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll_completions)
                .h(theme.ui(320.))
            } else {
                uniform_list(
                    "launcher-items",
                    self.items.len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        range.map(|index| this.render_item(index, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .h(theme.ui(320.))
            })
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
                    .child(match (self.claude_first, self.host.is_local() && self.dirs.dirs.len() > 1) {
                        (true, true) => "Enter: Claude · Alt+Enter: shell · Tab: config dir · Esc: close",
                        (true, false) => "Enter: Claude · Alt+Enter: shell · Esc: close",
                        (false, true) => "Enter: shell · Alt+Enter: Claude · Tab: config dir · Esc: close",
                        (false, false) => "Enter: shell · Alt+Enter: Claude · Esc: close",
                    }),
            );
        fade_in(card, "fade-launcher", FadeKind::Card, cx.global::<SettingsStore>().settings().animate_ui)
    }
}
