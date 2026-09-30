//! GPUI user interface for ccm. The only crate in the workspace that depends on GPUI.

pub mod terminal_view;

use daemon::pty::{PtyProcess, SpawnSpec};
use gpui::{App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

use crate::terminal_view::TerminalView;

/// Start `spec` in a PTY, show it in a window, and run until the window closes.
pub fn run(spec: SpawnSpec) {
    gpui_platform::application().run(move |cx: &mut App| {
        let (pty, reader) = match PtyProcess::spawn(&spec) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("ccm: failed to start `{}`: {err:#}", spec.program);
                cx.quit();
                return;
            }
        };
        let (cols, rows) = (spec.cols, spec.rows);
        let bounds = Bounds::centered(None, size(px(1000.), px(700.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("ccm".into()), ..Default::default() }),
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            cx.new(|cx| TerminalView::new(pty, reader, cols, rows, window, cx))
        });
        if let Err(err) = opened {
            eprintln!("ccm: failed to open window: {err:#}");
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
