//! GPUI user interface for ccm. The only crate in the workspace that depends on GPUI.

use gpui::{App, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size};

struct Hello;

impl Render for Hello {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .justify_center()
            .items_center()
            .bg(rgb(0x11141a))
            .text_color(rgb(0x7cc4ff))
            .child("ccm")
    }
}

/// Open the ccm window and run the GPUI event loop until the app quits.
pub fn run() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(800.), px(500.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Hello),
        )
        .expect("failed to open window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
