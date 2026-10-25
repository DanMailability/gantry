//! The reading view: a Claude pane's conversation drawn as formatted Markdown, with a prompt box to chat back. The
//! conversation text comes from the pane's host daemon (`read_transcript` -> `Transcript`), driven by `WorkspaceView`,
//! which also feeds answers in here through [`ReadingView::append`] and the session status through
//! [`ReadingView::set_status`]. Typing in the box emits [`ReadingEvent::Submit`]; `WorkspaceView` turns that into
//! `DaemonClient::input` to the pane's session (local or SSH host, same channel).
//!
//! Interactive TUI moments (permission prompts, y/n, menus) are not in the transcript, so while the session needs
//! input the view shows a banner with a one-click switch to the terminal ([`ReadingEvent::ShowTerminal`]).

use app_core::markdown::{Block, Inline, parse_markdown};
use app_core::text_field::TextField;
use app_core::theme_colors::mix;
use std::path::Path;

use app_core::image_paste;
use gpui::{
    App, ClipboardEntry, ClipboardItem, Context, EventEmitter, ExternalPaths, FocusHandle, Focusable, FontStyle,
    FontWeight, HighlightStyle, IntoElement, KeyDownEvent, PathPromptOptions, ScrollHandle, SharedString, StyledText,
    UnderlineStyle, Window, div, prelude::*, px, rgb,
};
use protocol::message::{ImageFormat, SessionStatus, Speaker, TranscriptMessage};

use crate::field;
use crate::hover_card::{self, HoverCard};
use crate::settings_store::SettingsStore;
use crate::terminal_view::{installed_families, installed_family};
use crate::theme::{RADIUS, RADIUS_SMALL, Theme, UI_FONT, theme};

/// Something the reading view asks `WorkspaceView` to do.
pub(crate) enum ReadingEvent {
    /// Send this text to the pane's Claude session (submits with a carriage return).
    Submit(String),
    /// Switch this pane back to the terminal (e.g. to answer a prompt the reading view can't show).
    ShowTerminal,
    /// Save this pasted/attached image on the pane's host; `id` ties the answer back to its chip.
    SaveImage {
        /// The composer attachment id.
        id: u64,
        /// The image format.
        format: ImageFormat,
        /// The image bytes.
        data: Vec<u8>,
    },
    /// Save this attached file on the pane's host; `id` ties the answer back to its chip.
    SaveFile {
        /// The composer attachment id.
        id: u64,
        /// The file's name.
        name: String,
        /// The file bytes.
        data: Vec<u8>,
    },
}

/// A pending or saved composer attachment, shown as a chip above the input.
struct Attachment {
    id: u64,
    name: String,
    image: bool,
    /// The host path once saved; `None` while saving.
    path: Option<String>,
    /// The save failed (e.g. the host's daemon is too old for files).
    failed: bool,
}

/// One message turned into blocks, with who wrote it and its original Markdown (for "copy message").
struct Shown {
    you: bool,
    source: String,
    blocks: Vec<Block>,
}

/// A Claude pane's conversation as formatted Markdown, with a prompt box.
pub struct ReadingView {
    focus_handle: FocusHandle,
    messages: Vec<Shown>,
    /// A fixed one-line notice shown instead of the conversation (e.g. the host's daemon is too old).
    notice: Option<String>,
    /// At least one `Transcript` answer has arrived.
    loaded: bool,
    /// The transcript file was found on the host.
    found: bool,
    /// The session's status, pushed in by `WorkspaceView` (for the working indicator and needs-input banner).
    status: Option<SessionStatus>,
    /// The prompt box's text.
    input: TextField,
    /// Scroll to the bottom on the next append (set when the user sends, so their message comes into view).
    pending_scroll: bool,
    /// Attachments staged in the composer (images and files), shown as chips above the input.
    attachments: Vec<Attachment>,
    /// Next attachment id.
    next_attachment: u64,
    /// Whether the pane's host daemon answers file attachments (images always work); pushed in by `WorkspaceView`.
    can_attach_files: bool,
    scroll: ScrollHandle,
}

impl Focusable for ReadingView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<ReadingEvent> for ReadingView {}

impl ReadingView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        ReadingView {
            focus_handle: cx.focus_handle(),
            messages: Vec::new(),
            notice: None,
            loaded: false,
            found: false,
            status: None,
            input: TextField::default(),
            pending_scroll: false,
            attachments: Vec::new(),
            next_attachment: 0,
            can_attach_files: true,
            scroll: ScrollHandle::new(),
        }
    }

    /// Set or clear the fixed notice; redraws only when it changes.
    pub(crate) fn set_notice(&mut self, notice: Option<String>, cx: &mut Context<Self>) {
        if self.notice != notice {
            self.notice = notice;
            cx.notify();
        }
    }

    /// Set the session status; redraws only when it changes.
    pub(crate) fn set_status(&mut self, status: Option<SessionStatus>, cx: &mut Context<Self>) {
        if self.status != status {
            self.status = status;
            cx.notify();
        }
    }

    /// Append the messages from a `Transcript` answer. Keeps the view pinned to the bottom when it already was, and
    /// forces a scroll to the bottom once if the user just sent a message.
    pub(crate) fn append(&mut self, messages: Vec<TranscriptMessage>, found: bool, cx: &mut Context<Self>) {
        let was_at_bottom = self.at_bottom();
        let added = !messages.is_empty();
        let first_load = !self.loaded;
        for message in messages {
            let you = matches!(message.speaker, Speaker::You);
            let blocks = parse_markdown(&message.text);
            self.messages.push(Shown { you, source: message.text, blocks });
        }
        let found_changed = self.found != found;
        self.loaded = true;
        self.found = found;
        if added && (self.pending_scroll || (!first_load && was_at_bottom)) {
            self.scroll.scroll_to_bottom();
        }
        if added {
            self.pending_scroll = false;
        }
        if added || first_load || found_changed {
            cx.notify();
        }
    }

    /// Whether the scroll position is at (or within a few pixels of) the bottom.
    fn at_bottom(&self) -> bool {
        let max = f32::from(self.scroll.max_offset().y);
        let scrolled = -f32::from(self.scroll.offset().y);
        max <= 1.0 || scrolled >= max - 8.0
    }

    /// Send the box's text and any saved attachment paths, clear the composer, and stick to the bottom so the sent
    /// message scrolls into view.
    fn submit(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self.attachments.iter().filter_map(|a| a.path.clone()).collect();
        if self.input.text().trim().is_empty() && paths.is_empty() {
            return;
        }
        let mut full = self.input.text().to_string();
        for path in paths {
            if !full.is_empty() {
                full.push('\n');
            }
            full.push_str(&path);
        }
        cx.emit(ReadingEvent::Submit(full));
        self.input.clear();
        self.attachments.clear();
        self.pending_scroll = true;
        cx.notify();
    }

    /// Whether the host daemon can save file attachments; pushed in by `WorkspaceView`.
    pub(crate) fn set_can_attach_files(&mut self, can: bool, cx: &mut Context<Self>) {
        if self.can_attach_files != can {
            self.can_attach_files = can;
            cx.notify();
        }
    }

    /// The resolved host path of a saved attachment arrived.
    pub(crate) fn set_attachment_path(&mut self, id: u64, path: String, cx: &mut Context<Self>) {
        if let Some(a) = self.attachments.iter_mut().find(|a| a.id == id) {
            a.path = Some(path);
            cx.notify();
        }
    }

    /// Saving an attachment failed.
    pub(crate) fn fail_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(a) = self.attachments.iter_mut().find(|a| a.id == id) {
            a.failed = true;
            cx.notify();
        }
    }

    fn remove_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        self.attachments.retain(|a| a.id != id);
        cx.notify();
    }

    /// Add a chip and return its id.
    fn new_attachment(&mut self, name: String, image: bool) -> u64 {
        let id = self.next_attachment;
        self.next_attachment += 1;
        self.attachments.push(Attachment { id, name, image, path: None, failed: false });
        id
    }

    /// Read `path`, classify it as image or file, add a chip, and ask `WorkspaceView` to save it on the host.
    fn attach_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Ok(data) = std::fs::read(path) else { return };
        if !image_paste::fits(data.len()) {
            return;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".to_string());
        match image_format_from_path(path) {
            Some(format) => {
                let id = self.new_attachment(name, true);
                cx.emit(ReadingEvent::SaveImage { id, format, data });
            }
            None if !self.can_attach_files => {
                let id = self.new_attachment(name, false);
                self.fail_attachment(id, cx);
            }
            None => {
                let id = self.new_attachment(name.clone(), false);
                cx.emit(ReadingEvent::SaveFile { id, name, data });
            }
        }
        cx.notify();
    }

    /// If the clipboard holds an image, stage it and return true.
    fn try_attach_clipboard_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((format, data)) = cx.read_from_clipboard().as_ref().and_then(clipboard_image) else {
            return false;
        };
        let id = self.new_attachment("pasted image".to_string(), true);
        cx.emit(ReadingEvent::SaveImage { id, format, data });
        cx.notify();
        true
    }

    /// Open a file picker and stage the chosen files.
    fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked =
            cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = this.update(cx, |this, cx| {
                for path in paths {
                    this.attach_path(&path, cx);
                }
            });
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = &keystroke.modifiers;
        let chord = m.control || m.platform || m.alt;
        match keystroke.key.as_str() {
            "enter" if !chord && !m.shift => {
                self.submit(cx);
                cx.stop_propagation();
            }
            "enter" if !chord && m.shift => {
                self.input.insert("\n");
                cx.stop_propagation();
                cx.notify();
            }
            _ => {
                // Ctrl/Cmd+V with an image on the clipboard attaches it; otherwise editing keys and typing go to the
                // box. Anything else (e.g. Ctrl+Shift+M) bubbles up so the workspace shortcut still works.
                if image_paste::is_paste_key(&keystroke.key, m.control, m.alt, m.shift, m.platform)
                    && self.try_attach_clipboard_image(cx)
                {
                    cx.stop_propagation();
                } else if field::edit(&mut self.input, keystroke, cx) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }
        }
    }

    /// The one-line placeholder for the message area when there is nothing to draw yet.
    fn placeholder(&self) -> Option<String> {
        if self.messages.is_empty() {
            return Some(
                if self.loaded && self.found {
                    "No messages in this conversation yet. Type below to start."
                } else {
                    "Waiting for the conversation…"
                }
                .to_string(),
            );
        }
        None
    }
}

impl Render for ReadingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let focused = self.focus_handle.is_focused(window);
        let base = div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.tokens.ground))
            .text_color(rgb(theme.tokens.text))
            .font_family(UI_FONT);
        if let Some(text) = self.notice.clone() {
            return base
                .items_center()
                .justify_center()
                .p(theme.ui(24.))
                .child(div().max_w(theme.ui(380.)).text_center().text_color(rgb(theme.tokens.muted)).child(text))
                .into_any_element();
        }
        let mono: SharedString = installed_family(
            cx.global::<SettingsStore>().settings().terminal_font.family.as_deref(),
            installed_families(cx),
        )
        .to_string()
        .into();
        let body = match self.placeholder() {
            Some(text) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .p(theme.ui(24.))
                .child(div().max_w(theme.ui(380.)).text_center().text_color(rgb(theme.tokens.muted)).child(text))
                .into_any_element(),
            None => {
                let messages = self
                    .messages
                    .iter()
                    .enumerate()
                    .map(|(i, m)| render_message(i, m, &mono, &theme))
                    .collect::<Vec<_>>();
                div()
                    .id("reading")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(div().w_full().flex().flex_col().items_center().px(theme.ui(20.)).py(theme.ui(22.)).child(
                        div().w_full().max_w(theme.ui(760.)).flex().flex_col().gap(theme.ui(26.)).children(messages),
                    ))
                    .into_any_element()
            }
        };
        base.child(body)
            .children(self.needs_input_banner(&theme, cx))
            .child(self.prompt_bar(focused, &theme, cx))
            .into_any_element()
    }
}

impl ReadingView {
    /// The banner shown while the session is blocked on the user, with a button that switches to the terminal.
    fn needs_input_banner(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let SessionStatus::NeedsInput { reason } = self.status.clone()? else { return None };
        let text = match reason {
            Some(reason) => format!("Claude needs a response ({reason}) — switch to the terminal."),
            None => "Claude needs a response — switch to the terminal.".to_string(),
        };
        Some(
            div()
                .id("reading-needs-input")
                .flex_none()
                .flex()
                .items_center()
                .gap(theme.ui(10.))
                .px(theme.ui(16.))
                .py(theme.ui(10.))
                .bg(rgb(mix(theme.tokens.accent, theme.tokens.surface, 0.22)))
                .border_t_1()
                .border_color(rgb(theme.tokens.line))
                .child(div().flex_1().min_w_0().text_sm().text_color(rgb(theme.tokens.text)).child(text))
                .child(
                    div()
                        .id("reading-to-terminal")
                        .flex_none()
                        .px(theme.ui(12.))
                        .py(theme.ui(5.))
                        .rounded(px(RADIUS_SMALL))
                        .bg(rgb(theme.tokens.accent))
                        .text_color(rgb(theme.tokens.on_accent))
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .child("Open terminal")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ReadingEvent::ShowTerminal))),
                ),
        )
    }

    /// The prompt box pinned at the bottom: attachment chips, a "Claude is working…" line while busy, an attach
    /// button, the input, and Send. Dropping files onto it attaches them.
    fn prompt_bar(&self, focused: bool, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let working = matches!(self.status, Some(SessionStatus::Busy));
        let placeholder = "Message Claude…  (Enter to send, Shift+Enter for a newline)";
        let attach_tip = if self.can_attach_files {
            "Attach images or files"
        } else {
            "Attach images (files need a newer Gantry on this host)"
        };
        let attach_card = HoverCard {
            title: attach_tip.to_string(),
            status: None,
            dir: None,
            container: false,
            host: None,
            folder: None,
        };
        let attach = div()
            .id("reading-attach")
            .flex_none()
            .size(theme.ui(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(RADIUS_SMALL))
            .text_size(theme.ui(18.))
            .text_color(rgb(theme.tokens.muted))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(theme.tokens.raised)).text_color(rgb(theme.tokens.text)))
            .tooltip(hover_card::tooltip(attach_card))
            .child("+")
            .on_click(cx.listener(|this, _, window, cx| this.pick_files(window, cx)));
        div()
            .id("reading-composer")
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(theme.tokens.line))
            .bg(rgb(theme.tokens.surface))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                for path in paths.paths().to_vec() {
                    this.attach_path(&path, cx);
                }
            }))
            .when(!self.attachments.is_empty(), |bar| bar.child(self.attachment_chips(theme, cx)))
            .when(working, |bar| {
                bar.child(
                    div()
                        .px(theme.ui(16.))
                        .pt(theme.ui(8.))
                        .text_xs()
                        .text_color(rgb(theme.tokens.accent))
                        .child("Claude is working…"),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme.ui(10.))
                    .px(theme.ui(16.))
                    .py(theme.ui(12.))
                    .child(attach)
                    .child(div().flex_1().min_w_0().text_size(theme.ui(14.)).child(field::render(
                        &self.input,
                        placeholder,
                        focused,
                        theme,
                        field::on_press(cx, |this, offset, clicks, window, cx| {
                            this.input.press(offset, clicks);
                            this.focus_handle.focus(window, cx);
                            cx.notify();
                        }),
                    )))
                    .child(
                        div()
                            .id("reading-send")
                            .flex_none()
                            .px(theme.ui(12.))
                            .py(theme.ui(6.))
                            .rounded(px(RADIUS_SMALL))
                            .bg(rgb(theme.tokens.accent))
                            .text_color(rgb(theme.tokens.on_accent))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .child("Send")
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
    }
}

impl ReadingView {
    /// The row of attachment chips above the input; each shows its name, state, and a remove button.
    fn attachment_chips(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().flex_wrap().gap(theme.ui(6.)).px(theme.ui(16.)).pt(theme.ui(10.)).children(
            self.attachments.iter().map(|attachment| {
                let id = attachment.id;
                let kind = if attachment.image { "image" } else { "file" };
                let label = if attachment.failed {
                    format!("{} — unsupported here", attachment.name)
                } else if attachment.path.is_none() {
                    format!("{} — saving…", attachment.name)
                } else {
                    attachment.name.clone()
                };
                let color = if attachment.failed { theme.tokens.danger } else { theme.tokens.text };
                div()
                    .flex()
                    .items_center()
                    .gap(theme.ui(6.))
                    .px(theme.ui(8.))
                    .py(theme.ui(3.))
                    .rounded(px(RADIUS_SMALL))
                    .bg(rgb(theme.tokens.raised))
                    .text_size(theme.ui(11.5))
                    .text_color(rgb(color))
                    .child(div().text_color(rgb(theme.tokens.muted)).child(kind))
                    .child(div().max_w(theme.ui(220.)).truncate().child(label))
                    .child(
                        div()
                            .id(("rv-chip-x", id as usize))
                            .cursor_pointer()
                            .text_color(rgb(theme.tokens.muted))
                            .hover(|style| style.text_color(rgb(theme.tokens.text)))
                            .child("×")
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(id, cx))),
                    )
            }),
        )
    }
}

/// The image format for an image file path by extension, or `None` for a non-image file.
fn image_format_from_path(path: &Path) -> Option<ImageFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// The image format and bytes on the clipboard, if it holds a supported image (and no text).
fn clipboard_image(item: &ClipboardItem) -> Option<(ImageFormat, Vec<u8>)> {
    if item.text().is_some() {
        return None;
    }
    item.entries().iter().find_map(|entry| {
        let ClipboardEntry::Image(image) = entry else { return None };
        let format = match image.format {
            gpui::ImageFormat::Png => ImageFormat::Png,
            gpui::ImageFormat::Jpeg => ImageFormat::Jpeg,
            gpui::ImageFormat::Gif => ImageFormat::Gif,
            gpui::ImageFormat::Webp => ImageFormat::Webp,
            _ => return None,
        };
        image_paste::fits(image.bytes.len()).then(|| (format, image.bytes.clone()))
    })
}

/// One message: a speaker label with a copy-message button, then its blocks (each with a copy button on hover).
fn render_message(index: usize, message: &Shown, mono: &SharedString, theme: &Theme) -> impl IntoElement {
    let (label, fill, ink) = if message.you {
        ("You", theme.tokens.accent, theme.tokens.on_accent)
    } else {
        ("Claude", theme.tokens.raised, theme.tokens.text)
    };
    let group = SharedString::from(format!("rv-msg-{index}"));
    let blocks = message.blocks.iter().enumerate().map(move |(bi, block)| {
        let block_group = SharedString::from(format!("rv-blk-{index}-{bi}"));
        div().relative().group(block_group.clone()).child(render_block(block, mono, theme)).child(copy_button(
            format!("rv-blkcopy-{index}-{bi}"),
            block_source(block),
            block_group,
            theme,
        ))
    });
    div()
        .group(group.clone())
        .w_full()
        .flex()
        .flex_col()
        .gap(theme.ui(8.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme.ui(8.))
                .child(
                    div()
                        .flex_none()
                        .px(theme.ui(10.))
                        .py(theme.ui(2.))
                        .rounded(px(RADIUS_SMALL))
                        .bg(rgb(fill))
                        .text_color(rgb(ink))
                        .text_size(theme.ui(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(label),
                )
                .child(copy_button(format!("rv-msgcopy-{index}"), message.source.clone(), group, theme)),
        )
        .child(div().w_full().flex().flex_col().gap(theme.ui(10.)).children(blocks))
}

/// A small "Copy" button revealed when its `group` is hovered; copies `text` to the clipboard on click.
fn copy_button(id: String, text: String, group: SharedString, theme: &Theme) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .flex_none()
        .px(theme.ui(6.))
        .py(theme.ui(1.))
        .rounded(px(RADIUS_SMALL))
        .bg(rgb(theme.tokens.raised))
        .text_size(theme.ui(10.5))
        .text_color(rgb(theme.tokens.muted))
        .cursor_pointer()
        .opacity(0.)
        .group_hover(group, |style| style.opacity(1.))
        .hover(|style| style.text_color(rgb(theme.tokens.text)))
        .child("Copy")
        .on_click(move |_, _, cx: &mut App| cx.write_to_clipboard(ClipboardItem::new_string(text.clone())))
}

/// The blocks of a message (or of a list item / blockquote), in order.
fn render_blocks(blocks: &[Block], mono: &SharedString, theme: &Theme) -> Vec<gpui::AnyElement> {
    blocks.iter().map(|block| render_block(block, mono, theme)).collect()
}

/// One block as a GPUI element.
fn render_block(block: &Block, mono: &SharedString, theme: &Theme) -> gpui::AnyElement {
    match block {
        Block::Heading { level, inlines } => {
            let size = match level {
                1 => 25.,
                2 => 21.,
                3 => 18.,
                4 => 16.,
                _ => 14.5,
            };
            div()
                .w_full()
                .when(*level <= 2, |heading| heading.pt(theme.ui(4.)))
                .text_size(theme.ui(size))
                .line_height(theme.ui(size * 1.3))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(theme.tokens.text))
                .child(styled_line(inlines, theme))
                .into_any_element()
        }
        Block::Paragraph { inlines } => div()
            .w_full()
            .text_size(theme.ui(14.5))
            .line_height(theme.ui(22.))
            .text_color(rgb(theme.tokens.text))
            .child(styled_line(inlines, theme))
            .into_any_element(),
        Block::CodeBlock { lang, text } => render_code_block(lang.as_deref(), text, mono, theme),
        Block::BulletList { items } => div()
            .w_full()
            .flex()
            .flex_col()
            .gap(theme.ui(6.))
            .children(items.iter().map(|item| list_item(None, item, mono, theme)))
            .into_any_element(),
        Block::OrderedList { start, items } => div()
            .w_full()
            .flex()
            .flex_col()
            .gap(theme.ui(6.))
            .children(
                items.iter().enumerate().map(|(index, item)| list_item(Some(*start + index as u64), item, mono, theme)),
            )
            .into_any_element(),
        Block::Quote { blocks } => div()
            .w_full()
            .pl(theme.ui(14.))
            .border_l_2()
            .border_color(rgb(theme.tokens.line))
            .text_color(rgb(theme.tokens.muted))
            .flex()
            .flex_col()
            .gap(theme.ui(10.))
            .children(render_blocks(blocks, mono, theme))
            .into_any_element(),
        Block::Rule => div().w_full().h(px(1.)).my(theme.ui(6.)).bg(rgb(theme.tokens.line)).into_any_element(),
    }
}

/// A list item: its marker, then its blocks.
fn list_item(number: Option<u64>, blocks: &[Block], mono: &SharedString, theme: &Theme) -> impl IntoElement {
    let marker = match number {
        Some(n) => format!("{n}."),
        None => "•".to_string(),
    };
    div()
        .w_full()
        .flex()
        .flex_row()
        .gap(theme.ui(8.))
        .child(
            div()
                .flex_none()
                .w(theme.ui(22.))
                .text_size(theme.ui(14.5))
                .line_height(theme.ui(22.))
                .text_color(rgb(theme.tokens.muted))
                .text_right()
                .child(marker),
        )
        .child(
            div().flex_1().min_w_0().flex().flex_col().gap(theme.ui(6.)).children(render_blocks(blocks, mono, theme)),
        )
}

/// A code block: a rounded gray box, monospace, padded, whitespace preserved, long lines scrolling sideways.
fn render_code_block(lang: Option<&str>, text: &str, mono: &SharedString, theme: &Theme) -> gpui::AnyElement {
    let lines: Vec<gpui::AnyElement> = text
        .split('\n')
        .map(|line| {
            div()
                .whitespace_nowrap()
                .child(if line.is_empty() { " ".to_string() } else { line.to_string() })
                .into_any_element()
        })
        .collect();
    div()
        .id("code-block")
        .w_full()
        .overflow_x_scroll()
        .rounded(px(RADIUS))
        .bg(rgb(mix(theme.tokens.text, theme.tokens.surface, 0.06)))
        .border_1()
        .border_color(rgb(theme.tokens.line))
        .px(theme.ui(12.))
        .py(theme.ui(10.))
        .font_family(mono.clone())
        .text_size(theme.ui(12.5))
        .line_height(theme.ui(18.))
        .text_color(rgb(theme.tokens.text))
        .when_some(lang.filter(|l| !l.is_empty()), |block, lang| {
            block.child(
                div()
                    .mb(theme.ui(6.))
                    .text_size(theme.ui(10.5))
                    .font_family(UI_FONT)
                    .text_color(rgb(theme.tokens.muted))
                    .child(lang.to_string()),
            )
        })
        .children(lines)
        .into_any_element()
}

/// Inline runs as one wrapping `StyledText`: bold/italic weights, a tinted box for inline code, and an accent
/// underline for links.
fn styled_line(inlines: &[Inline], theme: &Theme) -> StyledText {
    let mut text = String::new();
    let mut highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    for run in inlines {
        let start = text.len();
        text.push_str(&run.text);
        let range = start..text.len();
        let mut style = HighlightStyle::default();
        if run.bold {
            style.font_weight = Some(FontWeight::BOLD);
        }
        if run.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if run.code {
            style.background_color = Some(rgb(mix(theme.tokens.text, theme.tokens.surface, 0.1)).into());
            style.color = Some(rgb(theme.tokens.text).into());
        }
        if run.link.is_some() {
            style.color = Some(rgb(theme.tokens.accent).into());
            style.underline =
                Some(UnderlineStyle { thickness: px(1.), color: Some(rgb(theme.tokens.accent).into()), wavy: false });
        }
        if style != HighlightStyle::default() {
            highlights.push((range, style));
        }
    }
    StyledText::new(text).with_highlights(highlights)
}

/// The plain source text of a block, for the copy button (code verbatim; other blocks as readable text).
fn block_source(block: &Block) -> String {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph { inlines } => inline_text(inlines),
        Block::CodeBlock { text, .. } => text.clone(),
        Block::BulletList { items } => {
            items.iter().map(|item| format!("- {}", blocks_text(item))).collect::<Vec<_>>().join("\n")
        }
        Block::OrderedList { start, items } => items
            .iter()
            .enumerate()
            .map(|(i, item)| format!("{}. {}", start + i as u64, blocks_text(item)))
            .collect::<Vec<_>>()
            .join("\n"),
        Block::Quote { blocks } => {
            blocks.iter().map(|b| format!("> {}", block_source(b))).collect::<Vec<_>>().join("\n")
        }
        Block::Rule => "---".to_string(),
    }
}

/// The concatenated text of inline runs.
fn inline_text(inlines: &[Inline]) -> String {
    inlines.iter().map(|run| run.text.as_str()).collect()
}

/// A list item's blocks as one line of text.
fn blocks_text(blocks: &[Block]) -> String {
    blocks.iter().map(block_source).collect::<Vec<_>>().join(" ")
}
