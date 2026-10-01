//! GPUI user interface for ccm. The only crate in the workspace that depends on GPUI.

pub mod terminal_view;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use daemon::client::{ClientEvent, DaemonClient, Launch, StartError, restart_daemon};
use daemon::launch::spawn_detached_daemon;
use daemon::paths::{Paths, current_profile};
use gpui::{
    App, AppContext, Bounds, Context, Entity, IntoElement, PromptLevel, Render, Styled,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, px, rgb, size,
};
use protocol::message::PaneId;

use crate::terminal_view::TerminalView;

type Started = (DaemonClient, PaneId, async_channel::Receiver<ClientEvent>);

/// Placeholder shown while asking whether to restart a mismatched daemon.
struct Notice;

impl Render for Notice {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0x11141a))
    }
}

/// Connect to (or start) the daemon, show the startup pane, and run until the app quits.
pub fn run(launch: Launch) {
    gpui_platform::application().run(move |cx: &mut App| {
        terminal_view::bind_keys(cx);
        let paths = Paths::for_profile(&current_profile());
        let (cols, rows) = launch.explicit.as_ref().map_or((launch.default.cols, launch.default.rows), |s| (s.cols, s.rows));
        let bounds = Bounds::centered(None, size(px(1000.), px(700.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("ccm".into()), ..Default::default() }),
            ..Default::default()
        };
        match DaemonClient::start(&paths, launch.clone(), spawn_detached_daemon) {
            Ok(started) => open_terminal(cx, options, started, cols, rows),
            Err(StartError::VersionMismatch { .. }) => {
                let window = cx.open_window(options, |_, cx| cx.new(|_| Notice)).expect("open window");
                let _ = window.update(cx, move |_, window, cx| {
                    let answer = window.prompt(
                        PromptLevel::Warning,
                        "ccm daemon version mismatch",
                        Some("Restarting stops all sessions."),
                        &["Restart daemon", "Quit"],
                        cx,
                    );
                    cx.spawn_in(window, async move |_, cx| {
                        let restarted = answer.await == Ok(0) && restart_daemon(&paths).is_ok();
                        let started = restarted.then(|| DaemonClient::start(&paths, launch, spawn_detached_daemon).ok()).flatten();
                        let _ = cx.update(|window, cx| match started {
                            Some((client, pane, events)) => {
                                let view = window.replace_root(cx, |window, cx| TerminalView::new(client.clone(), pane, events, cols, rows, window, cx));
                                install_quit_prompt(window, cx, view, client);
                            }
                            None => cx.quit(),
                        });
                    })
                    .detach();
                });
            }
            Err(err) => {
                eprintln!("ccm: {err}");
                cx.quit();
                return;
            }
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}

fn open_terminal(cx: &mut App, options: WindowOptions, (client, pane, events): Started, cols: u16, rows: u16) {
    let window = cx
        .open_window(options, |window, cx| cx.new(|cx| TerminalView::new(client.clone(), pane, events, cols, rows, window, cx)))
        .expect("open window");
    let _ = window.update(cx, move |_, window, cx| {
        let entity = cx.entity();
        install_quit_prompt(window, cx, entity, client);
    });
}

/// Closing the window asks: Detach (sessions keep running) / Stop all sessions / Cancel.
fn install_quit_prompt(window: &mut Window, cx: &mut App, view: Entity<TerminalView>, client: DaemonClient) {
    let prompting = Rc::new(Cell::new(false));
    window.on_window_should_close(cx, move |window, cx| {
        if view.read(cx).is_finished() {
            return true;
        }
        if prompting.replace(true) {
            return false;
        }
        let answer = window.prompt(
            PromptLevel::Info,
            "Quit ccm?",
            Some("Sessions keep running after Detach."),
            &["Detach", "Stop all sessions", "Cancel"],
            cx,
        );
        let (client, prompting) = (client.clone(), prompting.clone());
        cx.spawn(async move |cx| {
            match answer.await {
                Ok(0) => {
                    cx.update(|cx| cx.quit());
                }
                Ok(1) => {
                    client.stop_all_and_wait(Duration::from_secs(2));
                    cx.update(|cx| cx.quit());
                }
                _ => prompting.set(false),
            }
        })
        .detach();
        false
    });
}
