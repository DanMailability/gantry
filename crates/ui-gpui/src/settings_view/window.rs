//! The Window section: what Gantry restores on start, the sidebar at start, scrollback and right-to-left display.

use app_core::settings::{Settings, SidebarAtStart};
use gpui::{Context, Div, FontWeight, Window, div, px, rgb};

use super::*;
use crate::theme::{RADIUS_SMALL, Theme};

/// Scrollback lines are changed in thousands.
const SCROLLBACK: Stepped = Stepped {
    name: "scrollback",
    get: |s| s.window.scrollback as f32,
    set: |s, value| s.window.scrollback = value as u32,
    delta: 1000.0,
    min: 1000.0,
    max: 100_000.0,
    unit: "lines",
};

/// The "When Gantry starts" choices: whether `restoreLayout` is on, its title and what it does.
const RESTORE: [(bool, &str, &str); 2] = [
    (true, "Restore tabs and panes", "Reattach running sessions; finished ones offer Continue."),
    (
        false,
        "Start with one new tab",
        "Finished sessions are not reopened; running ones stay as tabs. The saved layout is kept, so you can switch back.",
    ),
];

const SIDEBAR: [(SidebarAtStart, &str, &str); 3] = [
    (SidebarAtStart::Remember, "Remember", "Remember: as you left it (the sidebar shortcut toggles it any time)."),
    (SidebarAtStart::Shown, "Always shown", "Always shown: the sidebar is open at start, whatever it was."),
    (SidebarAtStart::Hidden, "Always hidden", "Always hidden: the sidebar is closed at start, whatever it was."),
];

impl SettingsView {
    pub(super) fn window_page(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let window_settings = settings.window;
        let restore: Vec<_> = RESTORE.iter().enumerate().map(|(index, (restore, title, description))| {
            let chosen = window_settings.restore_layout == *restore;
            let restore = *restore;
            self.pressable("restore-layout", index, None, window, cx, change(move |settings| settings.window.restore_layout = restore))
                .py(theme.ui(6.))
                .flex()
                .items_start()
                .gap(theme.ui(12.))
                .child(
                    div()
                        .mt(theme.ui(3.))
                        .flex_none()
                        .size(theme.ui(14.))
                        .rounded_full()
                        .border_2()
                        .border_color(rgb(if chosen { theme.tokens.accent } else { theme.tokens.muted }))
                        .when(chosen, |dot| dot.bg(rgb(theme.tokens.accent))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(*title))
                        .child(muted(theme, *description)),
                )
        }).collect();

        let segments: Vec<_> = SIDEBAR
            .iter()
            .enumerate()
            .map(|(index, (choice, label, _))| {
                let chosen = window_settings.sidebar_at_start == *choice;
                let choice = *choice;
                self.pressable("sidebar-at-start", index, None, window, cx, change(move |settings| settings.window.sidebar_at_start = choice))
                    .h(theme.ui(32.))
                    .px(theme.ui(16.))
                    .flex()
                    .items_center()
                    .rounded(px(RADIUS_SMALL))
                    .text_sm()
                    .whitespace_nowrap()
                    .when(chosen, |segment| segment.bg(rgb(theme.tokens.accent)).text_color(rgb(theme.tokens.on_accent)).font_weight(FontWeight::SEMIBOLD))
                    .when(!chosen, |segment| segment.text_color(rgb(theme.tokens.text)))
                    .child(*label)
            })
            .collect();
        let hint = SIDEBAR.iter().find(|(choice, _, _)| *choice == window_settings.sidebar_at_start).map_or("", |(_, _, hint)| *hint);

        let rtl = window_settings.right_to_left;
        let rtl_switch = self.switch("right-to-left", 0, rtl, window, cx, change(move |settings| settings.window.right_to_left = !rtl));

        div()
            .max_w(theme.ui(900.))
            .flex()
            .flex_col()
            .gap(theme.ui(28.))
            .child(div().flex().flex_col().gap(theme.ui(10.)).child(heading(theme, "When Gantry starts")).children(restore))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.ui(10.))
                    .child(heading(theme, "Sidebar at start"))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .self_start()
                            .max_w_full()
                            .p(px(3.))
                            .gap(px(3.))
                            .rounded(px(RADIUS_SMALL + 2.))
                            .bg(rgb(theme.tokens.raised))
                            .border_1()
                            .border_color(rgb(theme.tokens.line))
                            .children(segments),
                    )
                    .child(muted(theme, hint)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.ui(14.))
                    .child(heading(theme, "Terminal"))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x(theme.ui(12.))
                            .gap_y(theme.ui(4.))
                            .child(div().flex_none().w(theme.ui(170.)).child("Scrollback lines"))
                            .child(self.stepper(&SCROLLBACK, settings, window, cx))
                            .child(muted(theme, "New panes; open panes keep theirs until restarted.").flex_1().min_w(theme.ui(200.))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(theme.ui(16.))
                            .py(theme.ui(8.))
                            .child(rtl_switch)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Right-to-left display"))
                                    .child(muted(theme, "Shows Hebrew and Arabic in reading order. Off: characters stay in the order the program wrote them.")),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollback_steps_by_a_thousand_within_its_range() {
        let step = |value: f32, sign: f32| step(value, sign * SCROLLBACK.delta, SCROLLBACK.min, SCROLLBACK.max);
        assert_eq!(step(10_000., 1.), 11_000.);
        assert_eq!(step(1_000., -1.), 1_000.);
        assert_eq!(step(100_000., 1.), 100_000.);
        let mut settings = Settings::default();
        (SCROLLBACK.set)(&mut settings, 25_000.);
        assert_eq!((SCROLLBACK.get)(&settings), 25_000.);
        assert_eq!(settings.window.scrollback, 25_000);
    }

    #[test]
    fn the_restore_copy_says_what_the_spec_says() {
        assert_eq!(RESTORE[1].1, "Start with one new tab");
        assert!(RESTORE[1].2.starts_with("Finished sessions are not reopened; running ones stay as tabs."));
    }
}
