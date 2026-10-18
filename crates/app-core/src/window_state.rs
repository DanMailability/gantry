//! The window's size and maximized state, kept beside the layout file so the next start opens the same way.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Smallest width and height read back, so a bad file cannot open an unusable window.
const MIN_SIDE: f32 = 400.0;

/// How the window was when it closed. `width`/`height` are the restore size (the windowed size, also while maximized).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// Width in logical pixels.
    pub width: f32,
    /// Height in logical pixels.
    pub height: f32,
    /// Whether the window was maximized.
    pub maximized: bool,
}

/// Where the state is kept: beside the layout file.
pub fn file_beside(layout_file: &Path) -> PathBuf {
    layout_file.with_file_name("window.json")
}

/// Read the saved state. `None` if there is none, it is unreadable or its size is not usable.
pub fn load(path: &Path) -> Option<WindowState> {
    let state: WindowState = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    (state.width.is_finite() && state.height.is_finite() && state.width >= MIN_SIDE && state.height >= MIN_SIDE)
        .then_some(state)
}

/// Write the state, replacing the file atomically (write a temp file, then rename).
pub fn save(path: &Path, state: &WindowState) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(state).map_err(io::Error::other)?)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.json");
        let state = WindowState { width: 1200.0, height: 800.0, maximized: true };
        save(&path, &state).unwrap();
        assert_eq!(load(&path), Some(state));
    }

    #[test]
    fn missing_or_unusable_files_give_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.json");
        assert_eq!(load(&path), None);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load(&path), None);
        std::fs::write(&path, r#"{"width":10,"height":800,"maximized":false}"#).unwrap();
        assert_eq!(load(&path), None);
    }
}
