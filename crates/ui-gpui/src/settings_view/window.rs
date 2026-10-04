//! The Window section: what Gantry restores on start, the sidebar at start and the last prompt card.

use app_core::settings::{Settings, SidebarAtStart};
use app_core::theme_colors::{ProjectColor, mix};
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

const LAST_PROMPT_SIZE: Stepped = Stepped {
    name: "last-prompt-size",
    get: |s| s.window.last_prompt_size,
    set: |s, value| s.window.last_prompt_size = value,
    delta: 1.0,
    min: 11.0,
    max: 32.0,
    unit: "",
};

impl SettingsView {
    pub(super) fn window_page(
        &mut self,
        theme: &Theme,
        settings: &Settings,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let restore =
            RESTORE.iter().position(|(restore, _, _)| *restore == settings.window.restore_layout).unwrap_or(0);
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
        let restore_reset = self.reset_slot(
            "reset-restore-layout",
            0,
            settings,
            |s| s.window.restore_layout,
            |s, v| s.window.restore_layout = v,
            window,
            cx,
        );

        let resume = settings.window.resume_sessions;
        let resume_switch = self.switch(
            "resume-sessions",
            0,
            resume,
            window,
            cx,
            change(move |settings| settings.window.resume_sessions = !resume),
        );
        let resume_reset = self.reset_slot(
            "reset-resume-sessions",
            0,
            settings,
            |s| s.window.resume_sessions,
            |s, v| s.window.resume_sessions = v,
            window,
            cx,
        );

        let sidebar =
            SIDEBAR.iter().position(|(choice, _, _)| *choice == settings.window.sidebar_at_start).unwrap_or(0);
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
        let sidebar_reset = self.reset_slot(
            "reset-sidebar-at-start",
            0,
            settings,
            |s| s.window.sidebar_at_start,
            |s, v| s.window.sidebar_at_start = v,
            window,
            cx,
        );

        let shown = settings.window.last_prompt;
        let prompt_switch = self.switch(
            "last-prompt",
            0,
            shown,
            window,
            cx,
            change(move |settings| settings.window.last_prompt = !shown),
        );
        let prompt_reset = self.reset_slot(
            "reset-last-prompt",
            0,
            settings,
            |s| s.window.last_prompt,
            |s, v| s.window.last_prompt = v,
            window,
            cx,
        );

        let size_control = self.stepper(&LAST_PROMPT_SIZE, settings, window, cx);
        let size_reset = self.reset_slot(
            "reset-last-prompt-size",
            0,
            settings,
            |s| s.window.last_prompt_size,
            |s, v| s.window.last_prompt_size = v,
            window,
            cx,
        );
        let swatches = [None]
            .into_iter()
            .chain(ProjectColor::ALL.map(|color| Some(color.value(theme.preset))))
            .enumerate()
            .map(|(index, choice)| {
                let shown = mix(choice.unwrap_or(theme.tokens.accent), theme.tokens.surface, 0.5);
                let pick = change(move |settings| settings.window.last_prompt_color = choice);
                self.pressable(
                    "last-prompt-swatch",
                    index,
                    (settings.window.last_prompt_color == choice).then_some(theme.tokens.text),
                    window,
                    cx,
                    pick,
                )
                .size(theme.ui(28.))
                .rounded(px(RADIUS_SMALL))
                .bg(rgb(shown))
                .border_2()
            })
            .collect::<Vec<_>>();
        let swatches = div().flex().gap(theme.ui(8.)).children(swatches);
        let color_reset = self.reset_slot(
            "reset-last-prompt-color",
            0,
            settings,
            |s| s.window.last_prompt_color,
            |s, v| s.window.last_prompt_color = v,
            window,
            cx,
        );

        div()
            .flex()
            .flex_col()
            .gap(theme.ui(26.))
            .child(group(
                theme,
                "Start",
                None,
                vec![
                    row(theme, "Tabs at start", RESTORE[restore].2, restore_bar, restore_reset),
                    row(
                        theme,
                        "Resume sessions",
                        "Run finished sessions again on start (Claude with --resume) instead of waiting for Continue.",
                        resume_switch,
                        resume_reset,
                    ),
                    row(theme, "Sidebar at start", SIDEBAR[sidebar].2, sidebar_bar, sidebar_reset),
                ],
            ))
            .child(group(
                theme,
                "Panes",
                None,
                vec![
                    row(
                        theme,
                        "Last prompt",
                        "Show your last prompt in a card above each Claude pane.",
                        prompt_switch,
                        prompt_reset,
                    ),
                    row(
                        theme,
                        "Prompt text size",
                        "Two lines at most; the card grows with it.",
                        size_control,
                        size_reset,
                    ),
                    row(
                        theme,
                        "Prompt background",
                        "A tint of this colour. The first swatch is the theme's accent.",
                        swatches,
                        color_reset,
                    ),
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
