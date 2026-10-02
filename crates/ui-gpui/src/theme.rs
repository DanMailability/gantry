//! The "Mist" look: dark slate surfaces, a light-blue accent, pastel fills that say what a session needs, rounded
//! corners, and the interface font (Figtree, embedded; the terminal keeps its own font).

use std::borrow::Cow;

use app_core::session_tree::Tone;
use app_core::settings::StatusFills;
use app_core::theme_colors::{Preset, ToneColors, Tokens, tone_colors};
use gpui::{App, Div, FontWeight, Global, ParentElement, Pixels, SharedString, Styled, div, px, rgba};

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
}

impl Global for Theme {}

impl Theme {
    /// The colours of a session row, tab or pane header in `tone`.
    pub fn tone(&self, tone: Tone) -> ToneColors {
        tone_colors(self.preset, &self.fills, tone)
    }

    /// `size` scaled with the interface font size.
    pub fn ui(&self, size: f32) -> Pixels {
        px(size * self.ui_scale)
    }
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

/// The small "container" tag on dev container sessions, tabs and pane headers, drawn in `ink`.
pub(crate) fn container_chip(ink: u32) -> Div {
    tag_chip(ink, "container")
}

/// The Claude config dir's name on sessions, tabs and pane headers, drawn like [`container_chip`]; shown only with
/// two or more config dirs.
pub(crate) fn dir_chip(ink: u32, name: String) -> Div {
    tag_chip(ink, name)
}

fn tag_chip(ink: u32, text: impl Into<SharedString>) -> Div {
    // Shrinks with an ellipsis (to nothing but its border) when the row is too narrow for it.
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .px(px(5.))
        .rounded(px(RADIUS_SMALL - 3.))
        .border_1()
        .border_color(rgba((ink << 8) | 0x66))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
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
        Theme { preset, tokens: tokens(preset), fills: StatusFills::default(), ui_scale: 1.0 }
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
