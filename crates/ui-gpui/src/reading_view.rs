//! The reading view: a Claude pane's conversation drawn as formatted Markdown instead of the terminal grid. It reads
//! the clean per-message text from the session transcript (`claude_index::search::read_messages`), parses each
//! message as Markdown (`app_core::markdown`), and lays the blocks out with GPUI. It is read-only and refreshes on a
//! light timer; no live token streaming, per completed message is enough.
//!
//! Local Claude sessions only. A remote or not-yet-started session shows a short placeholder instead.

use std::path::PathBuf;
use std::time::Duration;

use app_core::markdown::{Block, Inline, parse_markdown};
use app_core::theme_colors::mix;
use claude_index::index::transcript_path;
use claude_index::search::{Message, Speaker, read_messages};
use gpui::{
    App, Context, FocusHandle, Focusable, FontStyle, FontWeight, HighlightStyle, IntoElement, ScrollHandle,
    SharedString, StyledText, Task, UnderlineStyle, Window, div, prelude::*, px, rgb,
};

use crate::settings_store::SettingsStore;
use crate::terminal_view::{installed_families, installed_family};
use crate::theme::{RADIUS, RADIUS_SMALL, Theme, UI_FONT, theme};

/// How often the view re-reads the transcript while it is shown.
const REFRESH: Duration = Duration::from_millis(1500);

/// Where a reading view gets its text.
pub(crate) enum ReadingSource {
    /// A local Claude session: the Claude config dir and the session id. The transcript file is found by id, so it can
    /// resolve later if it does not exist yet (a fresh session before its first message).
    Session {
        /// The Claude config dir the session lives under.
        config_dir: PathBuf,
        /// The session id (the transcript file's stem).
        session_id: String,
    },
    /// Nothing to read here; show this one-line message (a remote or non-Claude pane).
    Message(String),
}

/// One message turned into blocks, with who wrote it.
struct Shown {
    speaker: Speaker,
    blocks: Vec<Block>,
}

/// A Claude pane's conversation as formatted Markdown.
pub struct ReadingView {
    focus_handle: FocusHandle,
    source: ReadingSource,
    /// The transcript path once found.
    path: Option<PathBuf>,
    /// Messages parsed so far.
    messages: Vec<Shown>,
    /// Byte offset after the last complete line read, for the next incremental read.
    offset: u64,
    /// At least one successful read has happened.
    loaded: bool,
    scroll: ScrollHandle,
    /// The refresh timer; dropped with the view.
    _refresh: Task<()>,
}

impl Focusable for ReadingView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ReadingView {
    pub(crate) fn new(source: ReadingSource, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let refresh = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update_in(cx, |view, _window, cx| view.refresh(cx)).is_err() {
                    return;
                }
            }
        });
        let mut view = ReadingView {
            focus_handle: cx.focus_handle(),
            source,
            path: None,
            messages: Vec::new(),
            offset: 0,
            loaded: false,
            scroll: ScrollHandle::new(),
            _refresh: refresh,
        };
        view.refresh(cx);
        view
    }

    /// Whether the scroll position is at (or within a few pixels of) the bottom.
    fn at_bottom(&self) -> bool {
        let max = f32::from(self.scroll.max_offset().y);
        let scrolled = -f32::from(self.scroll.offset().y);
        max <= 1.0 || scrolled >= max - 8.0
    }

    /// Re-read the transcript from the last offset and append any new messages. Keeps the view pinned to the bottom
    /// when it already was (so a running session's new replies stay in view), but never scrolls on the first load.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let ReadingSource::Session { config_dir, session_id } = &self.source else { return };
        if self.path.is_none() {
            self.path = transcript_path(config_dir, session_id);
        }
        let Some(path) = self.path.clone() else { return };
        let Ok((new, end)) = read_messages(&path, self.offset) else { return };
        let was_at_bottom = self.at_bottom();
        let added = !new.is_empty();
        for Message { speaker, text } in new {
            self.messages.push(Shown { speaker, blocks: parse_markdown(&text) });
        }
        self.offset = end;
        let first_load = !self.loaded;
        self.loaded = true;
        if added || first_load {
            if added && !first_load && was_at_bottom {
                self.scroll.scroll_to_bottom();
            }
            cx.notify();
        }
    }

    /// The one-line placeholder to show instead of a conversation, if there is nothing to draw yet.
    fn placeholder(&self) -> Option<String> {
        match &self.source {
            ReadingSource::Message(text) => Some(text.clone()),
            ReadingSource::Session { .. } if self.path.is_none() => {
                Some("Waiting for the conversation to start.".to_string())
            }
            ReadingSource::Session { .. } if self.messages.is_empty() => {
                Some("No messages in this conversation yet.".to_string())
            }
            ReadingSource::Session { .. } => None,
        }
    }
}

impl Render for ReadingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let base = div()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(rgb(theme.tokens.ground))
            .text_color(rgb(theme.tokens.text))
            .font_family(UI_FONT);
        if let Some(text) = self.placeholder() {
            return base
                .flex()
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
        let messages = self.messages.iter().map(|message| render_message(message, &mono, &theme)).collect::<Vec<_>>();
        base.id("reading")
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                // A centred, comfortably wide reading column.
                div().w_full().flex().flex_col().items_center().px(theme.ui(20.)).py(theme.ui(22.)).child(
                    div().w_full().max_w(theme.ui(760.)).flex().flex_col().gap(theme.ui(26.)).children(messages),
                ),
            )
            .into_any_element()
    }
}

/// One message: a speaker label, then its blocks.
fn render_message(message: &Shown, mono: &SharedString, theme: &Theme) -> impl IntoElement {
    let (label, fill, ink) = match message.speaker {
        Speaker::You => ("You", theme.tokens.accent, theme.tokens.on_accent),
        Speaker::Claude => ("Claude", theme.tokens.raised, theme.tokens.text),
    };
    div()
        .w_full()
        .flex()
        .flex_col()
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
                .child(label)
                .self_start(),
        )
        .child(div().w_full().flex().flex_col().gap(theme.ui(10.)).children(render_blocks(
            &message.blocks,
            mono,
            theme,
        )))
}

/// The blocks of a message (or of a list item / blockquote), in order.
fn render_blocks(blocks: &[Block], mono: &SharedString, theme: &Theme) -> Vec<gpui::AnyElement> {
    blocks.iter().map(|block| render_block(block, mono, theme)).collect()
}

/// One block as a GPUI element.
fn render_block(block: &Block, mono: &SharedString, theme: &Theme) -> gpui::AnyElement {
    match block {
        Block::Heading { level, inlines } => {
            // Size variety by level; everything bold.
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
                // A blank line keeps its height.
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
/// underline for links. The runs are concatenated and each styled over its byte range.
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
