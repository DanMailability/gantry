//! Unified diff text (from `git diff`) as rows for the diff view: hunks, line numbers, and the changed words inside
//! changed lines.

use std::ops::Range;

/// What a diff row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// An unchanged line.
    Context,
    /// A line only in the new file.
    Added,
    /// A line only in the old file.
    Removed,
    /// A `@@ -a,b +c,d @@` line.
    Hunk,
    /// "No newline at end of file".
    NoNewline,
}

/// One row of the diff view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    /// What it is.
    pub kind: RowKind,
    /// Line number in the old file (context and removed lines).
    pub old_line: Option<u32>,
    /// Line number in the new file (context and added lines).
    pub new_line: Option<u32>,
    /// The line without its `+`, `-` or space prefix; a hunk row keeps its whole `@@` line.
    pub text: String,
    /// Byte ranges of `text` that changed inside a changed line (set by `mark_words`).
    pub words: Vec<Range<usize>>,
}

/// A parsed diff of one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedDiff {
    /// The rows in order.
    pub rows: Vec<DiffRow>,
    /// Added lines.
    pub added: u32,
    /// Removed lines.
    pub removed: u32,
}

/// Parse unified diff text. Header lines before the first hunk (`diff --git`, `index`, `---`, `+++`, rename and mode
/// lines) are skipped.
pub fn parse_diff(text: &str) -> ParsedDiff {
    let mut diff = ParsedDiff::default();
    let mut in_hunk = false;
    let (mut old_line, mut new_line) = (0u32, 0u32);
    let row = |kind, old_line, new_line, text: &str| DiffRow {
        kind,
        old_line,
        new_line,
        text: text.to_string(),
        words: Vec::new(),
    };
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            in_hunk = false;
            continue;
        }
        if let Some((old_start, new_start)) = hunk_start(line) {
            in_hunk = true;
            (old_line, new_line) = (old_start, new_start);
            diff.rows.push(row(RowKind::Hunk, None, None, line));
            continue;
        }
        if !in_hunk {
            continue;
        }
        match line.chars().next() {
            Some('+') => {
                diff.rows.push(row(RowKind::Added, None, Some(new_line), &line[1..]));
                new_line += 1;
                diff.added += 1;
            }
            Some('-') => {
                diff.rows.push(row(RowKind::Removed, Some(old_line), None, &line[1..]));
                old_line += 1;
                diff.removed += 1;
            }
            Some('\\') => diff.rows.push(row(RowKind::NoNewline, None, None, line[1..].trim_start())),
            _ => {
                // A context line; a blank one (an editor stripped its leading space) is an empty context line.
                let content = line.strip_prefix(' ').unwrap_or(line);
                diff.rows.push(row(RowKind::Context, Some(old_line), Some(new_line), content));
                old_line += 1;
                new_line += 1;
            }
        }
    }
    diff
}

/// The first old and new line numbers of a `@@ -a,b +c,d @@` header.
fn hunk_start(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(' ')?;
    let new = rest.strip_prefix('+')?.split(' ').next()?;
    let first = |range: &str| range.split(',').next()?.parse().ok();
    Some((first(old)?, first(new)?))
}

/// Mark the changed words of lines that git shows as a removed block followed by an added block of the same length:
/// the i-th removed line pairs with the i-th added line, and what lies between their common leading and trailing
/// whitespace-separated words is marked. Unequal blocks are left alone.
pub fn mark_words(rows: &mut [DiffRow]) {
    let mut i = 0;
    while i < rows.len() {
        let removed_end = run_end(rows, i, RowKind::Removed);
        let added_end = run_end(rows, removed_end, RowKind::Added);
        let (removed, added) = (removed_end - i, added_end - removed_end);
        if removed == 0 || added != removed {
            i = removed_end.max(i + 1);
            continue;
        }
        for offset in 0..removed {
            let (old, new) = (&rows[i + offset].text, &rows[removed_end + offset].text);
            if let Some((old_range, new_range)) = changed_middle(old, new) {
                rows[i + offset].words = old_range.into_iter().collect();
                rows[removed_end + offset].words = new_range.into_iter().collect();
            }
        }
        i = added_end;
    }
}

/// The end of the run of `kind` rows starting at `from`.
fn run_end(rows: &[DiffRow], from: usize, kind: RowKind) -> usize {
    from + rows[from..].iter().take_while(|row| row.kind == kind).count()
}

/// `text` as alternating runs of whitespace and non-whitespace, as byte ranges.
fn tokens(text: &str) -> Vec<Range<usize>> {
    let mut found: Vec<Range<usize>> = Vec::new();
    let mut previous_space = None;
    for (start, ch) in text.char_indices() {
        let space = ch.is_whitespace();
        match found.last_mut() {
            Some(last) if previous_space == Some(space) => last.end = start + ch.len_utf8(),
            _ => found.push(start..start + ch.len_utf8()),
        }
        previous_space = Some(space);
    }
    found
}

/// The changed range of the old line and of the new line.
type ChangedMiddle = (Option<Range<usize>>, Option<Range<usize>>);

/// What lies between the words two lines share at their start and end: one range per line, `None` when a line has
/// nothing there. `None` overall when they share no word at all (marking everything says nothing).
fn changed_middle(old: &str, new: &str) -> Option<ChangedMiddle> {
    let (old_tokens, new_tokens) = (tokens(old), tokens(new));
    let same = |a: &Range<usize>, b: &Range<usize>| old[a.clone()] == new[b.clone()];
    let shortest = old_tokens.len().min(new_tokens.len());
    let prefix = old_tokens.iter().zip(&new_tokens).take_while(|(a, b)| same(a, b)).count();
    let suffix = old_tokens
        .iter()
        .rev()
        .zip(new_tokens.iter().rev())
        .take(shortest - prefix)
        .take_while(|(a, b)| same(a, b))
        .count();
    if prefix == 0 && suffix == 0 {
        return None;
    }
    let middle = |tokens: &[Range<usize>]| {
        let end = tokens.len() - suffix;
        (prefix < end).then(|| tokens[prefix].start..tokens[end - 1].end)
    };
    Some((middle(&old_tokens), middle(&new_tokens)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = "diff --git a/src/webhook.rs b/src/webhook.rs
index 1111111..2222222 100644
--- a/src/webhook.rs
+++ b/src/webhook.rs
@@ -41,4 +41,5 @@ impl WebhookSender
     pub async fn send(&self, ev: &Event) {
-        self.client.post(&self.url).json(ev).send().await?;
+        let mut delay = BASE_DELAY;
+        self.try_once(ev).await?;
         Ok(())
";

    fn kinds(diff: &ParsedDiff) -> Vec<RowKind> {
        diff.rows.iter().map(|row| row.kind).collect()
    }

    #[test]
    fn parses_a_simple_modification() {
        let diff = parse_diff(SIMPLE);
        assert_eq!(
            kinds(&diff),
            [RowKind::Hunk, RowKind::Context, RowKind::Removed, RowKind::Added, RowKind::Added, RowKind::Context]
        );
        let numbers: Vec<(Option<u32>, Option<u32>)> = diff.rows.iter().map(|r| (r.old_line, r.new_line)).collect();
        assert_eq!(
            numbers,
            [
                (None, None),
                (Some(41), Some(41)),
                (Some(42), None),
                (None, Some(42)),
                (None, Some(43)),
                (Some(43), Some(44)),
            ]
        );
        assert_eq!(diff.rows[1].text, "    pub async fn send(&self, ev: &Event) {");
        assert_eq!(diff.rows[2].text, "        self.client.post(&self.url).json(ev).send().await?;");
    }

    #[test]
    fn hunk_header_is_a_row() {
        let diff = parse_diff(SIMPLE);
        assert_eq!(diff.rows[0].text, "@@ -41,4 +41,5 @@ impl WebhookSender");
    }

    #[test]
    fn counts_added_and_removed() {
        let diff = parse_diff(SIMPLE);
        assert_eq!((diff.added, diff.removed), (2, 1));
    }

    #[test]
    fn multiple_hunks_restart_their_numbers() {
        let text = "@@ -1,2 +1,2 @@\n a\n-b\n+c\n@@ -10 +10,2 @@ fn x()\n-old\n+new\n+more\n";
        let diff = parse_diff(text);
        let second = diff.rows.iter().position(|row| row.text.starts_with("@@ -10")).unwrap();
        assert_eq!((diff.rows[second + 1].old_line, diff.rows[second + 2].new_line), (Some(10), Some(10)));
        assert_eq!(diff.rows[second + 3].new_line, Some(11));
    }

    #[test]
    fn no_newline_marker_is_its_own_row() {
        let diff = parse_diff("@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n");
        assert_eq!(kinds(&diff), [RowKind::Hunk, RowKind::Removed, RowKind::NoNewline, RowKind::Added]);
        assert_eq!(diff.rows[2].text, "No newline at end of file");
        assert_eq!((diff.rows[2].old_line, diff.rows[2].new_line), (None, None));
    }

    #[test]
    fn crlf_lines_keep_their_text_without_the_cr() {
        let diff = parse_diff("@@ -1 +1 @@\r\n-one\r\n+two\r\n");
        assert_eq!(diff.rows[1].text, "one");
        assert_eq!(diff.rows[2].text, "two");
    }

    #[test]
    fn rename_with_edits_header_is_skipped() {
        let text = "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\nindex 1..2 100644\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-a\n+b\n";
        let diff = parse_diff(text);
        assert_eq!(kinds(&diff), [RowKind::Hunk, RowKind::Removed, RowKind::Added]);
    }

    #[test]
    fn a_removed_line_that_looks_like_a_header_is_still_removed() {
        let diff = parse_diff("@@ -1,2 +1 @@\n--- a rule\n keep\n");
        assert_eq!(diff.rows[1].kind, RowKind::Removed);
        assert_eq!(diff.rows[1].text, "-- a rule");
    }

    #[test]
    fn empty_diff_has_no_rows() {
        assert_eq!(parse_diff(""), ParsedDiff::default());
    }

    fn pair(old: &str, new: &str) -> Vec<DiffRow> {
        let mut rows = parse_diff(&format!("@@ -1 +1 @@\n-{old}\n+{new}\n")).rows;
        mark_words(&mut rows);
        rows
    }

    #[test]
    fn mark_words_marks_the_changed_token() {
        let rows = pair("call foo(a, b) now", "call foo(a, c) now");
        assert_eq!(&rows[1].text[rows[1].words[0].clone()], "b)");
        assert_eq!(&rows[2].text[rows[2].words[0].clone()], "c)");
        assert_eq!(rows[1].words.len(), 1);
    }

    #[test]
    fn mark_words_marks_an_insertion_only_on_the_added_side() {
        let rows = pair("a b", "a x b");
        assert!(rows[1].words.is_empty());
        assert_eq!(&rows[2].text[rows[2].words[0].clone()], "x ");
    }

    #[test]
    fn mark_words_skips_unequal_runs_and_totally_different_lines() {
        let mut rows = parse_diff("@@ -1,2 +1 @@\n-a b\n-c d\n+a x\n").rows;
        mark_words(&mut rows);
        assert!(rows.iter().all(|row| row.words.is_empty()));
        assert!(pair("alpha", "omega")[1].words.is_empty());
    }

    #[test]
    fn multibyte_text_ranges_are_char_boundaries() {
        let rows = pair("héllo wörld", "héllo wörlt");
        assert_eq!(&rows[1].text[rows[1].words[0].clone()], "wörld");
        assert_eq!(&rows[2].text[rows[2].words[0].clone()], "wörlt");
    }
}
