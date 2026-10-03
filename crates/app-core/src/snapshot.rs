//! Plain-data view of the terminal screen, ready for a renderer to paint.

/// An 8-bit-per-channel color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb {
    /// Build a color from `0xRRGGBB`.
    pub const fn hex(value: u32) -> Self {
        Rgb { r: (value >> 16) as u8, g: (value >> 8) as u8, b: value as u8 }
    }
}

/// Fully resolved visual attributes of one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellStyle {
    /// Text color (after inverse/dim/hidden are applied).
    pub fg: Rgb,
    /// Background color (after inverse and selection are applied).
    pub bg: Rgb,
    /// Bold weight.
    pub bold: bool,
    /// Italic style.
    pub italic: bool,
    /// Any underline style.
    pub underline: bool,
    /// Strikethrough.
    pub strikethrough: bool,
}

/// One grid cell.
#[derive(Clone, Debug, PartialEq)]
pub struct CellView {
    /// The base character.
    pub ch: char,
    /// Zero-width combining characters attached to `ch` (e.g. Hebrew niqqud).
    pub combining: Option<Vec<char>>,
    /// Visual attributes.
    pub style: CellStyle,
    /// The character occupies this cell and the next one.
    pub wide: bool,
    /// Placeholder cell following a wide character; draws nothing itself.
    pub spacer: bool,
}

/// Where and how to draw the cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorView {
    /// Column in the viewport.
    pub col: usize,
    /// Row in the viewport (0 = top).
    pub row: usize,
    /// The cursor sits on a wide character.
    pub wide: bool,
    /// Character under the cursor.
    pub ch: char,
    /// Style of the cell under the cursor.
    pub style: CellStyle,
}

/// Everything needed to paint one frame of a terminal.
#[derive(Clone, Debug)]
pub struct ScreenSnapshot {
    /// Number of columns.
    pub cols: usize,
    /// Visible rows, top to bottom; each has `cols` cells.
    pub rows: Vec<Vec<CellView>>,
    /// Cursor, if visible and inside the viewport.
    pub cursor: Option<CursorView>,
    /// Default background, for filling the pane.
    pub background: Rgb,
    /// Cursor color.
    pub cursor_color: Rgb,
}

/// A run of adjacent cells that can be drawn as one shaped text run.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// First column of the span.
    pub start_col: usize,
    /// Number of grid columns the span covers.
    pub width_cols: usize,
    /// Text of the span, including combining marks.
    pub text: String,
    /// Shared style of all cells in the span.
    pub style: CellStyle,
    /// The span is a single wide character (drawn without forcing cell advances).
    pub wide: bool,
}

/// Group a row into spans of identical style. Spacer cells are skipped.
///
/// Only ASCII text and frame characters (box drawing, blocks) merge into longer spans; every other character —
/// wide, right-to-left, or likely drawn from a fallback font — is its own span. The renderer pins each span to its
/// cells by counting glyphs, so a character that shapes into more or fewer glyphs than cells must not share a span.
pub fn row_spans(row: &[CellView]) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut last_kind = None;
    for (col, cell) in row.iter().enumerate() {
        if cell.spacer {
            continue;
        }
        let mut text = String::from(cell.ch);
        if let Some(marks) = &cell.combining {
            text.extend(marks.iter());
        }
        let kind = merge_kind(cell);
        let merge_into_last = kind.is_some() && kind == last_kind;
        last_kind = kind;
        if merge_into_last
            && let Some(last) = spans.last_mut()
            && last.style == cell.style
            && last.start_col + last.width_cols == col
        {
            last.text.push_str(&text);
            last.width_cols += 1;
            continue;
        }
        spans.push(Span {
            start_col: col,
            width_cols: if cell.wide { 2 } else { 1 },
            text,
            style: cell.style,
            wide: cell.wide,
        });
    }
    spans
}

/// Which cells may share a span: ASCII with ASCII, frame characters with frame characters; `None` never merges.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MergeKind {
    Ascii,
    Frame,
}

fn merge_kind(cell: &CellView) -> Option<MergeKind> {
    if cell.wide || cell.combining.is_some() {
        None
    } else if cell.ch.is_ascii() {
        Some(MergeKind::Ascii)
    } else if matches!(cell.ch, '\u{2500}'..='\u{259F}') {
        Some(MergeKind::Frame)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ch: char, fg: u32) -> CellView {
        CellView {
            ch,
            combining: None,
            style: CellStyle {
                fg: Rgb::hex(fg),
                bg: Rgb::hex(0),
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
            },
            wide: false,
            spacer: false,
        }
    }

    #[test]
    fn same_style_cells_merge_into_one_span() {
        let row = vec![cell('a', 1), cell('b', 1), cell('c', 2)];
        let spans = row_spans(&row);
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start_col, spans[0].width_cols, spans[0].text.as_str()), (0, 2, "ab"));
        assert_eq!((spans[1].start_col, spans[1].width_cols, spans[1].text.as_str()), (2, 1, "c"));
    }

    #[test]
    fn wide_char_is_its_own_two_column_span() {
        let mut wide = cell('漢', 1);
        wide.wide = true;
        let mut spacer = cell(' ', 1);
        spacer.spacer = true;
        let row = vec![cell('a', 1), wide, spacer, cell('b', 1)];
        let spans = row_spans(&row);
        let summary: Vec<_> = spans.iter().map(|s| (s.start_col, s.width_cols, s.text.as_str(), s.wide)).collect();
        assert_eq!(summary, vec![(0, 1, "a", false), (1, 2, "漢", true), (3, 1, "b", false)]);
    }

    #[test]
    fn rtl_chars_get_single_cell_spans() {
        let row: Vec<_> = "aשלום b".chars().map(|c| cell(c, 1)).collect();
        let summary: Vec<_> = row_spans(&row).iter().map(|s| (s.start_col, s.width_cols, s.text.clone())).collect();
        let expected: Vec<_> = [(0, 1, "a"), (1, 1, "ש"), (2, 1, "ל"), (3, 1, "ו"), (4, 1, "ם"), (5, 2, " b")]
            .into_iter()
            .map(|(c, w, t)| (c, w, t.to_string()))
            .collect();
        assert_eq!(summary, expected);
    }

    #[test]
    fn combining_marks_stay_with_their_base_char() {
        let mut base = cell('ש', 1);
        base.combining = Some(vec!['\u{05B8}']);
        let spans = row_spans(&[base]);
        assert_eq!(spans[0].text, "ש\u{05B8}");
        assert_eq!(spans[0].width_cols, 1);
    }

    #[test]
    fn fallback_symbols_get_their_own_span() {
        let row: Vec<_> = "a⏺✓b".chars().map(|c| cell(c, 1)).collect();
        let summary: Vec<_> = row_spans(&row).iter().map(|s| (s.start_col, s.width_cols, s.text.clone())).collect();
        let expected: Vec<_> = [(0, 1, "a"), (1, 1, "⏺"), (2, 1, "✓"), (3, 1, "b")]
            .into_iter()
            .map(|(c, w, t)| (c, w, t.to_string()))
            .collect();
        assert_eq!(summary, expected);
    }

    #[test]
    fn ascii_text_and_frames_still_merge() {
        let row: Vec<_> = "ab╭──╮cd".chars().map(|c| cell(c, 1)).collect();
        let texts: Vec<_> = row_spans(&row).iter().map(|s| s.text.clone()).collect();
        assert_eq!(texts, vec!["ab", "╭──╮", "cd"]);
    }
}
