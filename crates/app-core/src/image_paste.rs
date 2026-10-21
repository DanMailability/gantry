//! When Ctrl+V sends the clipboard's image to the pane's daemon instead of reaching the program.

/// Largest image sent, in bytes.
pub const MAX_BYTES: usize = 20 * 1024 * 1024;

/// Whether this keystroke is the one Claude itself uses to paste an image: Ctrl+V (Alt+V on Windows).
pub fn is_paste_key(key: &str, ctrl: bool, alt: bool, shift: bool, platform: bool) -> bool {
    if cfg!(windows) {
        key == "v" && alt && !ctrl && !shift && !platform
    } else {
        key == "v" && ctrl && !alt && !shift && !platform
    }
}

/// Whether a pane's image paste goes through its daemon: a Claude pane that is not plain local (on an SSH host or in
/// a dev container), whose daemon says it saves images.
pub fn eligible(claude: bool, remote: bool, container: bool, daemon_supports: bool) -> bool {
    claude && (remote || container) && daemon_supports
}

/// Whether an image of `len` bytes may be sent.
pub fn fits(len: usize) -> bool {
    len > 0 && len <= MAX_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_remote_or_container_claude_panes_with_a_capable_daemon_qualify() {
        assert!(!eligible(true, false, false, true), "a local Claude pane reads the clipboard itself");
        assert!(eligible(true, true, false, true));
        assert!(eligible(true, false, true, true));
        assert!(eligible(true, true, true, true));
        assert!(!eligible(false, true, false, true), "a shell pane");
        assert!(!eligible(false, false, true, true), "a shell pane in a container");
        assert!(!eligible(true, true, false, false), "an older daemon");
    }

    // The key differs by platform: Ctrl+V here, Alt+V on Windows.
    #[cfg(not(windows))]
    #[test]
    fn the_paste_key_is_a_bare_ctrl_v() {
        assert!(is_paste_key("v", true, false, false, false));
        assert!(!is_paste_key("v", true, false, true, false));
        assert!(!is_paste_key("v", true, true, false, false));
        assert!(!is_paste_key("v", false, false, false, false));
        assert!(!is_paste_key("c", true, false, false, false));
    }

    #[test]
    fn size_must_be_within_the_cap() {
        assert!(!fits(0));
        assert!(fits(1));
        assert!(fits(MAX_BYTES));
        assert!(!fits(MAX_BYTES + 1));
    }
}
