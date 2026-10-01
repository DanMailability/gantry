//! Drawing a `TextField` and applying editing keys to it.

use app_core::text_field::TextField;
use gpui::{Div, Keystroke, div, prelude::*, px, rgb};

use crate::workspace_view::{ACCENT, MUTED};

/// Apply an editing key (typing, Backspace, Delete, arrows, Home/End) to `field`. False for anything
/// else — chords with Ctrl/Alt/Cmd are never text.
pub(crate) fn edit(field: &mut TextField, keystroke: &Keystroke) -> bool {
    let modifiers = &keystroke.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
        return false;
    }
    match keystroke.key.as_str() {
        "backspace" => field.backspace(),
        "delete" => field.delete(),
        "left" => field.left(),
        "right" => field.right(),
        "home" => field.home(),
        "end" => field.end(),
        _ => match &keystroke.key_char {
            Some(text) if !text.is_empty() && !text.chars().any(char::is_control) => field.insert(text),
            _ => return false,
        },
    }
    true
}

/// The field's text with a caret at the cursor when `focused`; `placeholder` while empty.
pub(crate) fn render(field: &TextField, placeholder: &'static str, focused: bool) -> Div {
    let caret = || div().flex_none().w(px(1.)).h(px(14.)).bg(rgb(ACCENT));
    let row = div().flex().flex_row().items_center().min_w_0().overflow_hidden().whitespace_nowrap();
    if field.text().is_empty() {
        return row.when(focused, |row| row.child(caret())).child(div().text_color(rgb(MUTED)).child(placeholder));
    }
    let (before, after) = field.text().split_at(field.cursor());
    row.child(before.to_string()).when(focused, |row| row.child(caret())).child(after.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn key(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke { modifiers, key: key.into(), key_char: key_char.map(Into::into) }
    }

    #[test]
    fn typing_and_editing_keys_change_the_field() {
        let mut field = TextField::default();
        let plain = Modifiers::default();
        let shift = Modifiers { shift: true, ..Default::default() };
        for keystroke in [key("a", Some("a"), plain), key("b", Some("B"), shift), key("ש", Some("ש"), plain), key("space", Some(" "), plain)] {
            assert!(edit(&mut field, &keystroke));
        }
        assert_eq!(field.text(), "aBש ");
        for name in ["backspace", "left", "left", "delete", "home", "end"] {
            assert!(edit(&mut field, &key(name, None, plain)), "{name}");
        }
        assert_eq!(field.text(), "aש");
    }

    #[test]
    fn chords_and_non_editing_keys_are_left_alone() {
        let mut field = TextField::default();
        let ctrl = Modifiers { control: true, ..Default::default() };
        let alt = Modifiers { alt: true, ..Default::default() };
        assert!(!edit(&mut field, &key("a", Some("a"), ctrl)));
        assert!(!edit(&mut field, &key("1", Some("1"), alt)));
        for name in ["enter", "escape", "up", "down", "tab"] {
            assert!(!edit(&mut field, &key(name, None, Modifiers::default())), "{name}");
        }
        assert_eq!(field.text(), "");
    }
}
