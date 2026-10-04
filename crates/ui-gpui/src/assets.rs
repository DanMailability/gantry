//! Files the interface draws from, embedded in the binary.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// The dev container icon (a box), drawn by `theme::container_icon`.
pub(crate) const CONTAINER_ICON: &str = "icons/container.svg";

/// The split right button's icon (a window with its right half filled).
pub(crate) const SPLIT_RIGHT_ICON: &str = "icons/split-right.svg";

/// The split down button's icon (a window with its bottom half filled).
pub(crate) const SPLIT_DOWN_ICON: &str = "icons/split-down.svg";

/// The last prompt card's icon (a chat bubble), drawn by `workspace_view::prompt_card`.
pub(crate) const PROMPT_ICON: &str = "icons/prompt.svg";

/// Gantry's embedded assets.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            CONTAINER_ICON => Some(include_bytes!("../assets/icons/container.svg")),
            SPLIT_RIGHT_ICON => Some(include_bytes!("../assets/icons/split-right.svg")),
            SPLIT_DOWN_ICON => Some(include_bytes!("../assets/icons/split-down.svg")),
            PROMPT_ICON => Some(include_bytes!("../assets/icons/prompt.svg")),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![CONTAINER_ICON.into(), SPLIT_RIGHT_ICON.into(), SPLIT_DOWN_ICON.into(), PROMPT_ICON.into()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icons_are_embedded_and_nothing_else_is() {
        for icon in [CONTAINER_ICON, SPLIT_RIGHT_ICON, SPLIT_DOWN_ICON, PROMPT_ICON] {
            let bytes = Assets.load(icon).unwrap().expect("embedded");
            assert!(std::str::from_utf8(&bytes).unwrap().starts_with("<svg"), "{icon}");
        }
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
