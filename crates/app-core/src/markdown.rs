//! A small block model for rendering Markdown as formatted text (headings, paragraphs, code blocks, lists,
//! blockquotes, rules) with inline runs (bold, italic, inline code, links). The view in `ui-gpui` draws these
//! blocks; the parsing and its shape are tested here, with no display.
//!
//! This is a deliberately lossy model: it keeps what the reading view draws and flattens the rest. Tables become one
//! paragraph per row (cells joined with " | "), images become a link run of their alt text, and HTML is dropped.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// One run of inline text with its styling. A link run carries the destination URL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inline {
    /// The run's text.
    pub text: String,
    /// Drawn bold.
    pub bold: bool,
    /// Drawn italic.
    pub italic: bool,
    /// Drawn as inline code (monospace, tinted).
    pub code: bool,
    /// The link destination, when the run is a link (or an image's alt text).
    pub link: Option<String>,
}

/// A block-level element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// A heading, `level` 1..=6.
    Heading {
        /// Heading level, 1 (largest) to 6.
        level: u8,
        /// The heading's inline runs.
        inlines: Vec<Inline>,
    },
    /// A paragraph of inline runs.
    Paragraph {
        /// The paragraph's inline runs.
        inlines: Vec<Inline>,
    },
    /// A fenced or indented code block, whitespace preserved.
    CodeBlock {
        /// The info string of a fenced block (its language), if any.
        lang: Option<String>,
        /// The code, newlines kept, with no trailing newline.
        text: String,
    },
    /// An unordered list; each item is its own sequence of blocks.
    BulletList {
        /// The list items, each a sequence of blocks.
        items: Vec<Vec<Block>>,
    },
    /// An ordered list starting at `start`; each item is its own sequence of blocks.
    OrderedList {
        /// The first item's number.
        start: u64,
        /// The list items, each a sequence of blocks.
        items: Vec<Vec<Block>>,
    },
    /// A blockquote, its own sequence of blocks.
    Quote {
        /// The quoted blocks.
        blocks: Vec<Block>,
    },
    /// A thematic break (`---`).
    Rule,
}

/// Parse `text` as Markdown into a flat sequence of blocks.
pub fn parse_markdown(text: &str) -> Vec<Block> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    let events: Vec<Event> = Parser::new_ext(text, options).collect();
    let mut cursor = Cursor { events: &events, at: 0 };
    cursor.blocks()
}

/// The styling in force while inline runs are collected.
#[derive(Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
    link_depth: u32,
}

/// A position in the event stream, with the recursive-descent parse built on it.
struct Cursor<'a> {
    events: &'a [Event<'a>],
    at: usize,
}

impl<'a> Cursor<'a> {
    /// Parse block-level elements until the stream ends or the current container closes (an `End` that this level did
    /// not open). The caller consumes that closing `End`.
    fn blocks(&mut self) -> Vec<Block> {
        // A local copy of the (Copy) slice reference: event references then borrow the slice data, not `self`, so the
        // cursor position (`self.at`) can advance while an event is in hand.
        let events = self.events;
        let mut blocks = Vec::new();
        while let Some(event) = events.get(self.at) {
            match event {
                Event::Start(tag) => {
                    self.at += 1;
                    if let Some(block) = self.block_for(tag) {
                        blocks.push(block);
                    }
                }
                Event::Rule => {
                    self.at += 1;
                    blocks.push(Block::Rule);
                }
                // Loose text between blocks (rare); treat it as its own paragraph.
                Event::Text(_) | Event::Code(_) => {
                    let mut inlines = Vec::new();
                    self.inline_until_block(&mut inlines);
                    if !inlines.is_empty() {
                        blocks.push(Block::Paragraph { inlines });
                    }
                }
                // An `End` for a container this level did not open: hand it back to the opener.
                Event::End(_) => break,
                _ => self.at += 1,
            }
        }
        blocks
    }

    /// Build the block for `tag`, consuming through its matching `End`.
    fn block_for(&mut self, tag: &Tag<'a>) -> Option<Block> {
        match tag {
            Tag::Heading { level, .. } => {
                let inlines = self.inlines(TagEnd::Heading(*level));
                Some(Block::Heading { level: heading_level(*level), inlines })
            }
            Tag::Paragraph => {
                let inlines = self.inlines(TagEnd::Paragraph);
                (!inlines.is_empty()).then_some(Block::Paragraph { inlines })
            }
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        let lang = info.split_whitespace().next().unwrap_or("");
                        (!lang.is_empty()).then(|| lang.to_string())
                    }
                    CodeBlockKind::Indented => None,
                };
                let text = self.code_text();
                Some(Block::CodeBlock { lang, text })
            }
            Tag::List(first) => Some(self.list(*first)),
            Tag::BlockQuote(_) => {
                let blocks = self.blocks();
                self.skip_end();
                Some(Block::Quote { blocks })
            }
            Tag::Table(_) => Some(Block::Paragraph { inlines: self.table() }),
            // A stray item/cell start outside its list/table, or anything else: consume its contents as a paragraph.
            _ => {
                let inlines = self.inlines_to_matching_end();
                (!inlines.is_empty()).then_some(Block::Paragraph { inlines })
            }
        }
    }

    /// The items of a list; `first` is `Some(n)` for an ordered list starting at `n`, `None` for a bullet list.
    fn list(&mut self, first: Option<u64>) -> Block {
        let events = self.events;
        let mut items = Vec::new();
        while let Some(event) = events.get(self.at) {
            match event {
                Event::Start(Tag::Item) => {
                    self.at += 1;
                    items.push(self.blocks());
                    self.skip_end(); // the Item's End
                }
                Event::End(_) => break,
                _ => self.at += 1,
            }
        }
        self.skip_end(); // the List's End
        match first {
            Some(start) => Block::OrderedList { start, items },
            None => Block::BulletList { items },
        }
    }

    /// Collect inline runs up to (and consuming) `end`.
    fn inlines(&mut self, end: TagEnd) -> Vec<Inline> {
        let events = self.events;
        let mut runs = Vec::new();
        let mut style = Style::default();
        while let Some(event) = events.get(self.at) {
            if let Event::End(seen) = event
                && *seen == end
            {
                self.at += 1;
                break;
            }
            self.inline_event(&mut runs, &mut style);
        }
        merge(runs)
    }

    /// Collect inline runs up to (not consuming) the next block boundary (`Start`/`End`/`Rule`).
    fn inline_until_block(&mut self, runs: &mut Vec<Inline>) {
        let events = self.events;
        let mut style = Style::default();
        let mut collected = Vec::new();
        while let Some(event) = events.get(self.at) {
            if matches!(event, Event::Start(_) | Event::End(_) | Event::Rule) {
                break;
            }
            self.inline_event(&mut collected, &mut style);
        }
        runs.extend(merge(collected));
    }

    /// Collect inline runs up to (and consuming) the matching `End` of a container already entered, whatever it is.
    fn inlines_to_matching_end(&mut self) -> Vec<Inline> {
        let events = self.events;
        let mut runs = Vec::new();
        let mut style = Style::default();
        let mut depth = 1u32;
        while let Some(event) = events.get(self.at) {
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        self.at += 1;
                        break;
                    }
                }
                _ => {}
            }
            self.inline_event(&mut runs, &mut style);
        }
        merge(runs)
    }

    /// Fold one event into the inline runs, tracking styling across nested emphasis and links.
    fn inline_event(&mut self, runs: &mut Vec<Inline>, style: &mut Style) {
        let events = self.events;
        let event = &events[self.at];
        self.at += 1;
        match event {
            Event::Text(text) => runs.push(run(text, *style, None)),
            Event::Code(text) => runs.push(Inline { text: text.to_string(), code: true, ..Default::default() }),
            Event::SoftBreak | Event::HardBreak => runs.push(run(" ", *style, None)),
            Event::Start(Tag::Strong) => style.bold = true,
            Event::End(TagEnd::Strong) => style.bold = false,
            Event::Start(Tag::Emphasis) => style.italic = true,
            Event::End(TagEnd::Emphasis) => style.italic = false,
            Event::Start(Tag::Link { dest_url, .. }) | Event::Start(Tag::Image { dest_url, .. }) => {
                let link = dest_url.to_string();
                style.link_depth += 1;
                self.linked(runs, style, link);
                style.link_depth -= 1;
            }
            // Strikethrough and anything else: ignore the marker, keep the text.
            _ => {}
        }
    }

    /// Collect a link's or image's inner text (up to its own `End`) as runs carrying `link`.
    fn linked(&mut self, runs: &mut Vec<Inline>, style: &mut Style, link: String) {
        let events = self.events;
        while let Some(event) = events.get(self.at) {
            match event {
                Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {
                    self.at += 1;
                    break;
                }
                Event::Text(text) => {
                    runs.push(run(text, *style, Some(link.clone())));
                    self.at += 1;
                }
                Event::Code(text) => {
                    runs.push(Inline {
                        text: text.to_string(),
                        code: true,
                        link: Some(link.clone()),
                        ..Default::default()
                    });
                    self.at += 1;
                }
                Event::SoftBreak | Event::HardBreak => {
                    runs.push(run(" ", *style, Some(link.clone())));
                    self.at += 1;
                }
                Event::Start(Tag::Strong) => {
                    style.bold = true;
                    self.at += 1;
                }
                Event::End(TagEnd::Strong) => {
                    style.bold = false;
                    self.at += 1;
                }
                Event::Start(Tag::Emphasis) => {
                    style.italic = true;
                    self.at += 1;
                }
                Event::End(TagEnd::Emphasis) => {
                    style.italic = false;
                    self.at += 1;
                }
                _ => self.at += 1,
            }
        }
    }

    /// The text of the code block the cursor is inside, through its `End`, with no trailing newline.
    fn code_text(&mut self) -> String {
        let events = self.events;
        let mut text = String::new();
        while let Some(event) = events.get(self.at) {
            match event {
                Event::End(TagEnd::CodeBlock) => {
                    self.at += 1;
                    break;
                }
                Event::Text(chunk) => {
                    text.push_str(chunk);
                    self.at += 1;
                }
                _ => self.at += 1,
            }
        }
        while text.ends_with('\n') {
            text.pop();
        }
        text
    }

    /// Flatten a table into one line: every cell's text, cells joined with " | ", rows joined with a break run.
    fn table(&mut self) -> Vec<Inline> {
        let events = self.events;
        let mut runs: Vec<Inline> = Vec::new();
        let mut first_cell = true;
        let mut depth = 1u32;
        let mut style = Style::default();
        while let Some(event) = events.get(self.at) {
            match event {
                Event::Start(Tag::Table(_)) => depth += 1,
                Event::End(TagEnd::Table) => {
                    depth -= 1;
                    if depth == 0 {
                        self.at += 1;
                        break;
                    }
                }
                Event::Start(Tag::TableRow) | Event::Start(Tag::TableHead) => {
                    if !runs.is_empty() {
                        runs.push(run("\n", Style::default(), None));
                    }
                    first_cell = true;
                }
                Event::Start(Tag::TableCell) => {
                    if !first_cell {
                        runs.push(run(" | ", Style::default(), None));
                    }
                    first_cell = false;
                }
                Event::Text(text) => runs.push(run(text, style, None)),
                Event::Code(text) => runs.push(Inline { text: text.to_string(), code: true, ..Default::default() }),
                Event::Start(Tag::Strong) => style.bold = true,
                Event::End(TagEnd::Strong) => style.bold = false,
                Event::Start(Tag::Emphasis) => style.italic = true,
                Event::End(TagEnd::Emphasis) => style.italic = false,
                _ => {}
            }
            self.at += 1;
        }
        merge(runs)
    }

    /// Consume one `End` event if the cursor is on one.
    fn skip_end(&mut self) {
        if matches!(self.events.get(self.at), Some(Event::End(_))) {
            self.at += 1;
        }
    }
}

/// An inline run of `text` in `style`, carrying `link`.
fn run(text: &str, style: Style, link: Option<String>) -> Inline {
    Inline {
        text: text.to_string(),
        bold: style.bold,
        italic: style.italic,
        code: false,
        link: link.or_else(|| (style.link_depth > 0).then(String::new)),
    }
}

/// Merge adjacent runs with identical styling, and drop empty ones, so a line draws as few elements.
fn merge(runs: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    for current in runs {
        if current.text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last)
                if last.bold == current.bold
                    && last.italic == current.italic
                    && last.code == current.code
                    && last.link == current.link =>
            {
                last.text.push_str(&current.text);
            }
            _ => out.push(current),
        }
    }
    out
}

/// A `HeadingLevel` as 1..=6.
fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Inline {
        Inline { text: text.to_string(), ..Default::default() }
    }

    #[test]
    fn headings_keep_their_level_and_text() {
        let blocks = parse_markdown("# One\n\n### Three");
        assert_eq!(
            blocks,
            vec![
                Block::Heading { level: 1, inlines: vec![plain("One")] },
                Block::Heading { level: 3, inlines: vec![plain("Three")] },
            ]
        );
    }

    #[test]
    fn a_paragraph_joins_soft_wrapped_lines_with_spaces() {
        let blocks = parse_markdown("hello\nworld");
        assert_eq!(blocks, vec![Block::Paragraph { inlines: vec![plain("hello world")] }]);
    }

    #[test]
    fn bold_and_italic_runs_carry_their_flags() {
        let Block::Paragraph { inlines } = &parse_markdown("a **bold** and *italic* b")[0] else {
            panic!("not a paragraph");
        };
        assert_eq!(
            inlines,
            &vec![
                plain("a "),
                Inline { text: "bold".into(), bold: true, ..Default::default() },
                plain(" and "),
                Inline { text: "italic".into(), italic: true, ..Default::default() },
                plain(" b"),
            ]
        );
    }

    #[test]
    fn nested_emphasis_is_both_bold_and_italic() {
        let Block::Paragraph { inlines } = &parse_markdown("***both***")[0] else { panic!("not a paragraph") };
        assert_eq!(inlines, &vec![Inline { text: "both".into(), bold: true, italic: true, ..Default::default() }]);
    }

    #[test]
    fn inline_code_is_a_code_run() {
        let Block::Paragraph { inlines } = &parse_markdown("call `foo()` now")[0] else { panic!("not a paragraph") };
        assert_eq!(
            inlines,
            &vec![plain("call "), Inline { text: "foo()".into(), code: true, ..Default::default() }, plain(" now"),]
        );
    }

    #[test]
    fn fenced_code_keeps_its_language_and_whitespace() {
        let blocks = parse_markdown("```rust\nfn main() {\n    let x = 1;\n}\n```");
        assert_eq!(
            blocks,
            vec![Block::CodeBlock { lang: Some("rust".into()), text: "fn main() {\n    let x = 1;\n}".into() }]
        );
    }

    #[test]
    fn fenced_code_without_a_language_has_none() {
        let blocks = parse_markdown("```\nplain\n```");
        assert_eq!(blocks, vec![Block::CodeBlock { lang: None, text: "plain".into() }]);
    }

    #[test]
    fn a_bullet_list_has_one_item_sequence_each() {
        let blocks = parse_markdown("- one\n- two");
        assert_eq!(
            blocks,
            vec![Block::BulletList {
                items: vec![
                    vec![Block::Paragraph { inlines: vec![plain("one")] }],
                    vec![Block::Paragraph { inlines: vec![plain("two")] }],
                ]
            }]
        );
    }

    #[test]
    fn an_ordered_list_keeps_its_start() {
        let blocks = parse_markdown("3. three\n4. four");
        let Block::OrderedList { start, items } = &blocks[0] else { panic!("not an ordered list") };
        assert_eq!(*start, 3);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn a_nested_list_is_a_block_inside_its_item() {
        let blocks = parse_markdown("- outer\n  - inner");
        let Block::BulletList { items } = &blocks[0] else { panic!("not a bullet list") };
        assert_eq!(items.len(), 1);
        assert!(items[0].iter().any(|block| matches!(block, Block::BulletList { .. })), "nested list is kept");
    }

    #[test]
    fn a_blockquote_holds_its_own_blocks() {
        let blocks = parse_markdown("> quoted\n>\n> more");
        let Block::Quote { blocks: inner } = &blocks[0] else { panic!("not a quote") };
        assert_eq!(inner.len(), 2);
        assert!(matches!(inner[0], Block::Paragraph { .. }));
    }

    #[test]
    fn a_link_run_carries_its_destination() {
        let Block::Paragraph { inlines } = &parse_markdown("see [the docs](https://example.com) here")[0] else {
            panic!("not a paragraph");
        };
        assert_eq!(
            inlines,
            &vec![
                plain("see "),
                Inline { text: "the docs".into(), link: Some("https://example.com".into()), ..Default::default() },
                plain(" here"),
            ]
        );
    }

    #[test]
    fn a_thematic_break_is_a_rule() {
        let blocks = parse_markdown("above\n\n---\n\nbelow");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph { inlines: vec![plain("above")] },
                Block::Rule,
                Block::Paragraph { inlines: vec![plain("below")] },
            ]
        );
    }

    #[test]
    fn a_table_flattens_to_cells_joined_with_bars() {
        let blocks = parse_markdown("| a | b |\n|---|---|\n| 1 | 2 |");
        let Block::Paragraph { inlines } = &blocks[0] else { panic!("not a paragraph") };
        let text: String = inlines.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(text, "a | b\n1 | 2");
    }

    #[test]
    fn an_image_becomes_a_link_run_of_its_alt_text() {
        let Block::Paragraph { inlines } = &parse_markdown("![a cat](cat.png)")[0] else { panic!("not a paragraph") };
        assert_eq!(inlines, &vec![Inline { text: "a cat".into(), link: Some("cat.png".into()), ..Default::default() }]);
    }

    #[test]
    fn a_mixed_document_parses_to_the_expected_block_kinds() {
        let doc = "# Title\n\nA paragraph with **bold**.\n\n```\ncode\n```\n\n- a\n- b\n\n> quote\n\n---\n";
        let kinds: Vec<&str> = parse_markdown(doc)
            .iter()
            .map(|block| match block {
                Block::Heading { .. } => "heading",
                Block::Paragraph { .. } => "paragraph",
                Block::CodeBlock { .. } => "code",
                Block::BulletList { .. } => "bullets",
                Block::OrderedList { .. } => "ordered",
                Block::Quote { .. } => "quote",
                Block::Rule => "rule",
            })
            .collect();
        assert_eq!(kinds, vec!["heading", "paragraph", "code", "bullets", "quote", "rule"]);
    }

    #[test]
    fn empty_input_has_no_blocks() {
        assert!(parse_markdown("").is_empty());
    }
}
