//! The Settings tab's page: a header, a left menu of sections and the section's body.

use gpui::{
    App, Context, FocusHandle, Focusable, FontWeight, IntoElement, Render, Window, div, prelude::*, px, rgb,
};

use crate::settings_store::SettingsStore;
use crate::theme::{RADIUS_SMALL, theme};

/// A group of settings, chosen in the left menu.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    /// Theme, status colours and fonts.
    Appearance,
    /// What raises an alert, and how.
    Alerts,
}

impl Section {
    const ALL: [Section; 2] = [Section::Appearance, Section::Alerts];

    fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Alerts => "Alerts",
        }
    }
}

/// The Settings page, shown in the pane area while the Settings tab is active.
pub(crate) struct SettingsView {
    section: Section,
    focus_handle: FocusHandle,
}

impl SettingsView {
    /// A page on the Appearance section.
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        SettingsView { section: Section::Appearance, focus_handle: cx.focus_handle() }
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let path = cx.global::<SettingsStore>().path().display().to_string();
        let menu = div().flex_none().w(theme.ui(180.)).flex().flex_col().gap(theme.ui(4.)).children(Section::ALL.map(|section| {
            let active = section == self.section;
            div()
                .id(section.label())
                .h(theme.ui(32.))
                .px_3()
                .flex()
                .items_center()
                .rounded(px(RADIUS_SMALL))
                .cursor_pointer()
                .text_color(rgb(if active { theme.tokens.text } else { theme.tokens.muted }))
                .when(active, |item| item.bg(rgb(theme.tokens.raised)).font_weight(FontWeight::SEMIBOLD))
                .when(!active, |item| item.hover(|style| style.bg(rgb(theme.tokens.raised))))
                .child(section.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.section = section;
                    cx.notify();
                }))
        }));
        div()
            .id("settings-page")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .gap(theme.ui(16.))
            .p(theme.ui(24.))
            .text_color(rgb(theme.tokens.text))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(theme.ui(2.))
                    .child(div().text_xl().font_weight(FontWeight::BOLD).child("Settings"))
                    .child(div().text_sm().text_color(rgb(theme.tokens.muted)).child("Changes apply right away."))
                    .child(div().text_xs().text_color(rgb(theme.tokens.muted)).child(path)),
            )
            .child(div().flex_1().min_h_0().flex().flex_row().gap(theme.ui(24.)).child(menu).child(div().flex_1().min_w_0().h_full()))
    }
}
