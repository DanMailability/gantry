//! What a sidebar row, tab or pane header leaves out (status and how long, config dir, dev container, folder),
//! shown after GPUI's tooltip delay (0.5 s).

use app_core::session_tree::Tone;
use app_core::workspace::DirBadge;
use gpui::{AnyView, App, Context, FontWeight, IntoElement, Render, Window, div, prelude::*, px, rgb};

use crate::theme::{RADIUS_SMALL, UI_FONT, container_icon, dir_badge, theme};

/// The mark before a title: only tones without a fill need one (the fill already says the rest).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lead {
    /// No mark.
    None,
    /// A hollow ring (Closed).
    Ring,
    /// `↗` (running outside Gantry).
    Arrow,
}

/// The lead mark of `tone`.
pub(crate) fn lead(tone: Tone) -> Lead {
    match tone {
        Tone::Closed => Lead::Ring,
        Tone::Elsewhere => Lead::Arrow,
        _ => Lead::None,
    }
}

/// The card's content.
#[derive(Clone)]
pub(crate) struct HoverCard {
    /// The row's title.
    pub title: String,
    /// Status and how long ("Working · 3m"), if any.
    pub status: Option<String>,
    /// Its config dir, when badges are shown.
    pub dir: Option<DirBadge>,
    /// Runs in a dev container.
    pub container: bool,
    /// Its folder, `~`-shortened.
    pub folder: Option<String>,
}

impl Render for HoverCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        div()
            .max_w(theme.ui(320.))
            .px(theme.ui(12.))
            .py(theme.ui(10.))
            .flex()
            .flex_col()
            .gap(theme.ui(4.))
            .rounded(px(RADIUS_SMALL))
            .bg(rgb(theme.tokens.raised))
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .shadow_lg()
            .font_family(UI_FONT)
            .text_sm()
            .text_color(rgb(theme.tokens.text))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(self.title.clone()))
            .children(self.status.clone().map(|status| div().child(status)))
            .children(self.dir.as_ref().map(|badge| div().flex().items_center().gap_2().child(dir_badge(badge, &theme)).child(format!("Config: {}", badge.name))))
            .when(self.container, |card| card.child(div().flex().items_center().gap_2().child(container_icon(theme.tokens.text, &theme)).child("Dev container")))
            .children(self.folder.clone().map(|folder| div().text_xs().text_color(rgb(theme.tokens.muted)).child(folder)))
    }
}

/// A `.tooltip(…)` builder showing `card`.
pub(crate) fn tooltip(card: HoverCard) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| cx.new(|_| card.clone()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tones_without_a_fill_keep_a_lead_mark() {
        for tone in [Tone::NeedsInput, Tone::Working, Tone::Done, Tone::Idle] {
            assert_eq!(lead(tone), Lead::None, "{tone:?}");
        }
        assert_eq!(lead(Tone::Closed), Lead::Ring);
        assert_eq!(lead(Tone::Elsewhere), Lead::Arrow);
    }
}
