//! Files the interface draws from, embedded in the binary.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// The dev container icon (a box), drawn by `theme::container_icon`.
pub(crate) const CONTAINER_ICON: &str = "icons/container.svg";

/// Gantry's embedded assets.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok((path == CONTAINER_ICON).then_some(Cow::Borrowed(include_bytes!("../assets/icons/container.svg").as_slice())))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![CONTAINER_ICON.into()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_container_icon_is_embedded_and_nothing_else_is() {
        let bytes = Assets.load(CONTAINER_ICON).unwrap().expect("embedded");
        assert!(std::str::from_utf8(&bytes).unwrap().starts_with("<svg"));
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}
