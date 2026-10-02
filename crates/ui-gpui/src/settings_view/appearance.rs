//! The Appearance section: theme, status colours and fonts.

use app_core::session_tree::Tone;
use app_core::settings::{Settings, StatusFills, ThemeChoice, parse_hex};
use app_core::theme_colors::{Preset, ProjectColor, Tokens, contrast, derive, readable_fill, tokens as preset_tokens, tone_colors};
use gpui::{Context, Div, FontWeight, IntoElement, KeyDownEvent, Window, div, px, rgb, rgba, deferred};

use super::*;
use crate::theme::{RADIUS, RADIUS_SMALL, Theme};

/// A session status whose sidebar fill can be chosen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Status {
    NeedsInput,
    Working,
    Done,
    Idle,
}

impl Status {
    const ALL: [Status; 4] = [Status::NeedsInput, Status::Working, Status::Done, Status::Idle];

    fn tone(self) -> Tone {
        match self {
            Status::NeedsInput => Tone::NeedsInput,
            Status::Working => Tone::Working,
            Status::Done => Tone::Done,
            Status::Idle => Tone::Idle,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Status::NeedsInput => "Needs input",
            Status::Working => "Working",
            Status::Done => "Done",
            Status::Idle => "Idle",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Status::NeedsInput => "Claude is waiting for you",
            Status::Working => "Claude is busy",
            Status::Done => "Finished; you have not looked yet",
            Status::Idle => "Waiting quietly; a muted fill",
        }
    }

    fn sample(self) -> &'static str {
        match self {
            Status::NeedsInput => "OPS work log triage",
            Status::Working => "Tab drag to reorder",
            Status::Done => "Right-click menus",
            Status::Idle => "Plan mode bridge hook",
        }
    }

    fn fill(self, fills: &StatusFills) -> Option<u32> {
        match self {
            Status::NeedsInput => fills.needs_input,
            Status::Working => fills.working,
            Status::Done => fills.done,
            Status::Idle => fills.idle,
        }
    }

    fn set(self, fills: &mut StatusFills, fill: Option<u32>) {
        match self {
            Status::NeedsInput => fills.needs_input = fill,
            Status::Working => fills.working = fill,
            Status::Done => fills.done = fill,
            Status::Idle => fills.idle = fill,
        }
    }

    /// The fill the preset draws when nothing is chosen.
    fn default_fill(self, preset: Preset) -> u32 {
        tone_colors(preset, &StatusFills::default(), self.tone()).fill.unwrap_or(preset_tokens(preset).surface)
    }
}

const INTERFACE_SIZE: Stepped = Stepped {
    name: "interface-size",
    get: |s| s.ui_font_size,
    set: |s, value| s.ui_font_size = value,
    delta: 1.0,
    min: 11.0,
    max: 20.0,
    unit: "",
};

const SUGGESTED_FILLS: [u32; 12] =
    [0xc1e5cd, 0xb7e4dd, 0xcfe8b4, 0xe6efb0, 0xa8dcc0, 0xd5ecd9, 0xf7c9a3, 0xf6b8b8, 0xe9c6ef, 0xc9cdf6, 0xb9d5f6, 0xf1e1a6];

const THEME_CARDS: [(ThemeChoice, &str, &str); 3] = [
    (ThemeChoice::Mist, "Mist", "Dark slate, light-blue accent"),
    (ThemeChoice::MistLight, "Mist light", "The same look on a light ground"),
    (ThemeChoice::System, "Follow system", "Dark or light from the desktop"),
];

const PROJECT_COLOURS_HINT: &str =
    "Each project gets a colour in the sidebar, on its tabs and in recent sessions. Change one from the project's right-click menu.";

/// "Picked colours: 3 folders" ("1 folder" in the singular).
fn picked_text(count: usize) -> String {
    format!("Picked colours: {count} {}", if count == 1 { "folder" } else { "folders" })
}

/// The contrast line for a picked `fill`, and whether the fill may be used.
fn contrast_label(fill: u32, tokens: &Tokens) -> (String, bool) {
    if !readable_fill(fill, tokens) {
        return ("Too low contrast; choose a lighter or darker fill".into(), false);
    }
    let ratio = contrast(derive(fill, tokens).ink, fill);
    let text = if ratio < 10.0 { format!("{ratio:.1}") } else { format!("{ratio:.0}") };
    (format!("Text contrast {text} : 1"), true)
}

/// What to store for a picked `fill`: nothing when it is the theme's own.
fn fill_to_store(fill: u32, default: u32) -> Option<u32> {
    (fill != default).then_some(fill)
}

/// `#rrggbb`, with or without the `#`.
fn parse_fill(text: &str) -> Option<u32> {
    let text = text.trim();
    parse_hex(&if text.starts_with('#') { text.to_string() } else { format!("#{text}") })
}

impl SettingsView {
    fn open_fill_picker(&mut self, status: Status, window: &mut Window, cx: &mut Context<Self>) {
        let theme = theme(cx);
        let chosen = status.fill(&cx.global::<SettingsStore>().settings().status_fills);
        let current = chosen.unwrap_or(status.default_fill(theme.preset));
        self.hex.clear();
        self.hex.insert(&hex(current));
        self.hex_selected = true;
        self.popover = Some(Popover::Fill(status));
        self.hex_focus.focus(window, cx);
        cx.notify();
    }

    fn apply_fill(&mut self, status: Status, fill: u32, cx: &mut Context<Self>) {
        let default = status.default_fill(theme(cx).preset);
        SettingsStore::update(cx, |settings| status.set(&mut settings.status_fills, fill_to_store(fill, default)));
    }

    fn hex_key_down(&mut self, status: Status, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key == "enter" {
            let tokens = theme(cx).tokens;
            if let Some(fill) = parse_fill(self.hex.text()).filter(|fill| readable_fill(*fill, &tokens)) {
                self.apply_fill(status, fill, cx);
                self.close_popover(window, cx);
            }
        } else if keystroke.key == "a" && (keystroke.modifiers.control || keystroke.modifiers.platform) {
            self.hex_selected = true;
        } else {
            let replaces = matches!(keystroke.key.as_str(), "backspace" | "delete")
                || keystroke.key_char.as_ref().is_some_and(|text| !text.is_empty() && !text.chars().any(char::is_control));
            if self.hex_selected && replaces && !keystroke.modifiers.control && !keystroke.modifiers.platform {
                self.hex.clear();
            }
            if !field::edit(&mut self.hex, keystroke) {
                return;
            }
            self.hex_selected = false;
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn appearance(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(theme.ui(26.))
            .child(self.theme_cards(theme, settings, window, cx))
            .child(self.status_colours(theme, settings, window, cx))
            .child(self.interface_group(theme, settings, window, cx))
            .child(self.project_colours(theme, settings, window, cx))
    }

    fn theme_cards(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let cards = THEME_CARDS.iter().enumerate().map(|(index, (choice, name, hint))| {
            let choice = *choice;
            let selected = settings.theme == choice;
            let preview = preset_tokens(if choice == ThemeChoice::MistLight { Preset::MistLight } else { Preset::Mist });
            let preset = if choice == ThemeChoice::MistLight { Preset::MistLight } else { Preset::Mist };
            let bar = |status: Status| div().h(theme.ui(8.)).rounded(px(4.)).bg(rgb(status.default_fill(preset)));
            let light = preset_tokens(Preset::MistLight);
            let miniature = div()
                .h(theme.ui(74.))
                .flex()
                .gap(theme.ui(6.))
                .p(theme.ui(8.))
                .rounded(px(RADIUS_SMALL))
                .bg(rgb(preview.ground))
                .border_1()
                .border_color(rgb(preview.line))
                .child(
                    div()
                        .w(theme.ui(48.))
                        .flex()
                        .flex_col()
                        .gap(theme.ui(4.))
                        .p(theme.ui(5.))
                        .rounded(px(6.))
                        .bg(rgb(preview.surface))
                        .child(bar(Status::NeedsInput))
                        .child(bar(Status::Working))
                        .child(bar(Status::Done)),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_end()
                        .p(theme.ui(5.))
                        .rounded(px(6.))
                        .bg(rgb(preview.terminal))
                        .child(div().w(theme.ui(18.)).h(theme.ui(6.)).rounded(px(3.)).bg(rgb(preview.accent))),
                )
                .when(choice == ThemeChoice::System, |mini| {
                    mini.child(div().flex_1().rounded(px(6.)).bg(rgb(light.terminal)).border_1().border_color(rgb(light.line)))
                });
            let radio = if selected { theme.tokens.accent } else { theme.tokens.muted };
            self.pressable("theme", index, None, window, cx, change(move |settings| settings.theme = choice))
                .w(theme.ui(210.))
                .p(theme.ui(10.))
                .flex()
                .flex_col()
                .gap(theme.ui(10.))
                .rounded(px(RADIUS))
                .bg(rgb(theme.tokens.raised))
                .border_2()
                .when(selected, |card| card.border_color(rgb(theme.tokens.accent)))
                .child(miniature)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme.ui(8.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(
                            div()
                                .size(theme.ui(14.))
                                .rounded_full()
                                .border_2()
                                .border_color(rgb(radio))
                                .when(selected, |dot| dot.bg(rgb(theme.tokens.accent))),
                        )
                        .child(*name),
                )
                .child(div().mt(theme.ui(-6.)).child(muted(theme, *hint)))
        });
        div().flex().flex_col().gap(theme.ui(12.)).child(heading(theme, "Theme")).child(div().flex().flex_wrap().gap(theme.ui(14.)).children(cards))
    }

    fn status_colours(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let rows = Status::ALL.map(|status| {
            let index = status as usize;
            let chosen = status.fill(&settings.status_fills);
            let colors = theme.tone(status.tone());
            let shown = chosen.unwrap_or_else(|| status.default_fill(theme.preset));
            let open = self.popover == Some(Popover::Fill(status));
            let sample = div()
                .w(theme.ui(300.))
                .max_w_full()
                .flex_none()
                .h(theme.ui(34.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(RADIUS))
                .bg(rgb(colors.fill.unwrap_or(theme.tokens.surface)))
                .text_color(rgb(colors.ink))
                .child(div().flex_none().size(theme.ui(8.)).rounded_full().bg(rgb(colors.dot)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(status == Status::NeedsInput, |el| el.font_weight(FontWeight::SEMIBOLD))
                        .child(status.sample()),
                )
                .child(div().flex_none().text_xs().text_color(rgba((colors.ink << 8) | 0xcc)).child(status.label()));
            let open_picker = handler(move |this, window, cx| this.open_fill_picker(status, window, cx));
            let picker_button = self
                .pressable("status-colour", index, Some(if open { theme.tokens.accent } else { theme.tokens.line }), window, cx, open_picker)
                .h(theme.ui(34.))
                .pl(theme.ui(6.))
                .pr(theme.ui(12.))
                .flex()
                .items_center()
                .gap(theme.ui(8.))
                .rounded(px(RADIUS_SMALL))
                .bg(rgb(theme.tokens.raised))
                .child(div().size(theme.ui(22.)).rounded(px(6.)).bg(rgb(shown)).border_1().border_color(rgb(theme.tokens.line)))
                .child(hex(shown));
            let picker = open.then(|| self.fill_picker(status, theme, window, cx));
            let reset = self
                .pressable("status-reset", index, None, window, cx, change(move |settings| status.set(&mut settings.status_fills, None)))
                .h(theme.ui(34.))
                .px(theme.ui(10.))
                .flex()
                .items_center()
                .rounded(px(RADIUS_SMALL))
                .text_color(rgb(if chosen.is_some() { theme.tokens.text } else { theme.tokens.muted }))
                .child("Reset");
            div().flex().flex_wrap().items_center().gap_x(theme.ui(18.)).gap_y(theme.ui(6.)).py(theme.ui(6.)).child(sample).child(
                div()
                    .flex_1()
                    .min_w(theme.ui(300.))
                    .flex()
                    .items_center()
                    .gap(theme.ui(18.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(theme.ui(90.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(status.label()))
                            .child(muted(theme, status.description())),
                    )
                    .child(div().relative().flex_none().child(picker_button).children(picker))
                    .child(reset.flex_none()),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap(theme.ui(10.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(theme.ui(12.))
                    .child(heading(theme, "Status colours"))
                    .child(div().text_color(rgb(theme.tokens.muted)).child("Pick a fill; text and dot colours follow it.")),
            )
            .children(rows)
    }

    fn interface_group(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let control = self.stepper(&INTERFACE_SIZE, settings, window, cx);
        let reset = self.reset_slot("reset-interface-size", 0, settings, |s| s.ui_font_size, |s, v| s.ui_font_size = v, window, cx);
        group(theme, "Interface", None, vec![row(theme, "Interface size", "Sidebar, tabs and menus.", control, reset)])
    }

    fn project_colours(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let enabled = settings.project_colors.enabled;
        let switch = self.switch("project-colours", 0, enabled, window, cx, change(move |settings| settings.project_colors.enabled = !enabled));
        let palette = |name: &'static str, preset: Preset| {
            let ground = preset_tokens(preset);
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(theme.ui(12.))
                .gap_y(theme.ui(6.))
                .child(div().flex_none().w(theme.ui(80.)).child(name))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(theme.ui(8.))
                        .p(theme.ui(8.))
                        .rounded(px(RADIUS_SMALL))
                        .bg(rgb(ground.ground))
                        .border_1()
                        .border_color(rgb(theme.tokens.line))
                        .children(ProjectColor::ALL.map(|color| div().flex_none().size(theme.ui(26.)).rounded(px(RADIUS_SMALL - 2.)).bg(rgb(color.value(preset))))),
                )
        };
        let picked = settings.project_colors.folders.len();
        let forget = (picked > 0).then(|| {
            let forget = self
                .pressable("forget-colours", 0, None, window, cx, change(|settings| settings.project_colors.folders.clear()))
                .px(theme.ui(10.))
                .h(theme.ui(30.))
                .flex()
                .items_center()
                .rounded(px(RADIUS_SMALL))
                .text_color(rgb(theme.tokens.accent))
                .child("Forget all");
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(theme.ui(12.))
                .child(div().text_color(rgb(theme.tokens.muted)).child(picked_text(picked)))
                .child(forget)
        });
        div()
            .flex()
            .flex_col()
            .gap(theme.ui(12.))
            .child(heading(theme, "Project colours"))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(theme.ui(16.))
                    .py(theme.ui(4.))
                    .child(switch)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("Colour projects"))
                            .child(muted(theme, PROJECT_COLOURS_HINT)),
                    ),
            )
            .child(palette("Mist", Preset::Mist))
            .child(palette("Mist light", Preset::MistLight))
            .children(forget)
    }

    fn fill_picker(&mut self, status: Status, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chosen = status.fill(&cx.global::<SettingsStore>().settings().status_fills);
        let current = chosen.unwrap_or_else(|| status.default_fill(theme.preset));
        let typed = parse_fill(self.hex.text());
        let verdict = typed.map(|fill| contrast_label(fill, &theme.tokens));
        let swatches = SUGGESTED_FILLS.iter().enumerate().map(|(index, fill)| {
            let fill = *fill;
            let pick = handler(move |this, _, cx| {
                this.apply_fill(status, fill, cx);
                this.hex.clear();
                this.hex.insert(&hex(fill));
                this.hex_selected = true;
                cx.notify();
            });
            self.pressable("swatch", index, None, window, cx, pick)
                .h(theme.ui(30.))
                .rounded(px(RADIUS_SMALL))
                .bg(rgb(fill))
                .border_2()
                .when(fill == current, |swatch| swatch.border_color(rgb(theme.tokens.text)))
        }).collect::<Vec<_>>();
        let tone = theme.tone(if verdict.as_ref().is_some_and(|(_, ok)| *ok) { Tone::Done } else { Tone::NeedsInput });
        let hex_focused = self.hex_focus.is_focused(window);
        let field_box = div()
            .id("hex-field")
            .track_focus(&self.hex_focus)
            .flex_1()
            .h(theme.ui(32.))
            .px(theme.ui(10.))
            .flex()
            .items_center()
            .rounded(px(RADIUS_SMALL))
            .bg(rgb(theme.tokens.surface))
            .border_1()
            .border_color(rgb(if hex_focused { theme.tokens.accent } else { theme.tokens.line }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| this.hex_key_down(status, event, window, cx)))
            .on_click(cx.listener(|this, _, window, cx| this.hex_focus.focus(window, cx)))
            .child(field::render(&self.hex, "#rrggbb", hex_focused, theme).when(self.hex_selected, |text| {
                text.bg(rgba((theme.tokens.accent << 8) | 0x55))
            }));
        deferred(
            div()
                .occlude()
                .absolute()
                .top(theme.ui(40.))
                .right_0()
                .w(theme.ui(272.))
                .p(theme.ui(14.))
                .flex()
                .flex_col()
                .gap(theme.ui(12.))
                .rounded(px(RADIUS))
                .bg(rgb(theme.tokens.raised))
                .border_1()
                .border_color(rgb(theme.tokens.line))
                .shadow_lg()
                .child(div().grid().grid_cols(6).gap(theme.ui(8.)).children(swatches))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(theme.ui(10.))
                        .text_color(rgb(theme.tokens.muted))
                        .child("Hex")
                        .child(field_box),
                )
                .child(match verdict {
                    Some((text, _)) => div()
                        .flex()
                        .items_start()
                        .gap(theme.ui(8.))
                        .text_xs()
                        .text_color(rgb(theme.tokens.muted))
                        .child(div().flex_none().mt(theme.ui(4.)).size(theme.ui(8.)).rounded_full().bg(rgb(tone.dot)))
                        .child(div().flex_1().min_w_0().child(text)),
                    None => muted(theme, "Type a colour as #rrggbb"),
                }),
        )
        .with_priority(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_core::theme_colors::tokens;

    #[test]
    fn picked_colours_text_is_singular_for_one_folder() {
        assert_eq!(picked_text(1), "Picked colours: 1 folder");
        assert_eq!(picked_text(3), "Picked colours: 3 folders");
        assert!(PROJECT_COLOURS_HINT.starts_with("Each project gets a colour in the sidebar, on its tabs and in recent sessions."));
    }

    #[test]
    fn contrast_line_has_one_decimal_below_ten() {
        let t = tokens(Preset::Mist);
        let (text, ok) = contrast_label(0xf7c9a3, &t);
        assert!(ok);
        let ratio = text.strip_prefix("Text contrast ").and_then(|rest| rest.strip_suffix(" : 1")).unwrap();
        let value: f32 = ratio.parse().unwrap();
        assert!(value >= 4.5);
        if value < 10.0 {
            assert_eq!(ratio.split('.').nth(1).map(str::len), Some(1), "{text}");
        } else {
            assert!(!ratio.contains('.'), "{text}");
        }
    }

    #[test]
    fn unreadable_fill_is_refused() {
        let t = tokens(Preset::Mist);
        // Mid grey: neither light nor dark enough for derived text.
        let grey = (0x00..=0xff_u32).map(|v| v * 0x010101).find(|fill| !readable_fill(*fill, &t)).expect("some grey is unreadable");
        assert_eq!(contrast_label(grey, &t), ("Too low contrast; choose a lighter or darker fill".to_string(), false));
    }

    #[test]
    fn the_themes_own_fill_is_stored_as_nothing() {
        assert_eq!(fill_to_store(0xc1e5cd, 0xc1e5cd), None);
        assert_eq!(fill_to_store(0xb7e4dd, 0xc1e5cd), Some(0xb7e4dd));
        assert_eq!(Status::Done.default_fill(Preset::Mist), tone_colors(Preset::Mist, &StatusFills::default(), Tone::Done).fill.unwrap());
    }

    #[test]
    fn fills_are_typed_with_or_without_the_hash() {
        assert_eq!(parse_fill("#c1e5cd"), Some(0xc1e5cd));
        assert_eq!(parse_fill(" c1e5cd "), Some(0xc1e5cd));
        assert_eq!(parse_fill("#c1e5"), None);
        assert_eq!(parse_fill(""), None);
    }
}
