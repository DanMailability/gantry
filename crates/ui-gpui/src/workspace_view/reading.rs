//! The reading view toggle: swapping a local Claude pane between its terminal and a formatted-Markdown rendering of
//! the conversation. The terminal stays alive in its slot underneath, so toggling back is instant and the session
//! keeps running. Only local Claude panes offer the toggle; see `reading_view` for the view itself.

use super::{ToggleReadingView, WorkspaceView};

use std::path::PathBuf;

use app_core::split_tree::NodeId;
use gpui::{Context, Window, prelude::*};

use crate::reading_view::{ReadingSource, ReadingView};

impl WorkspaceView {
    /// Whether `leaf` can show the reading view: a Claude pane (not a shell), running on this machine (not an SSH
    /// host) and not inside a dev container (its transcript would live in the container).
    pub(super) fn reading_available(&self, leaf: NodeId) -> bool {
        self.workspace
            .leaves
            .get(&leaf)
            .is_some_and(|entry| entry.launch.is_claude() && entry.host.is_local() && entry.launch.container.is_none())
    }

    /// The Claude config dir `leaf`'s session lives under: the one the session reported, else the pane's launch dir,
    /// else the daemon's own.
    fn leaf_claude_dir(&self, leaf: NodeId, cx: &Context<Self>) -> Option<PathBuf> {
        let entry = self.workspace.leaves.get(&leaf)?;
        let recorded = entry
            .session
            .as_deref()
            .and_then(|id| self.sidebar.read(cx).sessions().get(&entry.host, id))
            .and_then(|info| info.claude_dir.clone());
        recorded.or_else(|| entry.launch.claude_dir.clone()).or_else(|| self.daemon_dir.clone())
    }

    /// Where `leaf`'s reading view reads from: its session transcript when the session id and config dir are known,
    /// else a short message explaining why there is nothing to show yet.
    fn reading_source(&self, leaf: NodeId, cx: &Context<Self>) -> ReadingSource {
        if !self.reading_available(leaf) {
            return ReadingSource::Message("Reading view is available for local Claude sessions for now.".to_string());
        }
        let session_id = self.workspace.leaves.get(&leaf).and_then(|entry| entry.session.clone());
        match (session_id, self.leaf_claude_dir(leaf, cx)) {
            (Some(session_id), Some(config_dir)) => ReadingSource::Session { config_dir, session_id },
            _ => ReadingSource::Message("Waiting for the conversation to start.".to_string()),
        }
    }

    /// Toggle the focused pane between its terminal and the reading view.
    pub(super) fn toggle_reading_view(&mut self, _: &ToggleReadingView, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(leaf) = self.workspace.focused() {
            self.toggle_reading_leaf(leaf, window, cx);
        }
    }

    /// Toggle `leaf` between its terminal and the reading view (no-op for a pane that cannot show it).
    pub(super) fn toggle_reading_leaf(&mut self, leaf: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.reading_available(leaf) {
            return;
        }
        if self.reading.remove(&leaf).is_some() {
            self.focus_current(window, cx);
            cx.notify();
            return;
        }
        let source = self.reading_source(leaf, cx);
        let view = cx.new(|cx| ReadingView::new(source, window, cx));
        self.reading.insert(leaf, view);
        self.focus_current(window, cx);
        cx.notify();
    }
}
