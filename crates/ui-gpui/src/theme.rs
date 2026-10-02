//! The "Mist" look: dark slate surfaces, a light-blue accent, pastel fills that say what a session needs, rounded
//! corners, and the interface font (Figtree, embedded; the terminal keeps its own font).

use std::borrow::Cow;

use app_core::session_tree::Tone;
use std::path::Path;

use app_core::settings::{ProjectColors, StatusFills};
use app_core::workspace::DirBadge;
use app_core::theme_colors::{Preset, ToneColors, Tokens, tone_colors};
use gpui::{App, Div, FontWeight, Global, ParentElement, Pixels, Styled, Svg, div, px, rgb, rgba, svg};

use crate::assets::CONTAINER_ICON;

/// The colours and sizes views draw with, a GPUI global replaced whenever the settings change.
#[derive(Clone)]
pub(crate) struct Theme {
    /// Which preset is drawn.
    pub preset: Preset,
    /// The preset's app colours.
    pub tokens: Tokens,
    /// The user's status fills that are readable on this preset.
    pub fills: StatusFills,
    /// `uiFontSize / 14`.
    pub ui_scale: f32,
    /// The `projectColors` setting.
    pub project_colors: ProjectColors,
}

impl Global for Theme {}

impl Theme {
    /// The colours of a session row, tab or pane header in `tone`.
    pub fn tone(&self, tone: Tone) -> ToneColors {
        tone_colors(self.preset, &self.fills, tone)
    }

    /// The colour of the project at `root` on this theme, if project colours are on.
    pub fn project(&self, root: &Path) -> Option<u32> {
        self.project_colors.color_of(root).map(|color| color.value(self.preset))
    }

    /// `size` scaled with the interface font size.
    pub fn ui(&self, size: f32) -> Pixels {
        px(size * self.ui_scale)
    }
}

/// A project's side line: 4 px wide, rounded, inset 4 px from the left edge and 6 px from the top and bottom of
/// the (relatively positioned) row, tab or header it is a child of.
pub(crate) fn side_line(color: u32, theme: &Theme) -> Div {
    div().absolute().left(theme.ui(4.)).top(theme.ui(6.)).bottom(theme.ui(6.)).w(theme.ui(4.)).rounded(px(2.)).bg(rgb(color))
}

/// The theme global.
pub(crate) fn theme(cx: &App) -> &Theme {
    cx.global::<Theme>()
}

/// Corner radii, in pixels: chips and small buttons, rows and tabs, panels and panes.
pub(crate) const RADIUS_SMALL: f32 = 8.0;
pub(crate) const RADIUS: f32 = 12.0;
pub(crate) const RADIUS_LARGE: f32 = 16.0;

/// Interface font family (embedded, see [`load_fonts`]).
pub(crate) const UI_FONT: &str = "Figtree";

const FONTS: [&[u8]; 5] = [
    include_bytes!("../assets/fonts/Figtree-Regular.ttf"),
    include_bytes!("../assets/fonts/Figtree-Italic.ttf"),
    include_bytes!("../assets/fonts/Figtree-Medium.ttf"),
    include_bytes!("../assets/fonts/Figtree-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Figtree-Bold.ttf"),
];

/// The dev container icon, in `ink` (slightly softened).
pub(crate) fn container_icon(ink: u32, theme: &Theme) -> Svg {
    svg().path(CONTAINER_ICON).flex_none().size(theme.ui(14.)).text_color(rgba((ink << 8) | 0xd9))
}

/// A config dir's badge: its letter on its colour. Shown only with two or more config dirs.
pub(crate) fn dir_badge(badge: &DirBadge, theme: &Theme) -> Div {
    div()
        .flex_none()
        .size(theme.ui(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(rgb(badge.color.value(theme.preset)))
        .text_color(rgb(theme.tokens.ground))
        .text_size(theme.ui(10.))
        .font_weight(FontWeight::BOLD)
        .child(badge.letter.clone())
}

/// Make the embedded interface font available.
pub(crate) fn load_fonts(cx: &App) {
    if let Err(error) = cx.text_system().add_fonts(embedded_fonts()) {
        tracing::warn!("interface font not loaded: {error:#}");
    }
}

fn embedded_fonts() -> Vec<Cow<'static, [u8]>> {
    FONTS.iter().map(|font| Cow::Borrowed(*font)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_core::theme_colors::{contrast, terminal_palette, tokens};

    const PRESETS: [Preset; 2] = [Preset::Mist, Preset::MistLight];
    const TONES: [Tone; 6] = [Tone::NeedsInput, Tone::Working, Tone::Done, Tone::Idle, Tone::Elsewhere, Tone::Closed];

    fn theme(preset: Preset) -> Theme {
        Theme { preset, tokens: tokens(preset), fills: StatusFills::default(), ui_scale: 1.0, project_colors: ProjectColors::default() }
    }

    #[test]
    fn row_text_is_readable_on_every_tone() {
        for preset in PRESETS {
            let theme = theme(preset);
            let t = theme.tokens;
            for tone in TONES {
                let colors = theme.tone(tone);
                let ratio = contrast(colors.ink, colors.fill.unwrap_or(t.surface));
                assert!(ratio >= 4.5, "{preset:?} {tone:?}: {ratio:.2}");
            }
            assert!(contrast(t.text, t.surface) >= 4.5 && contrast(t.muted, t.surface) >= 4.5);
            assert!(contrast(t.on_accent, t.accent) >= 4.5, "{preset:?}");
        }
    }

    #[test]
    fn states_that_ask_for_attention_stand_out_from_quiet_ones() {
        // On Mist the pastel fills are far lighter than the surface; quiet states stay close to it.
        let theme = theme(Preset::Mist);
        for tone in [Tone::NeedsInput, Tone::Working, Tone::Done] {
            assert!(contrast(theme.tone(tone).fill.unwrap(), theme.tokens.surface) >= 7.0, "{tone:?}");
        }
        assert!(contrast(theme.tone(Tone::Idle).fill.unwrap(), theme.tokens.surface) < 1.5);
    }

    #[test]
    fn pane_cards_match_the_terminal_background() {
        for preset in PRESETS {
            assert_eq!(app_core::snapshot::Rgb::hex(tokens(preset).terminal), terminal_palette(preset).background, "{preset:?}");
        }
    }

    #[test]
    fn project_colours_follow_the_preset_and_the_switch() {
        let root = Path::new("/r/app");
        let mut dark = theme(Preset::Mist);
        let light = Theme { preset: Preset::MistLight, ..dark.clone() };
        let color = app_core::theme_colors::ProjectColor::auto(root);
        assert_eq!(dark.project(root), Some(color.value(Preset::Mist)));
        assert_eq!(light.project(root), Some(color.value(Preset::MistLight)));
        dark.project_colors.enabled = false;
        assert_eq!(dark.project(root), None);
    }

    #[test]
    fn ui_sizes_scale_with_the_interface_font() {
        let theme = Theme { ui_scale: 1.5, ..theme(Preset::Mist) };
        assert_eq!(theme.ui(48.0), px(72.0));
    }

    #[test]
    fn the_embedded_interface_font_loads() {
        let text_system = gpui_platform::current_platform(true).text_system();
        text_system.add_fonts(embedded_fonts()).unwrap();
        assert!(text_system.all_font_names().iter().any(|name| name == UI_FONT));
    }
}
