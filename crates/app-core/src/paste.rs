//! Prepare clipboard text for sending to a terminal program.

/// Encode pasted text: line endings become `CR`, `ESC` is removed (so pasted text can never
/// inject control sequences or end bracketed paste early), and bracketed-paste markers are
/// added when the program enabled that mode.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let cleaned: String = text.replace("\r\n", "\r").replace('\n', "\r").chars().filter(|&c| c != '\x1b').collect();
    if bracketed { [b"\x1b[200~".as_slice(), cleaned.as_bytes(), b"\x1b[201~"].concat() } else { cleaned.into_bytes() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newlines_become_carriage_returns() {
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
    }

    #[test]
    fn bracketed_paste_wraps_text() {
        assert_eq!(encode_paste("hi", true), b"\x1b[200~hi\x1b[201~".to_vec());
    }

    #[test]
    fn escape_cannot_break_out_of_bracketed_paste() {
        let pasted = encode_paste("x\x1b[201~rm -rf ~\n", true);
        assert_eq!(pasted, b"\x1b[200~x[201~rm -rf ~\r\x1b[201~".to_vec());
    }
}
