//! GPUI user interface for Gantry. The only crate in the workspace that depends on GPUI.

mod alerts;
mod askpass_view;
mod assets;
mod context_menu;
mod field;
mod hover_card;
pub mod launcher_view;
pub mod recents_view;
mod settings_store;
mod settings_view;
mod setup_view;
pub mod sidebar_view;
pub mod terminal_view;
mod theme;
mod update_watch;
mod window_frame;
pub mod workspace_view;

use std::collections::HashSet;
use std::path::PathBuf;

use app_core::layout_store;
use app_core::settings::WindowSettings;
use app_core::split_tree::NodeId;
use app_core::workspace::{HostLive, Launch, restore};
use daemon::client::{Connection, DaemonClient, StartError, restart_daemon};
use daemon::launch::spawn_detached_daemon;
use daemon::paths::{Paths, current_profile};
use gpui::{
    App, AppContext, Bounds, Context, IntoElement, PromptLevel, Render, Styled, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, div, px, rgb, size,
};

use protocol::host::HostId;

use crate::settings_store::SettingsStore;
use crate::workspace_view::{Resume, WorkspaceView};

/// Placeholder shown while asking whether to restart a mismatched daemon.
struct Notice;

impl Render for Notice {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(theme::theme(cx).tokens.ground))
    }
}

/// Freedesktop app id: Wayland app id / X11 class, desktop file and icon name.
pub const APP_ID: &str = "io.github.idogrf.Gantry";

/// What to resume on this start: under `--resume-sessions` exactly the leaves that were running when the update
/// stopped everything (`was_live`), else the settings decide.
fn resume_mode(window: &WindowSettings, resume_sessions: bool, was_live: HashSet<NodeId>) -> Resume {
    if resume_sessions {
        Resume::Only(was_live)
    } else if window.resume_sessions {
        Resume::Finished
    } else {
        Resume::Nothing
    }
}

/// Connect to (or start) the daemon, restore the layout, and run until the app quits. `resume_sessions` (the hidden
/// `--resume-sessions` flag) brings back the panes that were running when Restart and update stopped them.
pub fn run(launch: Launch, resume_sessions: bool) {
    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
        // Without this, the first notification makes macOS ask "Where is use_default?": mac-notification-sys looks up an
        // app named "use_default" with AppleScript. Must run once, before the first notification.
        #[cfg(target_os = "macos")]
        if let Err(err) = notify_rust::set_application(APP_ID) {
            tracing::warn!("set_application failed: {err}");
        }
        theme::load_fonts(cx);
        let paths = Paths::for_profile(&current_profile());
        SettingsStore::init(paths.settings_file.clone(), cx);
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
                    .open_window(options, |window, cx| {
                        cx.new(|cx| workspace(connection, &launch, &paths, resume_sessions, window, cx))
                    })
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
                        let connection =
                            restarted.then(|| DaemonClient::connect(&paths, spawn_detached_daemon).ok()).flatten();
                        let _ = cx.update(|window, cx| match connection {
                            Some(connection) => {
                                let view = window.replace_root(cx, |window, cx| {
                                    workspace(connection, &launch, &paths, resume_sessions, window, cx)
                                });
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

/// Debug builds only: other daemons of this machine, shown as hosts, from `GANTRY_EXTRA_HOSTS=name=profile[,…]`.
/// A test seam until SSH hosts exist.
#[cfg(debug_assertions)]
fn extra_hosts() -> Vec<(HostId, Connection)> {
    let Ok(spec) = std::env::var("GANTRY_EXTRA_HOSTS") else { return Vec::new() };
    spec.split(',')
        .filter_map(|entry| {
            let (name, profile) = entry.split_once('=')?;
            match DaemonClient::connect(&Paths::for_profile(profile), spawn_detached_daemon) {
                Ok(connection) => Some((HostId::named(name), connection)),
                Err(err) => {
                    tracing::warn!("extra host {name}: {err}");
                    None
                }
            }
        })
        .collect()
}

#[cfg(not(debug_assertions))]
fn extra_hosts() -> Vec<(HostId, Connection)> {
    Vec::new()
}

/// The root view for a fresh connection: the saved layout reconciled with the daemon's panes.
fn workspace(
    connection: Connection,
    launch: &Launch,
    paths: &Paths,
    resume_sessions: bool,
    window: &mut Window,
    cx: &mut Context<WorkspaceView>,
) -> WorkspaceView {
    let saved = layout_store::load(&paths.layout_file).unwrap_or_else(|err| {
        tracing::warn!("ignoring the saved layout: {err}");
        None
    });
    let window_settings = cx.global::<SettingsStore>().settings().window;
    let was_live: HashSet<NodeId> = saved
        .iter()
        .flat_map(|saved| saved.leaves.iter())
        .filter(|(_, leaf)| leaf.pane.is_some())
        .map(|(id, _)| *id)
        .collect();
    let keep = if resume_sessions { was_live.clone() } else { HashSet::new() };
    let extra = extra_hosts();
    let mut restored = {
        let mut hosts =
            vec![HostLive { host: HostId::local(), instance: &connection.instance, panes: &connection.panes }];
        hosts.extend(extra.iter().map(|(host, connection)| HostLive {
            host: host.clone(),
            instance: &connection.instance,
            panes: &connection.panes,
        }));
        restore(saved, &hosts, launch, window_settings.restore_layout, &keep)
    };
    let resume = resume_mode(&window_settings, resume_sessions, was_live);
    restored.workspace.sidebar.visible = window_settings.sidebar_at_start.visible(restored.workspace.sidebar.visible);
    let default_cwd: PathBuf = launch.default.cwd.clone();
    WorkspaceView::new(connection, extra, restored, paths.layout_file.clone(), default_cwd, resume, window, cx)
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
    use super::*;

    const DESKTOP: &str = include_str!("../../../packaging/linux/io.github.idogrf.Gantry.desktop");

    #[test]
    fn resume_flag_resumes_only_what_was_live() {
        let mut window = app_core::settings::Settings::default().window;
        let live = HashSet::from([3]);
        window.resume_sessions = false;
        assert_eq!(resume_mode(&window, true, live.clone()), Resume::Only(live.clone()));
        assert_eq!(resume_mode(&window, false, live.clone()), Resume::Nothing);
        window.resume_sessions = true;
        assert_eq!(resume_mode(&window, false, live), Resume::Finished);
    }

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
