//! A right-click menu: entries in groups, a hint for the entry's shortcut, the row's main action in the accent colour.

use std::rc::Rc;

use app_core::menus::{MenuAction, MenuItem, Shortcut};
use gpui::{App, FocusHandle, FontWeight, IntoElement, Pixels, Point, Window, anchored, deferred, div, prelude::*, px, rgb};

use crate::theme::{RADIUS, RADIUS_SMALL, Theme, UI_FONT};

/// An open menu: its items, where it opened, and the focus handle that takes `Esc`.
pub(crate) struct OpenMenu {
    /// Its items.
    pub items: Vec<MenuItem>,
    /// Top-left corner, in window coordinates.
    pub position: Point<Pixels>,
    /// Focused while open, so `Esc` closes it.
    pub focus: FocusHandle,
}

/// The hint shown for `shortcut`.
pub(crate) fn shortcut_text(shortcut: Shortcut) -> &'static str {
    #[cfg(target_os = "macos")]
    let text = match shortcut {
        Shortcut::SplitRight => "⌘D",
        Shortcut::SplitDown => "⌘E",
        Shortcut::Close => "⌘W",
        Shortcut::Recents => "⌘⇧R",
    };
    #[cfg(not(target_os = "macos"))]
    let text = match shortcut {
        Shortcut::SplitRight => "Ctrl+Shift+D",
        Shortcut::SplitDown => "Ctrl+Shift+E",
        Shortcut::Close => "Ctrl+Shift+W",
        Shortcut::Recents => "Ctrl+Shift+R",
    };
    text
}

/// Draw `menu` floating above everything; a pick or a click outside calls back, `Esc` dismisses.
pub(crate) fn render(
    id: &'static str,
    menu: &OpenMenu,
    theme: &Theme,
    on_pick: impl Fn(&MenuAction, &mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_pick = Rc::new(on_pick);
    let on_dismiss = Rc::new(on_dismiss);
    let dismiss_key = on_dismiss.clone();
    let rows = menu.items.iter().enumerate().map(|(index, item)| match item {
        MenuItem::Separator => div().h(px(1.)).mx_2().my_1().bg(rgb(theme.tokens.line)).into_any_element(),
        MenuItem::Entry(entry) => {
            let action = entry.action.clone();
            let on_pick = on_pick.clone();
            div()
                .id(("menu-item", index))
                .px_3()
                .py_1()
                .flex()
                .items_center()
                .gap_4()
                .rounded(px(RADIUS_SMALL))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(theme.tokens.line)))
                .when(entry.accent, |item| item.text_color(rgb(theme.tokens.accent)).font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1().child(entry.label))
                .when_some(entry.shortcut, |item, shortcut| item.child(div().text_xs().text_color(rgb(theme.tokens.muted)).child(shortcut_text(shortcut))))
                .on_click(move |_, window, cx| on_pick(&action, window, cx))
                .into_any_element()
        }
    });
    deferred(
        anchored().position(menu.position).snap_to_window().child(
            div()
                .id(id)
                // A floating layer: clicks must not reach what is beneath it.
                .occlude()
                .track_focus(&menu.focus)
                .on_key_down(move |event, window, cx| {
                    if event.keystroke.key == "escape" {
                        dismiss_key(window, cx);
                    }
                    // Keys must not reach what is beneath the menu (the sidebar filter).
                    cx.stop_propagation();
                })
                .on_mouse_down_out(move |_, window, cx| on_dismiss(window, cx))
                .min_w(theme.ui(220.))
                .p_1()
                .flex()
                .flex_col()
                .text_sm()
                .font_family(UI_FONT)
                .text_color(rgb(theme.tokens.text))
                .bg(rgb(theme.tokens.raised))
                .border_1()
                .border_color(rgb(theme.tokens.line))
                .rounded(px(RADIUS))
                .shadow_lg()
                .children(rows),
        ),
    )
    .with_priority(1)
}
