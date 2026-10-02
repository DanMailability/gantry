//! The Terminal section: font, scrollback, text direction and shell.

use app_core::settings::Settings;
use gpui::{Context, Div, Window, div};

use super::*;
use crate::theme::Theme;

impl SettingsView {
    pub(super) fn terminal(&mut self, _theme: &Theme, _settings: &Settings, _window: &Window, _cx: &mut Context<Self>) -> Div {
        div()
    }
}
