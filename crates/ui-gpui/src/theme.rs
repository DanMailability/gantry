//! The "Mist" look: dark slate surfaces, a light-blue accent, pastel fills that say what a session needs, rounded
//! corners, and the interface font (Figtree, embedded; the terminal keeps its own font).

use std::borrow::Cow;

use app_core::session_tree::Tone;
use gpui::{App, Div, FontWeight, ParentElement, Styled, div, px, rgba};

/// Behind everything: the window and the gaps between panes.
pub(crate) const GROUND: u32 = 0x171b24;
/// Sidebar and tab bar.
pub(crate) const SURFACE: u32 = 0x1d2230;
/// Fields, buttons, the active segment of a switch.
pub(crate) const RAISED: u32 = 0x272e3e;
/// Hairlines and outlines.
pub(crate) const LINE: u32 = 0x2c3344;
/// Body text.
pub(crate) const TEXT: u32 = 0xe7eaf3;
/// Secondary text.
pub(crate) const MUTED: u32 = 0x97a1b6;
/// Focus rings, the caret, primary buttons.
pub(crate) const ACCENT: u32 = 0x9ecbff;
/// Text on an accent fill.
pub(crate) const ON_ACCENT: u32 = 0x0f2440;
/// Inside a pane: the terminal's default background (`app_core::palette::DARK`).
pub(crate) const TERMINAL: u32 = 0x141821;

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

/// Colours of a session row, tab or pane header in a given tone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ToneColors {
    /// Background (`None` = the surface shows through).
    pub fill: Option<u32>,
    /// Text.
    pub ink: u32,
    /// The status dot.
    pub dot: u32,
}

/// The colours of `tone`: solid pastels with dark ink for the states that ask for attention.
pub(crate) fn tone_colors(tone: Tone) -> ToneColors {
    let (fill, ink, dot) = match tone {
        Tone::NeedsInput => (Some(0xf7c9a3), 0x3b2414, 0xc9773d),
        Tone::Working => (Some(0xb9d5f6), 0x132a43, 0x4f8fd6),
        Tone::Done => (Some(0xc1e5cd), 0x143322, 0x4c9e6b),
        Tone::Idle => (Some(0x2f3647), 0xd5dae6, MUTED),
        Tone::Elsewhere => (None, 0xc9d0de, MUTED),
        Tone::Closed => (None, MUTED, 0x555e70),
    };
    ToneColors { fill, ink, dot }
}

/// The small "container" tag on dev container sessions, tabs and pane headers, drawn in `ink`.
pub(crate) fn container_chip(ink: u32) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(RADIUS_SMALL - 3.))
        .border_1()
        .border_color(rgba((ink << 8) | 0x66))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child("container")
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
    use app_core::theme_colors::contrast;

    #[test]
    fn row_text_is_readable_on_every_tone() {
        let tones = [Tone::NeedsInput, Tone::Working, Tone::Done, Tone::Idle, Tone::Elsewhere, Tone::Closed];
        for tone in tones {
            let colors = tone_colors(tone);
            let ratio = contrast(colors.ink, colors.fill.unwrap_or(SURFACE));
            assert!(ratio >= 4.5, "{tone:?}: {ratio:.2}");
        }
        assert!(contrast(TEXT, SURFACE) >= 4.5 && contrast(MUTED, SURFACE) >= 4.5 && contrast(ON_ACCENT, ACCENT) >= 4.5);
    }

    #[test]
    fn states_that_ask_for_attention_stand_out_from_quiet_ones() {
        // The pastel fills are far lighter than the surface; quiet states stay close to it.
        for tone in [Tone::NeedsInput, Tone::Working, Tone::Done] {
            assert!(contrast(tone_colors(tone).fill.unwrap(), SURFACE) >= 7.0, "{tone:?}");
        }
        assert!(contrast(tone_colors(Tone::Idle).fill.unwrap(), SURFACE) < 1.5);
    }

    #[test]
    fn pane_cards_match_the_terminal_background() {
        assert_eq!(app_core::snapshot::Rgb::hex(TERMINAL), app_core::palette::DARK.background);
    }

    #[test]
    fn the_embedded_interface_font_loads() {
        let text_system = gpui_platform::current_platform(true).text_system();
        text_system.add_fonts(embedded_fonts()).unwrap();
        assert!(text_system.all_font_names().iter().any(|name| name == UI_FONT));
    }
}
