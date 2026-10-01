//! A single-line text field's contents and cursor; the GUI draws it and forwards editing keys.

/// Text plus a cursor (a byte offset on a character boundary).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextField {
    text: String,
    cursor: usize,
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

    /// Insert `text` (control characters dropped) at the cursor.
    pub fn insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if let Some((start, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
    }

    /// Delete the character after the cursor.
    pub fn delete(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.text.replace_range(self.cursor..self.cursor + c.len_utf8(), "");
        }
    }

    /// Move one character left.
    pub fn left(&mut self) {
        if let Some((start, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = start;
        }
    }

    /// Move one character right.
    pub fn right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    /// Move to the start.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move to the end.
    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Empty the field.
    pub fn clear(&mut self) {
        *self = TextField::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_respects_multibyte_characters() {
        let mut field = TextField::default();
        field.insert("שלום");
        field.left();
        field.insert("x");
        assert_eq!(field.text(), "שלוxם");
        field.backspace();
        field.backspace();
        assert_eq!(field.text(), "שלם");
        field.delete();
        assert_eq!(field.text(), "של");
        field.home();
        field.right();
        field.insert("a\u{1b}b\n");
        assert_eq!((field.text(), field.cursor()), ("שabל", "שab".len()));
    }

    #[test]
    fn edges_are_harmless_and_clear_resets() {
        let mut field = TextField::default();
        field.backspace();
        field.delete();
        field.left();
        field.right();
        field.insert("ab");
        field.end();
        field.delete();
        field.right();
        assert_eq!((field.text(), field.cursor()), ("ab", 2));
        field.clear();
        assert_eq!(field, TextField::default());
    }
}
