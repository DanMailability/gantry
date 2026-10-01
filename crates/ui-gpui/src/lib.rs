//! GPUI user interface for Gantry. The only crate in the workspace that depends on GPUI.

mod alerts;
mod field;
pub mod launcher_view;
pub mod sidebar_view;
pub mod terminal_view;
pub mod workspace_view;
mod window_frame;

use std::path::PathBuf;

use app_core::layout_store;
use app_core::workspace::{Launch, restore};
use daemon::client::{Connection, DaemonClient, StartError, restart_daemon};
use daemon::launch::spawn_detached_daemon;
use daemon::paths::{Paths, current_profile};
use gpui::{
    App, AppContext, Bounds, Context, IntoElement, PromptLevel, Render, Styled, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, div, px, rgb, size,
};

use crate::workspace_view::WorkspaceView;

/// Placeholder shown while asking whether to restart a mismatched daemon.
struct Notice;

impl Render for Notice {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0x11141a))
    }
}

/// Freedesktop app id: Wayland app id / X11 class, desktop file and icon name.
pub const APP_ID: &str = "io.github.idogrf.Gantry";

/// Connect to (or start) the daemon, restore the layout, and run until the app quits.
pub fn run(launch: Launch) {
    gpui_platform::application().run(move |cx: &mut App| {
        terminal_view::bind_keys(cx);
        workspace_view::bind_keys(cx);
        let paths = Paths::for_profile(&current_profile());
        let bounds = Bounds::centered(None, size(px(1000.), px(700.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Gantry".into()), ..Default::default() }),
            app_id: Some(APP_ID.into()),
            #[cfg(target_os = "linux")]
            window_decorations: Some(gpui::WindowDecorations::Client),
            ..Default::default()
        };
        match DaemonClient::connect(&paths, spawn_detached_daemon) {
            Ok(connection) => {
                let window = cx
                    .open_window(options, |window, cx| cx.new(|cx| workspace(connection, &launch, &paths, window, cx)))
                    .expect("open window");
                install_quit_prompt(window, cx);
            }
            Err(StartError::VersionMismatch { .. }) => {
                let window = cx.open_window(options, |_, cx| cx.new(|_| Notice)).expect("open window");
                let _ = window.update(cx, move |_, window, cx| {
                    let answer = window.prompt(
                        PromptLevel::Warning,
                        "Gantry daemon version mismatch",
                        Some("Restarting stops all sessions."),
                        &["Restart daemon", "Quit"],
                        cx,
                    );
                    cx.spawn_in(window, async move |_, cx| {
                        let restarted = answer.await == Ok(0) && restart_daemon(&paths).is_ok();
                        let connection = restarted.then(|| DaemonClient::connect(&paths, spawn_detached_daemon).ok()).flatten();
                        let _ = cx.update(|window, cx| match connection {
                            Some(connection) => {
                                let view = window.replace_root(cx, |window, cx| workspace(connection, &launch, &paths, window, cx));
                                install_should_close(window, cx, view);
                            }
                            None => cx.quit(),
                        });
                    })
                    .detach();
                });
            }
            Err(err) => {
                eprintln!("gantry: {err}");
                cx.quit();
                return;
            }
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}

/// The root view for a fresh connection: the saved layout reconciled with the daemon's panes.
fn workspace(connection: Connection, launch: &Launch, paths: &Paths, window: &mut Window, cx: &mut Context<WorkspaceView>) -> WorkspaceView {
    let saved = layout_store::load(&paths.layout_file).unwrap_or_else(|err| {
        tracing::warn!("ignoring the saved layout: {err}");
        None
    });
    let restored = restore(saved, &connection.instance, &connection.panes, launch);
    let default_cwd: PathBuf = launch.default.cwd.clone();
    WorkspaceView::new(connection, restored, paths.layout_file.clone(), default_cwd, window, cx)
}

fn install_quit_prompt(window: WindowHandle<WorkspaceView>, cx: &mut App) {
    let _ = window.update(cx, |_, window, cx| {
        let view = cx.entity();
        install_should_close(window, cx, view);
    });
}

/// Closing the window asks: Detach (sessions keep running) / Stop all sessions / Cancel.
fn install_should_close(window: &mut Window, cx: &mut App, view: gpui::Entity<WorkspaceView>) {
    window.on_window_should_close(cx, move |window, cx| view.update(cx, |view, cx| view.request_quit(window, cx)));
}

#[cfg(test)]
mod tests {
    use super::APP_ID;

    const DESKTOP: &str = include_str!("../../../packaging/linux/io.github.idogrf.Gantry.desktop");

    #[test]
    fn the_desktop_entry_matches_the_app_id() {
        let field = |key: &str| {
            DESKTOP.lines().find_map(|line| line.strip_prefix(&format!("{key}="))).unwrap_or_default().to_string()
        };
        assert_eq!(field("Icon"), APP_ID);
        assert_eq!(field("StartupWMClass"), APP_ID);
        assert_eq!(field("Name"), "Gantry");
        assert_eq!(field("Exec"), "gantry");
    }
}
