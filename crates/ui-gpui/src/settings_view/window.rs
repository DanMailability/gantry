//! The Window section: what Gantry restores on start and the sidebar at start.

use app_core::settings::{Settings, SidebarAtStart};
use gpui::{Context, Div, Window, div};

use super::*;
use crate::theme::Theme;

/// The "Tabs at start" choices: whether `restoreLayout` is on, its label and what it does.
const RESTORE: [(bool, &str, &str); 2] = [
    (true, "Restore", "Reattach running sessions; finished ones offer Continue."),
    (
        false,
        "One new tab",
        "Running sessions stay as tabs; finished ones are not reopened. The saved layout is kept, so you can switch back.",
    ),
];

const SIDEBAR: [(SidebarAtStart, &str, &str); 3] = [
    (SidebarAtStart::Remember, "Remember", "As you left it. The sidebar shortcut toggles it any time."),
    (SidebarAtStart::Shown, "Shown", "Always open when Gantry starts."),
    (SidebarAtStart::Hidden, "Hidden", "Always closed when Gantry starts."),
];

impl SettingsView {
    pub(super) fn window_page(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let restore = RESTORE.iter().position(|(restore, _, _)| *restore == settings.window.restore_layout).unwrap_or(0);
        let restore_labels = RESTORE.map(|(_, label, _)| label);
        let restore_bar = self.segmented(
            "restore-layout",
            &restore_labels,
            restore,
            |index| {
                let restore = RESTORE[index].0;
                change(move |settings| settings.window.restore_layout = restore)
            },
            window,
            cx,
        );
        let restore_reset = self.reset_slot("reset-restore-layout", 0, settings, |s| s.window.restore_layout, |s, v| s.window.restore_layout = v, window, cx);

        let sidebar = SIDEBAR.iter().position(|(choice, _, _)| *choice == settings.window.sidebar_at_start).unwrap_or(0);
        let sidebar_labels = SIDEBAR.map(|(_, label, _)| label);
        let sidebar_bar = self.segmented(
            "sidebar-at-start",
            &sidebar_labels,
            sidebar,
            |index| {
                let choice = SIDEBAR[index].0;
                change(move |settings| settings.window.sidebar_at_start = choice)
            },
            window,
            cx,
        );
        let sidebar_reset = self.reset_slot("reset-sidebar-at-start", 0, settings, |s| s.window.sidebar_at_start, |s, v| s.window.sidebar_at_start = v, window, cx);

        div().flex().flex_col().gap(theme.ui(26.)).child(group(
            theme,
            "Start",
            None,
            vec![
                row(theme, "Tabs at start", RESTORE[restore].2, restore_bar, restore_reset),
                row(theme, "Sidebar at start", SIDEBAR[sidebar].2, sidebar_bar, sidebar_reset),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restore_copy_says_what_the_spec_says() {
        assert_eq!(RESTORE[1].1, "One new tab");
        assert!(RESTORE[1].2.starts_with("Running sessions stay as tabs; finished ones are not reopened."));
    }

    #[test]
    fn every_choice_has_its_own_description() {
        assert_eq!(SIDEBAR.map(|(_, label, _)| label), ["Remember", "Shown", "Hidden"]);
        assert_eq!(SIDEBAR[2].2, "Always closed when Gantry starts.");
        assert_eq!(RESTORE[0].1, "Restore");
    }
}
