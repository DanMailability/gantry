//! Files the interface draws from, embedded in the binary.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// The dev container icon (a box), drawn by `theme::container_icon`.
pub(crate) const CONTAINER_ICON: &str = "icons/container.svg";

/// The split right button's icon (a window with its right half filled).
pub(crate) const SPLIT_RIGHT_ICON: &str = "icons/split-right.svg";

/// The split down button's icon (a window with its bottom half filled).
pub(crate) const SPLIT_DOWN_ICON: &str = "icons/split-down.svg";

/// The close pane button's icon (an X).
pub(crate) const CLOSE_ICON: &str = "icons/close.svg";

/// The last prompt card's icon (a chat bubble), drawn by `workspace_view::prompt_card`.
pub(crate) const PROMPT_ICON: &str = "icons/prompt.svg";

/// The pin on a row marked unread (a bookmark), drawn by `theme::pin_icon`.
pub(crate) const PIN_ICON: &str = "icons/pin.svg";

/// The app icon in color, drawn next to the name in the title bar (a picture, not a tinted icon like the others).
pub(crate) const LOGO: &str = "icons/gantry.png";

/// Gantry's embedded assets.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            CONTAINER_ICON => Some(include_bytes!("../assets/icons/container.svg")),
            SPLIT_RIGHT_ICON => Some(include_bytes!("../assets/icons/split-right.svg")),
            SPLIT_DOWN_ICON => Some(include_bytes!("../assets/icons/split-down.svg")),
            CLOSE_ICON => Some(include_bytes!("../assets/icons/close.svg")),
            PROMPT_ICON => Some(include_bytes!("../assets/icons/prompt.svg")),
            PIN_ICON => Some(include_bytes!("../assets/icons/pin.svg")),
            LOGO => Some(include_bytes!("../assets/icons/gantry.png")),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![
            CONTAINER_ICON.into(),
            SPLIT_RIGHT_ICON.into(),
            SPLIT_DOWN_ICON.into(),
            CLOSE_ICON.into(),
            PROMPT_ICON.into(),
            PIN_ICON.into(),
            LOGO.into(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icons_are_embedded_and_nothing_else_is() {
        for icon in [CONTAINER_ICON, SPLIT_RIGHT_ICON, SPLIT_DOWN_ICON, CLOSE_ICON, PROMPT_ICON, PIN_ICON] {
            let bytes = Assets.load(icon).unwrap().expect("embedded");
            assert!(std::str::from_utf8(&bytes).unwrap().starts_with("<svg"), "{icon}");
        }
        assert!(Assets.load(LOGO).unwrap().expect("embedded").starts_with(b"\x89PNG"));
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
