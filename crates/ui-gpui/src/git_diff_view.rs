//! The git diff page: what a file's diff is made of on screen. The pure parts (state, placeholders, copy text, text
//! segments) live here with their tests; `workspace_view` lays the page out and wires the clicks.

use std::ops::Range;
use std::rc::Rc;

use app_core::git_diff::{DiffRow, ParsedDiff, RowKind, mark_words, parse_diff};
use protocol::message::GitDiffBody;

/// What the diff area shows.
#[derive(Clone)]
pub(crate) enum DiffState {
    /// No file picked yet.
    Nothing,
    /// Asked, no answer yet.
    Loading,
    /// The parsed diff; `truncated` = the daemon cut it at its caps; `max_chars` = the longest line, for the width.
    Rows { diff: Rc<ParsedDiff>, truncated: bool, max_chars: usize },
    /// A binary file.
    Binary,
    /// A submodule pointer.
    Submodule,
    /// A file bigger than the limit, with its size in bytes.
    TooLarge(u64),
    /// Anything else, readable.
    Failed(String),
}

impl DiffState {
    /// The state for the daemon's answer: text is parsed and its changed words marked.
    pub(crate) fn from_body(body: GitDiffBody) -> DiffState {
        match body {
            GitDiffBody::Text { text, truncated } => {
                let mut diff = parse_diff(&text);
                mark_words(&mut diff.rows);
                let max_chars = diff.rows.iter().map(|row| row.text.chars().count()).max().unwrap_or(0);
                DiffState::Rows { diff: Rc::new(diff), truncated, max_chars }
            }
            GitDiffBody::Binary => DiffState::Binary,
            GitDiffBody::Submodule => DiffState::Submodule,
            GitDiffBody::TooLarge { bytes } => DiffState::TooLarge(bytes),
        }
    }
}

/// A piece of a line drawn as one element: a run of ASCII, or one non-ASCII character on its own (the shaper never
/// gets right-to-left text as one run; the display order stays the logical one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    /// The text, with tabs shown as four spaces.
    pub text: String,
}

/// `text` as segments; empty text gives none.
pub(crate) fn segments(text: &str) -> Vec<Segment> {
    let mut found = Vec::new();
    let mut run = String::new();
    for ch in text.chars() {
        match ch {
            '\t' => run.push_str("    "),
            ch if ch.is_ascii() => run.push(ch),
            ch => {
                if !run.is_empty() {
                    found.push(Segment { text: std::mem::take(&mut run) });
                }
                found.push(Segment { text: ch.to_string() });
            }
        }
    }
    if !run.is_empty() {
        found.push(Segment { text: run });
    }
    found
}

/// `text` cut at the `words` ranges into pieces, each with whether it is a changed word.
pub(crate) fn pieces<'a>(text: &'a str, words: &[Range<usize>]) -> Vec<(&'a str, bool)> {
    let mut found = Vec::new();
    let mut at = 0;
    for word in words {
        let (start, end) = (word.start.clamp(at, text.len()), word.end.clamp(at, text.len()));
        if start > at {
            found.push((&text[at..start], false));
        }
        if end > start {
            found.push((&text[start..end], true));
        }
        at = end;
    }
    if at < text.len() {
        found.push((&text[at..], false));
    }
    found
}

/// The chars of `text` in display order for a line with right-to-left letters, each with whether it lies in a changed
/// word: tabs become four spaces, brackets inside right-to-left runs are mirrored. `None` when the line needs no
/// reordering.
pub(crate) fn display_chars(text: &str, words: &[Range<usize>]) -> Option<Vec<(String, bool)>> {
    let order = app_core::bidi::display_line(text)?;
    let offsets: Vec<usize> = text.char_indices().map(|(offset, _)| offset).collect();
    Some(
        order
            .into_iter()
            .map(|(logical, ch)| {
                let shown = if ch == '\t' { "    ".to_string() } else { ch.to_string() };
                (shown, words.iter().any(|word| word.contains(&offsets[logical])))
            })
            .collect(),
    )
}

/// The text rows `range` copy: their lines without the `+`, `-` or space prefix, one per line.
pub(crate) fn copy_text(rows: &[DiffRow], range: Range<usize>) -> String {
    let end = range.end.min(rows.len());
    let start = range.start.min(end);
    rows[start..end]
        .iter()
        .filter(|row| row.kind != RowKind::NoNewline)
        .map(|row| row.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// How many digits the line-number gutters need (at least 2).
pub(crate) fn gutter_digits(rows: &[DiffRow]) -> usize {
    let largest = rows.iter().flat_map(|row| [row.old_line, row.new_line]).flatten().max().unwrap_or(0);
    largest.to_string().len().max(2)
}

/// A size as "812 KiB" or "5.0 MiB".
pub(crate) fn size_text(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB { format!("{:.1} MiB", bytes as f64 / MIB as f64) } else { format!("{} KiB", bytes.div_ceil(1024)) }
}

/// What replaces the diff when there is none to draw; `None` when `state` has rows to draw.
pub(crate) fn placeholder_text(state: &DiffState) -> Option<String> {
    match state {
        DiffState::Nothing => Some("Pick a file to see what changed.".to_string()),
        DiffState::Loading => Some("Loading…".to_string()),
        DiffState::Binary => Some("Binary file".to_string()),
        DiffState::Submodule => Some("Submodule changed".to_string()),
        DiffState::TooLarge(bytes) => Some(format!("File too large ({})", size_text(*bytes))),
        DiffState::Failed(reason) => Some(format!("git said: {reason}")),
        DiffState::Rows { diff, .. } if diff.rows.is_empty() => Some("No changes".to_string()),
        DiffState::Rows { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "@@ -1,2 +1,2 @@ fn x()\n keep\n-old line\n+new line\n";

    fn rows() -> Vec<DiffRow> {
        parse_diff(TEXT).rows
    }

    #[test]
    fn from_body_parses_and_marks_words() {
        let DiffState::Rows { diff, truncated, max_chars } =
            DiffState::from_body(GitDiffBody::Text { text: TEXT.into(), truncated: true })
        else {
            panic!("not rows")
        };
        assert!(truncated);
        assert_eq!(diff.rows.len(), 4);
        assert_eq!(&diff.rows[2].text[diff.rows[2].words[0].clone()], "old");
        assert_eq!(max_chars, "@@ -1,2 +1,2 @@ fn x()".chars().count());
        assert!(matches!(DiffState::from_body(GitDiffBody::Binary), DiffState::Binary));
        assert!(matches!(DiffState::from_body(GitDiffBody::Submodule), DiffState::Submodule));
        assert!(matches!(DiffState::from_body(GitDiffBody::TooLarge { bytes: 9 }), DiffState::TooLarge(9)));
    }

    #[test]
    fn segments_split_non_ascii_into_single_characters() {
        let texts = |text: &str| segments(text).into_iter().map(|s| s.text).collect::<Vec<_>>();
        assert_eq!(texts("let x = 1;"), ["let x = 1;"]);
        assert_eq!(texts("a שלום b"), ["a ", "ש", "ל", "ו", "ם", " b"]);
        assert_eq!(texts("\tx"), ["    x"]);
        assert!(texts("").is_empty());
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // One changed range is the case.
    fn display_chars_reorder_right_to_left_lines_and_keep_the_changed_flags() {
        let shown = display_chars("a שלום", &[2..4]).unwrap();
        let text: Vec<&str> = shown.iter().map(|(ch, _)| ch.as_str()).collect();
        assert_eq!(text, ["a", " ", "ם", "ו", "ל", "ש"]);
        let flagged: Vec<bool> = shown.iter().map(|(_, changed)| *changed).collect();
        assert_eq!(flagged, [false, false, false, false, false, true], "the first Hebrew letter is the changed one");
        assert_eq!(display_chars("plain text", &[]), None);
        assert_eq!(display_chars("\tשלום", &[]).unwrap()[0].0, "ם");
    }

    #[test]
    fn copy_text_drops_the_prefixes() {
        let rows = rows();
        assert_eq!(copy_text(&rows, 1..4), "keep\nold line\nnew line");
        assert_eq!(copy_text(&rows, 2..3), "old line");
        assert_eq!(copy_text(&rows, 0..1), "@@ -1,2 +1,2 @@ fn x()");
        assert_eq!(copy_text(&rows, 3..99), "new line", "a range past the end is cut");
    }

    #[test]
    fn gutter_width_fits_the_largest_line_number() {
        assert_eq!(gutter_digits(&rows()), 2);
        let big = parse_diff("@@ -99998,2 +99998,2 @@\n a\n-b\n+c\n");
        assert_eq!(gutter_digits(&big.rows), 5);
    }

    #[test]
    fn sizes_read_as_kib_and_mib() {
        assert_eq!(size_text(812 * 1024), "812 KiB");
        assert_eq!(size_text(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(size_text(100), "1 KiB");
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // One changed range is the case.
    fn pieces_alternate_plain_and_changed_text() {
        assert_eq!(pieces("abc def ghi", &[4..7]), [("abc ", false), ("def", true), (" ghi", false)]);
        assert_eq!(pieces("abc", &[]), [("abc", false)]);
        assert_eq!(pieces("abc", &[0..3]), [("abc", true)]);
        assert_eq!(pieces("ab cd", &[3..5]), [("ab ", false), ("cd", true)]);
        assert!(pieces("", &[]).is_empty());
    }

    #[test]
    fn placeholder_text_for_each_state() {
        let text = |state: DiffState| placeholder_text(&state);
        assert_eq!(text(DiffState::Nothing).as_deref(), Some("Pick a file to see what changed."));
        assert_eq!(text(DiffState::Loading).as_deref(), Some("Loading…"));
        assert_eq!(text(DiffState::Binary).as_deref(), Some("Binary file"));
        assert_eq!(text(DiffState::Submodule).as_deref(), Some("Submodule changed"));
        assert_eq!(text(DiffState::TooLarge(5 * 1024 * 1024)).as_deref(), Some("File too large (5.0 MiB)"));
        assert_eq!(text(DiffState::Failed("boom".into())).as_deref(), Some("git said: boom"));
        let empty = DiffState::Rows { diff: Rc::new(ParsedDiff::default()), truncated: false, max_chars: 0 };
        assert_eq!(text(empty).as_deref(), Some("No changes"));
        let with_rows = DiffState::Rows { diff: Rc::new(parse_diff(TEXT)), truncated: false, max_chars: 20 };
        assert_eq!(text(with_rows), None);
    }

    #[test]
    fn hunk_rows_copy_as_themselves_and_no_newline_rows_are_skipped() {
        let parsed = parse_diff("@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n");
        assert_eq!(copy_text(&parsed.rows, 0..4), "@@ -1 +1 @@\na\nb");
        assert!(parsed.rows.iter().any(|row| row.kind == RowKind::NoNewline));
    }
}
