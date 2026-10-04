//! Files the interface draws from, embedded in the binary.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// The dev container icon (a box), drawn by `theme::container_icon`.
pub(crate) const CONTAINER_ICON: &str = "icons/container.svg";

/// The last prompt card's icon (a chat bubble), drawn by `workspace_view::prompt_card`.
pub(crate) const PROMPT_ICON: &str = "icons/prompt.svg";

/// Gantry's embedded assets.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            CONTAINER_ICON => Some(Cow::Borrowed(include_bytes!("../assets/icons/container.svg").as_slice())),
            PROMPT_ICON => Some(Cow::Borrowed(include_bytes!("../assets/icons/prompt.svg").as_slice())),
            _ => None,
        })
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![CONTAINER_ICON.into(), PROMPT_ICON.into()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_icons_are_embedded_and_nothing_else_is() {
        for path in [CONTAINER_ICON, PROMPT_ICON] {
            let bytes = Assets.load(path).unwrap().expect("embedded");
            assert!(std::str::from_utf8(&bytes).unwrap().starts_with("<svg"), "{path}");
        }
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
