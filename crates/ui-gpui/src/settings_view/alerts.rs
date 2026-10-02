//! The Alerts section: what raises an alert, and how.

use app_core::session_tree::Tone;
use app_core::settings::{AlertSettings, AlertTone, EventAlerts, Settings};
use gpui::{Context, Div, FontWeight, MouseButton, Window, div, px, rgb, canvas};

use super::*;
use crate::alerts;
use crate::theme::{RADIUS, RADIUS_SMALL, Theme};

/// An event that can alert, a row of the alerts grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Event {
    NeedsInput,
    Finished,
}

impl Event {
    const ALL: [Event; 2] = [Event::NeedsInput, Event::Finished];

    fn index(self) -> usize {
        self as usize
    }

    fn label(self) -> &'static str {
        match self {
            Event::NeedsInput => "Needs input",
            Event::Finished => "Finished",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Event::NeedsInput => "Claude asks for permission or a reply",
            Event::Finished => "Busy, then idle",
        }
    }

    fn tone(self) -> Tone {
        match self {
            Event::NeedsInput => Tone::NeedsInput,
            Event::Finished => Tone::Done,
        }
    }

    pub(super) fn alerts(self, alerts: &mut AlertSettings) -> &mut EventAlerts {
        match self {
            Event::NeedsInput => &mut alerts.needs_input,
            Event::Finished => &mut alerts.finished,
        }
    }
}

const TONES: [(AlertTone, &str); 2] = [(AlertTone::Bell, "Bell (low, ringing)"), (AlertTone::Tick, "Tick (short, high)")];

fn tone_label(tone: AlertTone) -> &'static str {
    TONES.iter().find(|(candidate, _)| *candidate == tone).map_or("", |(_, label)| label)
}


const FINISHED_AFTER: Stepped = Stepped {
    name: "finished-after",
    get: |s| s.alerts.finished_after_seconds as f32,
    set: |s, value| s.alerts.finished_after_seconds = value as u32,
    delta: 1.0,
    min: 0.0,
    max: 600.0,
    unit: "s",
};


/// The volume (0 to 1, whole percents) under pointer position `x` on a slider spanning `left..left + width`.
fn volume_at(x: f32, left: f32, width: f32) -> f32 {
    if width <= 0.0 {
        return 0.0;
    }
    (((x - left) / width).clamp(0.0, 1.0) * 100.0).round() / 100.0
}

impl SettingsView {
    pub(super) fn alerts(&mut self, theme: &Theme, settings: &Settings, window: &Window, cx: &mut Context<Self>) -> Div {
        let alerts = settings.alerts;
        let wide = has_room(self.grid_bounds.get().size.width, theme.ui(GRID_WIDE));
        let trailing_space = || wide.then(|| div().flex_none().w(theme.ui(TRAILING_WIDTH)));
        let head = grid_line(
            theme,
            div().child("Event"),
            div().child("Badge"),
            div().child("Notification"),
            div().child("Sound"),
            trailing_space(),
        )
        .py(theme.ui(10.))
        .bg(rgb(theme.tokens.raised))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme.tokens.muted));
        let rows: Vec<Div> = Event::ALL
            .into_iter()
            .map(|event| {
                let mut copy = alerts;
                let channels = *event.alerts(&mut copy);
                let index = event.index();
                let badge = self.switch("badge", index, channels.badge, window, cx, change(move |s| {
                    let e = event.alerts(&mut s.alerts);
                    e.badge = !e.badge;
                }));
                let notification = self.switch("notification", index, channels.notification, window, cx, change(move |s| {
                    let e = event.alerts(&mut s.alerts);
                    e.notification = !e.notification;
                }));
                let sound = self.switch("sound", index, channels.sound, window, cx, change(move |s| {
                    let e = event.alerts(&mut s.alerts);
                    e.sound = !e.sound;
                }));
                let options = TONES
                    .iter()
                    .map(|(tone, text)| {
                        let tone = *tone;
                        (text.to_string(), tone == channels.tone, change(move |s| event.alerts(&mut s.alerts).tone = tone))
                    })
                    .collect();
                let picker = self.dropdown("tone", index, Popover::Tone(event), tone_label(channels.tone).into(), theme.ui(150.), options, window, cx);
                let tone = channels.tone;
                let play = handler(move |_, _, cx| {
                    let volume = cx.global::<SettingsStore>().settings().alerts.volume;
                    alerts::play(tone, volume);
                });
                let play = self.button("play", index, "▶ Play", window, cx, play);
                let trailing = div().flex_none().flex().items_center().gap(theme.ui(10.)).child(picker).child(play);
                let event_cell = div()
                            .flex()
                            .items_center()
                            .gap(theme.ui(10.))
                            .child(div().flex_none().size(theme.ui(10.)).rounded_full().bg(rgb(theme.tone(event.tone()).dot)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(event.label()))
                                    .child(muted(theme, event.description()).truncate()),
                            );
                let (inline, below) = if wide { (Some(trailing), None) } else { (None, Some(trailing)) };
                div()
                    .py(theme.ui(14.))
                    .border_t_1()
                    .border_color(rgb(theme.tokens.line))
                    .child(grid_line(theme, event_cell, badge, notification, sound, inline))
                    .children(below.map(|group| div().flex().justify_end().px(theme.ui(18.)).pt(theme.ui(10.)).child(group)))
            })
            .collect();
        let grid = div()
            .relative()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .rounded(px(RADIUS))
            .child(head)
            .children(rows);

        let threshold = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(theme.ui(10.))
            .text_color(rgb(theme.tokens.muted))
            .child("Count a session as finished after it was busy for")
            .child(self.stepper(&FINISHED_AFTER, settings, window, cx))
            .child("seconds");

        let when = [
            (
                "Alert even when the pane is on screen",
                "Off: a pane you can see in the active window raises nothing.",
                alerts.when_visible,
                change(|s| s.alerts.when_visible = !s.alerts.when_visible),
            ),
            (
                "Only send notifications while Gantry is in the background",
                "Badges and sounds still follow the grid above.",
                alerts.only_when_unfocused,
                change(|s| s.alerts.only_when_unfocused = !s.alerts.only_when_unfocused),
            ),
        ];
        let when_rows: Vec<Div> = when
            .into_iter()
            .enumerate()
            .map(|(index, (title, description, on, toggle))| {
                div()
                    .flex()
                    .items_center()
                    .gap(theme.ui(16.))
                    .py(theme.ui(10.))
                    .child(self.switch("when", index, on, window, cx, toggle))
                    .child(div().child(div().font_weight(FontWeight::SEMIBOLD).child(title)).child(muted(theme, description)))
            })
            .collect();

        let sounds = self.switch("play-sounds", 0, alerts.play_sounds, window, cx, change(|s| s.alerts.play_sounds = !s.alerts.play_sounds));
        let slider = self.volume_slider(theme, alerts.volume, window, cx);
        let sound_section = div()
            .flex()
            .flex_col()
            .gap(theme.ui(12.))
            .child(heading(theme, "Sound"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme.ui(28.))
                    .child(div().flex_none().flex().items_center().gap(theme.ui(12.)).child(sounds).child(div().font_weight(FontWeight::SEMIBOLD).child("Play sounds")))
                    .child(slider),
            )
            .child(
                div()
                    .max_w(theme.ui(720.))
                    .p(theme.ui(12.))
                    .rounded(px(RADIUS))
                    .bg(rgb(theme.tokens.raised))
                    .text_color(rgb(theme.tokens.muted))
                    .child("Claude Code also sends its own notifications for the same events, so you may get two. Turn them off in Claude Code's settings if you prefer Gantry's."),
            );

        div()
            .flex()
            .flex_col()
            .gap(theme.ui(26.))
            .child(div().flex().flex_col().gap(theme.ui(10.)).child(heading(theme, "How each event alerts you")).child(grid).child(threshold))
            .child(div().flex().flex_col().gap(theme.ui(8.)).child(heading(theme, "When")).children(when_rows))
            .child(sound_section)
    }

    /// "Volume", a draggable slider (written on release; arrow keys write at once) and the percentage.
    fn volume_slider(&mut self, theme: &Theme, volume: f32, window: &Window, cx: &mut Context<Self>) -> Div {
        let shown = self.volume_drag.unwrap_or(volume);
        let handle = self.stop("volume", 0, cx);
        let focused = handle.is_focused(window);
        let dragging = self.volume_drag.is_some();
        let entity = cx.entity();
        let bounds_cell = self.slider_bounds.clone();
        let knob = theme.ui(16.);
        let rail = div()
            .relative()
            .mx(knob / 2.)
            .w_full()
            .h(theme.ui(6.))
            .rounded_full()
            .bg(rgb(theme.tokens.line))
            .child(div().h_full().w(relative(shown)).rounded_full().bg(rgb(theme.tokens.accent)))
            .child(
                div()
                    .absolute()
                    .top(theme.ui(-5.))
                    .left(relative(shown))
                    .ml(-knob / 2.)
                    .size(knob)
                    .rounded_full()
                    .bg(rgb(theme.tokens.accent)),
            )
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, window, _| {
                        bounds_cell.set(bounds);
                        if !dragging {
                            return;
                        }
                        let moving = entity.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                moving.update(cx, |this, cx| {
                                    this.volume_drag = Some(volume_at(f32::from(event.position.x), f32::from(bounds.left()), f32::from(bounds.size.width)));
                                    cx.notify();
                                });
                            }
                        });
                        let released = entity.clone();
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                released.update(cx, |this, cx| {
                                    if let Some(volume) = this.volume_drag.take() {
                                        SettingsStore::update(cx, |s| s.alerts.volume = volume);
                                    }
                                    cx.notify();
                                });
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            );
        let slider = div()
            .id("volume")
            .track_focus(&handle)
            .tab_stop(true)
            .flex_1()
            .min_w(theme.ui(120.))
            .h(theme.ui(28.))
            .flex()
            .items_center()
            .cursor_pointer()
            .rounded(px(RADIUS_SMALL))
            .border_1()
            .border_color(if focused { rgb(theme.tokens.text) } else { rgba(0) })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    let bounds = this.slider_bounds.get();
                    this.volume_drag = Some(volume_at(f32::from(event.position.x), f32::from(bounds.left()), f32::from(bounds.size.width)));
                    this.stop("volume", 0, cx).focus(window, cx);
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                let delta = match event.keystroke.key.as_str() {
                    "left" | "down" => -5.0,
                    "right" | "up" => 5.0,
                    _ => return,
                };
                let percent = (cx.global::<SettingsStore>().settings().alerts.volume * 100.0).round();
                let volume = step(percent, delta, 0.0, 100.0) / 100.0;
                SettingsStore::update(cx, |s| s.alerts.volume = volume);
                cx.stop_propagation();
                cx.notify();
            }))
            .child(rail);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(theme.ui(12.))
            .child(div().flex_none().child("Volume"))
            .child(slider)
            .child(div().flex_none().w(theme.ui(44.)).text_color(rgb(theme.tokens.muted)).child(format!("{:.0}%", shown * 100.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slider_value_follows_the_pointer_in_whole_percents() {
        assert_eq!(volume_at(150.0, 100.0, 200.0), 0.25);
        assert_eq!(volume_at(50.0, 100.0, 200.0), 0.0);
        assert_eq!(volume_at(900.0, 100.0, 200.0), 1.0);
        assert_eq!(volume_at(100.0, 100.0, 0.0), 0.0);
    }
}
