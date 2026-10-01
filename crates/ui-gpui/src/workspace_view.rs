//! The window's root view: tabs of split panes, routing daemon events to them, and saving the layout.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use app_core::layout_store;
use app_core::split_tree::{Axis, NodeId, Rect};
use app_core::workspace::{PaneLaunch, Restored, Workspace};
use daemon::client::{ClientEvent, Connection, DaemonClient, batch_events};
use daemon::pty::default_shell;
use gpui::{
    Context, Entity, FocusHandle, Focusable, IntoElement, PromptLevel, Render, Subscription, Task, Window, div,
    prelude::*, px, relative, rgb,
};
use protocol::message::PaneId;

use crate::terminal_view::{PaneEvent, TerminalView};

pub(crate) const PANEL: u32 = 0x1a1e26;
pub(crate) const BORDER: u32 = 0x2a2f3a;
pub(crate) const ACCENT: u32 = 0x6cb6ff;
pub(crate) const TEXT: u32 = 0xd6dde6;
pub(crate) const MUTED: u32 = 0x8b949e;
const BACKGROUND: u32 = 0x11141a;
pub(crate) const TAB_BAR_HEIGHT: f32 = 34.0;
/// Largest amount of output routed per UI update, so input and rendering keep running during a flood.
const MAX_BATCH_BYTES: usize = 256 * 1024;

struct Slot {
    view: Entity<TerminalView>,
    _subscription: Subscription,
}

/// Root view of the ccm window.
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
        let events_task = cx.spawn(async move |this, cx| {
            while let Ok(first) = events.recv().await {
                let batch = batch_events(first, &events, MAX_BATCH_BYTES);
                if this.update(cx, |view, cx| view.on_events(batch, cx)).is_err() {
                    return;
                }
                // Yield so keystrokes (e.g. Ctrl+C) are handled between batches.
                cx.background_executor().timer(Duration::ZERO).await;
            }
            let _ = this.update(cx, |view, cx| view.on_events(vec![ClientEvent::Disconnected], cx));
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
            _events: events_task,
        };
        let leaves: Vec<NodeId> = this.workspace.leaves.keys().copied().collect();
        for leaf in leaves {
            this.add_slot(leaf, window, cx);
        }
        for (leaf, pane) in &restored.attach {
            this.slots[leaf].view.update(cx, |view, cx| view.attach(*pane, cx));
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
        this.save();
        this
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

    fn on_events(&mut self, events: Vec<ClientEvent>, cx: &mut Context<Self>) {
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
                        self.save();
                    }
                }
                ClientEvent::Spawned { request, pane, cwd } => match self.pending.remove(request) {
                    Some(leaf) => {
                        self.workspace.pane_started(leaf, *pane, cwd.clone());
                        self.slots[&leaf].view.update(cx, |view, cx| view.attach(*pane, cx));
                        if self.workspace.focused() == Some(leaf) {
                            self.client.focus(*pane);
                        }
                        self.save();
                    }
                    // The leaf was closed while its process was starting.
                    None => self.client.kill(*pane),
                },
                ClientEvent::SpawnFailed { request, error } => {
                    if let Some(leaf) = self.pending.remove(request) {
                        self.slots[&leaf].view.update(cx, |view, cx| view.failed(error.clone(), cx));
                    }
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

    fn on_pane_event(&mut self, leaf: NodeId, event: PaneEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Focused => self.focus_leaf(leaf, window, cx),
            PaneEvent::Rerun => {
                let Some(entry) = self.workspace.leaves.get_mut(&leaf) else { return };
                entry.launch = entry.launch.rerun();
                let launch = entry.launch.clone();
                self.slots[&leaf].view.update(cx, |view, cx| view.restarting(launch, cx));
                self.spawn_leaf(leaf, None, cx);
                self.save();
            }
            PaneEvent::Close => self.close_leaf(leaf, window, cx),
            PaneEvent::TitleChanged => cx.notify(),
        }
    }

    /// Open a new Claude or shell pane — as a new tab, or splitting the focused pane along `axis` —
    /// in the focused pane's current directory.
    fn open(&mut self, claude: bool, axis: Option<Axis>, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.workspace.focused().and_then(|leaf| self.workspace.leaves.get(&leaf));
        let cwd = source.map_or_else(|| self.default_cwd.clone(), |leaf| leaf.launch.cwd.clone());
        let cwd_from = source.and_then(|leaf| leaf.pane);
        let launch = if claude { PaneLaunch::claude(cwd) } else { PaneLaunch { program: default_shell(), args: Vec::new(), cwd } };
        let leaf = match axis.and_then(|axis| self.workspace.split(axis, launch.clone())) {
            Some(leaf) => leaf,
            None => self.workspace.new_tab(launch),
        };
        self.add_slot(leaf, window, cx);
        self.spawn_leaf(leaf, cwd_from, cx);
        self.focus_current(window, cx);
        self.save();
    }

    fn close_leaf(&mut self, leaf: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.workspace.close(leaf).and_then(|removed| removed.pane) {
            self.client.kill(pane);
        }
        self.slots.remove(&leaf);
        self.pending.retain(|_, waiting| *waiting != leaf);
        self.focus_current(window, cx);
        self.save();
    }

    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.select_tab(index) {
            self.focus_current(window, cx);
            self.save();
        }
    }

    fn focus_leaf(&mut self, leaf: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.focus(leaf) {
            self.focus_current(window, cx);
            self.save();
        }
    }

    /// Give keyboard focus to the workspace's focused leaf (or the empty window).
    fn focus_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.workspace.focused().and_then(|leaf| self.slots.get(&leaf).map(|slot| (leaf, slot))) {
            Some((leaf, slot)) => {
                slot.view.focus_handle(cx).focus(window, cx);
                if let Some(pane) = self.workspace.leaves[&leaf].pane {
                    self.client.focus(pane);
                }
            }
            None => self.focus_handle.focus(window, cx),
        }
        cx.notify();
    }

    fn save(&self) {
        if let Err(err) = layout_store::save(&self.layout_file, &self.workspace) {
            tracing::warn!("saving the layout failed: {err}");
        }
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
            "Quit ccm?",
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

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let titles: Vec<String> = self
            .workspace
            .tabs
            .iter()
            .map(|tab| self.slots.get(&tab.focused).map_or_else(String::new, |slot| slot.view.read(cx).title()))
            .collect();
        div()
            .id("tab-bar")
            .h(px(TAB_BAR_HEIGHT))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(BORDER))
            .text_sm()
            .children(titles.into_iter().enumerate().map(|(index, title)| {
                let active = index == self.workspace.active;
                div()
                    .id(("tab", index))
                    .h_full()
                    .px_3()
                    .max_w(px(220.))
                    .flex()
                    .items_center()
                    .border_r_1()
                    .border_color(rgb(BORDER))
                    .text_color(rgb(if active { TEXT } else { MUTED }))
                    .when(active, |tab| tab.bg(rgb(BACKGROUND)).border_t_2().border_color(rgb(ACCENT)))
                    .child(div().truncate().child(title))
                    .on_click(cx.listener(move |this, _, window, cx| this.select_tab(index, window, cx)))
            }))
            .child(tab_bar_button("new-claude", "+ Claude", cx.listener(|this, _, window, cx| this.open(true, None, window, cx))))
            .child(tab_bar_button("new-shell", "+ Shell", cx.listener(|this, _, window, cx| this.open(false, None, window, cx))))
            .child(div().flex_1().h_full())
    }

    fn render_active_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(tab) = self.workspace.active_tab() else {
            return div()
                .flex_1()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .items_center()
                .justify_center()
                .text_color(rgb(TEXT))
                .child(tab_bar_button("empty-claude", "New Claude", cx.listener(|this, _, window, cx| this.open(true, None, window, cx))))
                .child(tab_bar_button("empty-shell", "New shell", cx.listener(|this, _, window, cx| this.open(false, None, window, cx))))
                .into_any_element();
        };
        let multiple = tab.root.leaves().len() > 1;
        div()
            .id("tab-content")
            .relative()
            .flex_1()
            .w_full()
            .bg(rgb(BORDER))
            .children(tab.root.layout(Rect::UNIT).into_iter().filter_map(|(leaf, rect)| {
                let view = self.slots.get(&leaf)?.view.clone();
                let focused = leaf == tab.focused;
                let title = view.read(cx).title();
                Some(
                    div()
                        .absolute()
                        .left(relative(rect.x))
                        .top(relative(rect.y))
                        .w(relative(rect.w))
                        .h(relative(rect.h))
                        .p(px(1.))
                        .child(
                            div()
                                .size_full()
                                .flex()
                                .flex_col()
                                .bg(rgb(BACKGROUND))
                                .when(multiple, |pane| pane.child(pane_header(title, focused)))
                                .child(div().flex_1().min_h_0().child(view)),
                        ),
                )
            }))
            .into_any_element()
    }
}

fn pane_header(title: String, focused: bool) -> impl IntoElement {
    div()
        .h(px(22.))
        .px_2()
        .flex()
        .items_center()
        .text_xs()
        .bg(rgb(PANEL))
        .text_color(rgb(if focused { ACCENT } else { MUTED }))
        .child(div().truncate().child(title))
}

pub(crate) fn tab_bar_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h_full()
        .px_3()
        .flex()
        .items_center()
        .text_color(rgb(MUTED))
        .hover(|style| style.text_color(rgb(ACCENT)))
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .child(self.render_tab_bar(cx))
            .child(self.render_active_tab(cx))
    }
}
