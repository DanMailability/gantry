//! The sign-in dialog (an overlay): shows one question from ssh (password, key passphrase, host-key trust, or a plain
//! prompt) and sends the answer back to the attempt that asked. The answer is never stored or logged.

use app_core::host_state::{PromptKind, prompt_kind, prompt_title};
use app_core::text_field::TextField;
use daemon::askpass::{AskAnswer, AskPrompt};
use gpui::{
    App, ClickEvent, Context, Div, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, KeyDownEvent, Render,
    SharedString, Stateful, Window, div, prelude::*, px, rgb,
};

use crate::field;
use crate::theme::{RADIUS_LARGE, RADIUS_SMALL, Theme, theme};

/// What the dialog asks the window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AskpassEvent {
    /// The question was answered or cancelled: take the dialog down (and show the next question, if any).
    Done,
}

/// One question on screen.
pub(crate) struct AskpassView {
    focus_handle: FocusHandle,
    host: String,
    prompt: String,
    kind: PromptKind,
    field: TextField,
    reply: async_channel::Sender<AskAnswer>,
    answered: bool,
}

impl EventEmitter<AskpassEvent> for AskpassView {}

impl Focusable for AskpassView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AskpassView {
    /// The dialog for `prompt`.
    pub(crate) fn new(prompt: AskPrompt, cx: &mut Context<Self>) -> Self {
        let AskPrompt { host, prompt, reply } = prompt;
        AskpassView {
            focus_handle: cx.focus_handle(),
            kind: prompt_kind(&prompt),
            host,
            prompt,
            field: TextField::default(),
            reply,
            answered: false,
        }
    }

    /// Send `answer` (once) and ask the window to take the dialog down. The typed text is dropped with it.
    fn finish(&mut self, answer: AskAnswer, cx: &mut Context<Self>) {
        if self.answered {
            return;
        }
        self.answered = true;
        // The channel holds one answer and nothing else is sent: this never waits.
        let _ = self.reply.try_send(answer);
        self.field.clear();
        cx.emit(AskpassEvent::Done);
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.kind != PromptKind::YesNo {
            let text = self.field.text().to_string();
            self.finish(AskAnswer::Answer(text), cx);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "escape" => self.finish(AskAnswer::Cancel, cx),
            "enter" => self.submit(cx),
            _ if self.kind != PromptKind::YesNo => {
                if !field::edit(&mut self.field, keystroke, cx) {
                    return;
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

fn button(id: &'static str, label: impl Into<SharedString>, primary: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(theme.ui(32.))
        .px(theme.ui(14.))
        .flex()
        .items_center()
        .rounded(px(RADIUS_SMALL))
        .border_1()
        .border_color(rgb(if primary { theme.tokens.accent } else { theme.tokens.line }))
        .when(primary, |b| {
            b.bg(rgb(theme.tokens.accent)).text_color(rgb(theme.tokens.on_accent)).font_weight(FontWeight::SEMIBOLD)
        })
        .when(!primary, |b| b.bg(rgb(theme.tokens.raised)))
        .cursor_pointer()
        .hover(|style| style.border_color(rgb(theme.tokens.accent)))
        .child(label.into())
}

impl Render for AskpassView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let focused = self.focus_handle.is_focused(window);
        let input = match self.kind {
            PromptKind::YesNo => None,
            PromptKind::Plain => Some(field::render(
                &self.field,
                "",
                focused,
                &theme,
                field::on_press(cx, |this, offset, clicks, _, cx| {
                    this.field.press(offset, clicks);
                    cx.notify();
                }),
            )),
            PromptKind::Masked => {
                // Bullets with the caret at the end; the real text stays in `self.field`.
                let mut hidden = TextField::default();
                hidden.insert(&"•".repeat(self.field.text().chars().count()));
                Some(field::render(&hidden, "", focused, &theme, field::on_press(cx, |_, _, _, _, _| {})))
            }
        };
        let buttons = match self.kind {
            PromptKind::YesNo => div()
                .flex()
                .justify_end()
                .gap(theme.ui(8.))
                .child(button("askpass-no", "Don't connect", false, &theme).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.finish(AskAnswer::Answer("no".into()), cx)),
                ))
                .child(button("askpass-yes", "Trust and connect", true, &theme).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| this.finish(AskAnswer::Answer("yes".into()), cx)),
                )),
            _ => div()
                .flex()
                .justify_end()
                .gap(theme.ui(8.))
                .child(
                    button("askpass-cancel", "Cancel", false, &theme)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.finish(AskAnswer::Cancel, cx))),
                )
                .child(
                    button("askpass-ok", "Sign in", true, &theme)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.submit(cx))),
                ),
        };
        div()
            .key_context("Askpass")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .w(theme.ui(420.))
            .max_w_full()
            .flex()
            .flex_col()
            .gap(theme.ui(12.))
            .p(theme.ui(22.))
            .bg(rgb(theme.tokens.surface))
            .border_1()
            .border_color(rgb(theme.tokens.line))
            .rounded(px(RADIUS_LARGE))
            .shadow_lg()
            .text_sm()
            .text_color(rgb(theme.tokens.text))
            .child(
                div()
                    .text_size(theme.ui(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(prompt_title(&self.host, &self.prompt)),
            )
            .child(div().text_xs().text_color(rgb(theme.tokens.muted)).child(self.prompt.clone()))
            .children(input.map(|input| {
                div()
                    .h(theme.ui(32.))
                    .px(theme.ui(10.))
                    .flex()
                    .items_center()
                    .rounded(px(RADIUS_SMALL))
                    .border_1()
                    .border_color(rgb(theme.tokens.accent))
                    .bg(rgb(theme.tokens.ground))
                    .child(input.flex_1())
            }))
            .child(buttons)
    }
}
