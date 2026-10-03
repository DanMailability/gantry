//! A single-line text field's contents and cursor; the GUI draws it and forwards editing keys.

/// Text plus a cursor and a selection anchor (byte offsets on character boundaries). The selection
/// is the range between `anchor` and `cursor`; it is empty when they are equal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextField {
    text: String,
    cursor: usize,
    anchor: usize,
}

/// What a character counts as for word movement and double-click selection.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Word,
    Space,
    Other,
}

fn class(c: char) -> Class {
    if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

impl TextField {
    /// The text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Cursor position, as a byte offset into `text`.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected byte range, if any.
    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        (self.anchor != self.cursor).then(|| self.anchor.min(self.cursor)..self.anchor.max(self.cursor))
    }

    /// The selected text, if any.
    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|range| &self.text[range])
    }

    /// Replace the selection with `text` (control characters dropped), or insert it at the cursor.
    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.anchor = self.cursor;
    }

    /// Delete the selection, or the character before the cursor.
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some((start, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
            self.anchor = start;
        }
    }

    /// Delete the selection, or the character after the cursor.
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.text.replace_range(self.cursor..self.cursor + c.len_utf8(), "");
        }
    }

    /// Delete the selection, or the word before the cursor.
    pub fn delete_word_back(&mut self) {
        if self.delete_selection() {
            return;
        }
        let start = self.word_start(self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.anchor = start;
    }

    /// Delete the selection, or the word after the cursor.
    pub fn delete_word_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        let end = self.word_end(self.cursor);
        self.text.replace_range(self.cursor..end, "");
    }

    /// Move one character left (to the selection's start when collapsing one); `select` extends the selection.
    pub fn left(&mut self, select: bool) {
        let to = match self.selection() {
            Some(range) if !select => range.start,
            _ => self.text[..self.cursor].char_indices().next_back().map_or(self.cursor, |(start, _)| start),
        };
        self.move_to(to, select);
    }

    /// Move one character right (to the selection's end when collapsing one); `select` extends the selection.
    pub fn right(&mut self, select: bool) {
        let to = match self.selection() {
            Some(range) if !select => range.end,
            _ => self.cursor + self.text[self.cursor..].chars().next().map_or(0, char::len_utf8),
        };
        self.move_to(to, select);
    }

    /// Move to the start of the previous word; `select` extends the selection.
    pub fn word_left(&mut self, select: bool) {
        self.move_to(self.word_start(self.cursor), select);
    }

    /// Move to the end of the next word; `select` extends the selection.
    pub fn word_right(&mut self, select: bool) {
        self.move_to(self.word_end(self.cursor), select);
    }

    /// Move to the start; `select` extends the selection.
    pub fn home(&mut self, select: bool) {
        self.move_to(0, select);
    }

    /// Move to the end; `select` extends the selection.
    pub fn end(&mut self, select: bool) {
        self.move_to(self.text.len(), select);
    }

    /// Select everything.
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    /// Select the run of same-kind characters (letters and digits, spaces, or punctuation) at byte `offset`.
    pub fn select_word_at(&mut self, offset: usize) {
        let offset = offset.min(self.text.len());
        let Some(c) = self.text[offset..].chars().next().or_else(|| self.text[..offset].chars().next_back()) else {
            return;
        };
        let kind = class(c);
        let pivot = if offset < self.text.len() { offset } else { offset - c.len_utf8() };
        let before =
            self.text[..pivot].chars().rev().take_while(|&c| class(c) == kind).map(char::len_utf8).sum::<usize>();
        let after = self.text[pivot..].chars().take_while(|&c| class(c) == kind).map(char::len_utf8).sum::<usize>();
        self.anchor = pivot - before;
        self.cursor = pivot + after;
    }

    /// A left press at byte `offset`: one click places the cursor, two select the word, three or more select all.
    pub fn press(&mut self, offset: usize, clicks: usize) {
        match clicks {
            0 | 1 => self.move_to(offset.min(self.text.len()), false),
            2 => self.select_word_at(offset),
            _ => self.select_all(),
        }
    }

    /// Empty the field.
    pub fn clear(&mut self) {
        *self = TextField::default();
    }

    fn move_to(&mut self, to: usize, select: bool) {
        self.cursor = to;
        if !select {
            self.anchor = to;
        }
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            return false;
        };
        self.cursor = range.start;
        self.anchor = range.start;
        self.text.replace_range(range, "");
        true
    }

    /// Start of the word before `from`: skips spaces and punctuation, then the word.
    fn word_start(&self, from: usize) -> usize {
        let head = &self.text[..from];
        let trimmed = head.trim_end_matches(|c| class(c) != Class::Word).len();
        head[..trimmed].trim_end_matches(|c| class(c) == Class::Word).len()
    }

    /// End of the word after `from`: skips spaces and punctuation, then the word.
    fn word_end(&self, from: usize) -> usize {
        let tail = &self.text[from..];
        let skipped = tail.len() - tail.trim_start_matches(|c| class(c) != Class::Word).len();
        let rest = &tail[skipped..];
        from + skipped + rest.len() - rest.trim_start_matches(|c| class(c) == Class::Word).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_respects_multibyte_characters() {
        let mut field = TextField::default();
        field.insert("שלום");
        field.left(false);
        field.insert("x");
        assert_eq!(field.text(), "שלוxם");
        field.backspace();
        field.backspace();
        assert_eq!(field.text(), "שלם");
        field.delete();
        assert_eq!(field.text(), "של");
        field.home(false);
        field.right(false);
        field.insert("a\u{1b}b\n");
        assert_eq!((field.text(), field.cursor()), ("שabל", "שab".len()));
    }

    #[test]
    fn edges_are_harmless_and_clear_resets() {
        let mut field = TextField::default();
        field.backspace();
        field.delete();
        field.left(false);
        field.right(false);
        field.insert("ab");
        field.end(false);
        field.delete();
        field.right(false);
        assert_eq!((field.text(), field.cursor()), ("ab", 2));
        field.clear();
        assert_eq!(field, TextField::default());
    }

    fn field(text: &str, cursor: usize) -> TextField {
        let mut field = TextField::default();
        field.insert(text);
        field.cursor = cursor;
        field.anchor = cursor;
        field
    }

    #[test]
    fn word_deletion_skips_separators_then_the_word() {
        let mut f = field("open /home/ido  foo", 19);
        f.delete_word_back();
        assert_eq!((f.text(), f.cursor()), ("open /home/ido  ", 16));
        f.delete_word_back();
        assert_eq!((f.text(), f.cursor()), ("open /home/", 11));
        f.delete_word_back();
        assert_eq!((f.text(), f.cursor()), ("open /", 6));
        f.delete_word_back();
        assert_eq!((f.text(), f.cursor()), ("", 0));
        let mut f = field("open /home/ido  foo", 4);
        f.delete_word_forward();
        assert_eq!((f.text(), f.cursor()), ("open/ido  foo", 4));
    }

    #[test]
    fn word_movement_and_selection() {
        let mut f = field("שלום עולם x", 0);
        f.word_right(false);
        assert_eq!(f.cursor(), "שלום".len());
        f.word_right(true);
        assert_eq!(f.selected_text(), Some(" עולם"));
        f.word_left(true);
        assert_eq!(f.selected_text(), Some(" "));
        f.end(false);
        f.word_left(false);
        assert_eq!(f.cursor(), "שלום עולם ".len());
        f.left(true);
        assert_eq!(f.selected_text(), Some(" "));
        f.left(false);
        assert_eq!((f.selection(), f.cursor()), (None, "שלום עולם".len()));
    }

    #[test]
    fn typing_and_deleting_replace_the_selection() {
        let mut f = field("hello world", 0);
        f.select_all();
        f.insert("x");
        assert_eq!((f.text(), f.cursor()), ("x", 1));
        let mut f = field("hello world", 0);
        f.word_right(true);
        f.backspace();
        assert_eq!((f.text(), f.cursor()), (" world", 0));
        f.word_right(true);
        f.delete();
        assert_eq!(f.text(), "");
        let mut f = field("ab cd", 0);
        f.word_right(true);
        f.delete_word_back();
        assert_eq!(f.text(), " cd");
    }

    #[test]
    fn select_word_at_picks_the_run_under_the_offset() {
        let mut f = field("foo  bar/baz", 0);
        f.select_word_at(1);
        assert_eq!(f.selected_text(), Some("foo"));
        f.select_word_at(4);
        assert_eq!(f.selected_text(), Some("  "));
        f.select_word_at(7);
        assert_eq!(f.selected_text(), Some("bar"));
        f.select_word_at(8);
        assert_eq!(f.selected_text(), Some("/"));
        f.select_word_at(99);
        assert_eq!(f.selected_text(), Some("baz"));
        let mut empty = TextField::default();
        empty.select_word_at(0);
        assert_eq!(empty.selection(), None);
    }

    #[test]
    fn presses_place_the_cursor_then_select_word_then_all() {
        let mut f = field("foo bar", 0);
        f.press(5, 1);
        assert_eq!((f.cursor(), f.selection()), (5, None));
        f.press(5, 2);
        assert_eq!(f.selected_text(), Some("bar"));
        f.press(5, 3);
        assert_eq!(f.selected_text(), Some("foo bar"));
        f.press(99, 1);
        assert_eq!((f.cursor(), f.selection()), (7, None));
    }
}
