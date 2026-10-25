//! The reading view toggle and its data flow. A Claude pane (local or on an SSH host) can swap its terminal for a
//! formatted-Markdown rendering of the conversation. The terminal stays alive in its slot underneath, so toggling
//! back is instant and the session keeps running.
//!
//! The conversation text is fetched through the pane's host daemon (`read_transcript` → `Transcript`), like the git
//! diff view fetches remote content, so the heavy transcript file stays on the host. A light timer re-requests from
//! the last byte offset so large transcripts are not re-sent.

use super::{Reading, ToggleReadingView, WorkspaceView};

use std::path::PathBuf;
use std::time::Duration;

use app_core::split_tree::NodeId;
use gpui::{Context, Task, Window, prelude::*};

use protocol::message::ImageFormat;

use crate::reading_view::{ReadingEvent, ReadingView};

/// How often each shown reading view re-requests its transcript.
const REFRESH: Duration = Duration::from_millis(1500);

impl WorkspaceView {
    /// Whether `leaf` can show the reading view: a Claude pane (not a plain shell). Remote and dev-container panes
    /// qualify; the daemon resolves the transcript on whichever host the session runs on.
    pub(super) fn reading_available(&self, leaf: NodeId) -> bool {
        self.workspace.leaves.get(&leaf).is_some_and(|entry| entry.launch.is_claude())
    }

    /// The Claude config dir `leaf`'s session lives under on its host: the one the session reported, else the pane's
    /// launch dir. `None` leaves the host daemon to use its own default dir.
    fn leaf_claude_dir(&self, leaf: NodeId, cx: &Context<Self>) -> Option<PathBuf> {
        let entry = self.workspace.leaves.get(&leaf)?;
        let recorded = entry
            .session
            .as_deref()
            .and_then(|id| self.sidebar.read(cx).sessions().get(&entry.host, id))
            .and_then(|info| info.claude_dir.clone());
        recorded.or_else(|| entry.launch.claude_dir.clone())
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
            self.reading_requests.retain(|_, waiting| *waiting != leaf);
            self.focus_current(window, cx);
            cx.notify();
            return;
        }
        self.open_reading(leaf, window, cx);
        self.focus_current(window, cx);
        cx.notify();
    }

    /// Open a reading view for `leaf` (no focus change): create the view, subscribe to its prompt events, and start
    /// fetching. Does nothing for a pane that cannot show it, or one already showing the reading view.
    pub(super) fn open_reading(&mut self, leaf: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.reading_available(leaf) || self.reading.contains_key(&leaf) {
            return;
        }
        let view = cx.new(ReadingView::new);
        let subscription = cx.subscribe_in(&view, window, move |this, _, event: &ReadingEvent, window, cx| {
            this.on_reading_event(leaf, event, window, cx)
        });
        self.reading.insert(
            leaf,
            Reading {
                view,
                offset: 0,
                request: None,
                pending_images: std::collections::VecDeque::new(),
                _subscription: subscription,
            },
        );
        self.send_reading_request(leaf, cx);
        self.push_reading_status(leaf, cx);
    }

    /// Handle a reading view's prompt event: send the typed text to the pane, or switch back to the terminal.
    fn on_reading_event(&mut self, leaf: NodeId, event: &ReadingEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ReadingEvent::Submit(text) => self.send_reading_input(leaf, text, cx),
            ReadingEvent::SaveImage { id, format, data } => {
                self.save_reading_image(leaf, *id, *format, data.clone(), cx)
            }
            ReadingEvent::SaveFile { id, name, data } => {
                self.save_reading_file(leaf, *id, name.clone(), data.clone(), cx)
            }
            ReadingEvent::ShowTerminal => {
                if self.reading.remove(&leaf).is_some() {
                    self.reading_requests.retain(|_, waiting| *waiting != leaf);
                    self.attachment_requests.retain(|_, (l, _)| *l != leaf);
                    self.focus_current(window, cx);
                    cx.notify();
                }
            }
        }
    }

    /// The pane's host and live pane key, and the reading view, for an attachment save (all `Some` only when the
    /// pane is live and showing the reading view).
    fn reading_target(
        &self,
        leaf: NodeId,
    ) -> Option<(protocol::host::HostId, protocol::host::PaneKey, gpui::Entity<ReadingView>)> {
        let entry = self.workspace.leaves.get(&leaf)?;
        let key = entry.key()?;
        let view = self.reading.get(&leaf)?.view.clone();
        Some((entry.host.clone(), key, view))
    }

    /// Save a composer image on the pane's host (same path as a terminal paste); the chip resolves via PasteImageSaved.
    fn save_reading_image(
        &mut self,
        leaf: NodeId,
        id: u64,
        format: ImageFormat,
        data: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        let Some((host, key, view)) = self.reading_target(leaf) else { return };
        let Some(client) = self.client(&host).cloned() else { return };
        if !client.paste_image() {
            view.update(cx, |view, cx| view.fail_attachment(id, cx));
            return;
        }
        client.save_paste_image(key.pane, format, data);
        if let Some(reading) = self.reading.get_mut(&leaf) {
            reading.pending_images.push_back(id);
        }
    }

    /// Save a composer file on the pane's host; the chip resolves via AttachmentSaved/AttachmentFailed.
    fn save_reading_file(&mut self, leaf: NodeId, id: u64, name: String, data: Vec<u8>, cx: &mut Context<Self>) {
        let Some((host, key, view)) = self.reading_target(leaf) else { return };
        let Some(client) = self.client(&host).cloned() else { return };
        if !client.attachment() {
            view.update(cx, |view, cx| view.fail_attachment(id, cx));
            return;
        }
        self.next_request += 1;
        let request = self.next_request;
        self.attachment_requests.insert(request, (leaf, id));
        client.save_attachment(key.pane, request, &name, &data);
    }

    /// Whether `leaf`'s reading view has a pasted-image save in flight (so a PasteImageSaved belongs to it).
    pub(super) fn has_pending_reading_image(&self, leaf: NodeId) -> bool {
        self.reading.get(&leaf).is_some_and(|reading| !reading.pending_images.is_empty())
    }

    /// Resolve the oldest in-flight composer image for `leaf` with its host `path` (`None` = the save failed).
    pub(super) fn on_reading_image(&mut self, leaf: NodeId, path: Option<String>, cx: &mut Context<Self>) {
        let Some(id) = self.reading.get_mut(&leaf).and_then(|reading| reading.pending_images.pop_front()) else {
            return;
        };
        let Some(reading) = self.reading.get(&leaf) else { return };
        let view = reading.view.clone();
        match path {
            Some(path) => view.update(cx, |view, cx| view.set_attachment_path(id, path, cx)),
            None => view.update(cx, |view, cx| view.fail_attachment(id, cx)),
        }
    }

    /// Resolve a composer file attachment by its request with its host `path` (`None` = the save failed).
    pub(super) fn on_attachment(&mut self, request: u64, path: Option<String>, cx: &mut Context<Self>) {
        let Some((leaf, id)) = self.attachment_requests.remove(&request) else { return };
        let Some(reading) = self.reading.get(&leaf) else { return };
        let view = reading.view.clone();
        match path {
            Some(path) => view.update(cx, |view, cx| view.set_attachment_path(id, path, cx)),
            None => view.update(cx, |view, cx| view.fail_attachment(id, cx)),
        }
    }

    /// Send `text` to `leaf`'s Claude session over the daemon's input channel (local or SSH host). Multiline text is
    /// wrapped in a bracketed paste; a carriage return submits it.
    fn send_reading_input(&mut self, leaf: NodeId, text: &str, _cx: &mut Context<Self>) {
        let Some(entry) = self.workspace.leaves.get(&leaf) else { return };
        let host = entry.host.clone();
        let Some(key) = entry.key() else { return };
        let Some(client) = self.client(&host) else { return };
        let mut bytes = app_core::paste::encode_paste(text, text.contains('\n'));
        bytes.push(b'\r');
        client.input(key.pane, &bytes);
    }

    /// Push `leaf`'s current session status into its reading view (for the working indicator and needs-input banner).
    fn push_reading_status(&mut self, leaf: NodeId, cx: &mut Context<Self>) {
        let status = self.workspace.leaf_status(leaf, self.sidebar.read(cx).sessions()).cloned();
        if let Some(reading) = self.reading.get(&leaf) {
            let view = reading.view.clone();
            view.update(cx, |view, cx| view.set_status(status, cx));
        }
    }

    /// The reading views' refresh timer: re-request each shown view's transcript from its last offset.
    pub(super) fn start_reading_timer(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update(cx, |view, cx| view.reading_tick(cx)).is_err() {
                    return;
                }
            }
        })
    }

    /// Ask each shown reading view's host for any new messages.
    fn reading_tick(&mut self, cx: &mut Context<Self>) {
        let leaves: Vec<NodeId> = self.reading.keys().copied().collect();
        for leaf in leaves {
            self.push_reading_status(leaf, cx);
            self.send_reading_request(leaf, cx);
        }
    }

    /// Send one `read_transcript` request for `leaf` (unless one is already in flight, or its host is not connected).
    fn send_reading_request(&mut self, leaf: NodeId, cx: &mut Context<Self>) {
        let Some(reading) = self.reading.get(&leaf) else { return };
        let in_flight = reading.request;
        let (offset, view) = (reading.offset, reading.view.clone());
        let Some(entry) = self.workspace.leaves.get(&leaf) else { return };
        let host = entry.host.clone();
        let session = entry.session.clone();
        let Some(client) = self.client(&host).cloned() else {
            // The host is not connected: a request that was in flight will never be answered, so drop it and let the
            // next tick (after a reconnect) ask again rather than stall forever.
            if let Some(request) = in_flight {
                self.reading_requests.remove(&request);
                if let Some(reading) = self.reading.get_mut(&leaf) {
                    reading.request = None;
                }
            }
            return;
        };
        if in_flight.is_some() {
            return;
        }
        view.update(cx, |view, cx| view.set_can_attach_files(client.attachment(), cx));
        if !client.transcript() {
            view.update(cx, |view, cx| {
                view.set_notice(
                    Some("Reading view needs a newer Gantry on this host. Update it from the SSH hosts list.".into()),
                    cx,
                )
            });
            return;
        }
        view.update(cx, |view, cx| view.set_notice(None, cx));
        // A fresh session has no id until its first transcript line; the view shows "Waiting…" until then.
        let Some(session) = session else { return };
        let claude_dir = self.leaf_claude_dir(leaf, cx);
        self.next_request += 1;
        let request = self.next_request;
        self.reading_requests.insert(request, leaf);
        if let Some(reading) = self.reading.get_mut(&leaf) {
            reading.request = Some(request);
        }
        client.read_transcript(request, &session, claude_dir, offset);
    }

    /// Route a `Transcript` answer to its leaf's reading view.
    pub(super) fn on_transcript(
        &mut self,
        host: &protocol::host::HostId,
        request: u64,
        messages: Vec<protocol::message::TranscriptMessage>,
        offset: u64,
        found: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(leaf) = self.reading_requests.remove(&request) else { return };
        if self.workspace.leaves.get(&leaf).is_none_or(|entry| &entry.host != host) {
            return;
        }
        let Some(reading) = self.reading.get_mut(&leaf) else { return };
        if reading.request != Some(request) {
            return;
        }
        reading.request = None;
        reading.offset = offset;
        let view = reading.view.clone();
        view.update(cx, |view, cx| view.append(messages, found, cx));
    }
}
