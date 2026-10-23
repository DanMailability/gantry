//! The reading view: a Claude pane's conversation drawn as formatted Markdown instead of the terminal grid. The
//! conversation text comes from the pane's host daemon (`read_transcript` → `Transcript`), the same way the git diff
//! view fetches remote content, so it works for local and SSH-host sessions alike. The `WorkspaceView` drives the
//! requests and feeds answers in here through [`ReadingView::append`]; this view only renders and scrolls.
//!
//! Each message's text is parsed with `app_core::markdown` and laid out with GPUI. Read-only, per completed message
//! (no live token streaming).

use app_core::markdown::{Block, Inline, parse_markdown};
use app_core::theme_colors::mix;
use gpui::{
    App, Context, FocusHandle, Focusable, FontStyle, FontWeight, HighlightStyle, IntoElement, ScrollHandle,
    SharedString, StyledText, UnderlineStyle, Window, div, prelude::*, px, rgb,
};
use protocol::message::{Speaker, TranscriptMessage};

use crate::settings_store::SettingsStore;
use crate::terminal_view::{installed_families, installed_family};
use crate::theme::{RADIUS, RADIUS_SMALL, Theme, UI_FONT, theme};

/// One message turned into blocks, with who wrote it.
struct Shown {
    you: bool,
    blocks: Vec<Block>,
}

/// A Claude pane's conversation as formatted Markdown.
pub struct ReadingView {
    focus_handle: FocusHandle,
    /// Messages parsed so far.
    messages: Vec<Shown>,
    /// A fixed one-line notice shown instead of the conversation (e.g. the host's daemon is too old).
    notice: Option<String>,
    /// At least one `Transcript` answer has arrived.
    loaded: bool,
    /// The transcript file was found on the host.
    found: bool,
    scroll: ScrollHandle,
}

impl Focusable for ReadingView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ReadingView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        ReadingView {
            focus_handle: cx.focus_handle(),
            messages: Vec::new(),
            notice: None,
            loaded: false,
            found: false,
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

    /// Append the messages from a `Transcript` answer. Keeps the view pinned to the bottom when it already was (so a
    /// running session's new replies stay in view), but never scrolls on the first load.
    pub(crate) fn append(&mut self, messages: Vec<TranscriptMessage>, found: bool, cx: &mut Context<Self>) {
        let was_at_bottom = self.at_bottom();
        let added = !messages.is_empty();
        let first_load = !self.loaded;
        for message in messages {
            let you = matches!(message.speaker, Speaker::You);
            self.messages.push(Shown { you, blocks: parse_markdown(&message.text) });
        }
        let found_changed = self.found != found;
        self.loaded = true;
        self.found = found;
        if added && !first_load && was_at_bottom {
            self.scroll.scroll_to_bottom();
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

    /// The one-line placeholder to show instead of a conversation, if there is nothing to draw yet.
    fn placeholder(&self) -> Option<String> {
        if let Some(notice) = &self.notice {
            return Some(notice.clone());
        }
        if self.messages.is_empty() {
            return Some(
                if self.loaded && self.found {
                    "No messages in this conversation yet."
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
    let (label, fill, ink) = if message.you {
        ("You", theme.tokens.accent, theme.tokens.on_accent)
    } else {
        ("Claude", theme.tokens.raised, theme.tokens.text)
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
