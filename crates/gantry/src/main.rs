//! Gantry — a terminal for Claude Code sessions.

mod args;

use std::process::ExitCode;
use std::time::Duration;

use args::LaunchArgs;
use daemon::launch::{DaemonOptions, DaemonOutcome, detach_from_terminal, run_daemon};
use daemon::paths::{Paths, current_profile};

fn main() -> ExitCode {
    let mut argv = std::env::args().skip(1).peekable();
    match argv.peek().map(String::as_str) {
        Some("daemon") => return daemon_main(),
        Some("internal-print-env") => {
            println!("{}", daemon::shell_env::print_env_json());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let launch = match LaunchArgs::parse(argv).map_err(|m| m.to_string()).and_then(|a| a.into_launch().map_err(|e| e.to_string())) {
        Ok(launch) => launch,
        Err(message) => {
            eprintln!("gantry: {message}");
            return ExitCode::from(2);
        }
    };
    ui_gpui::run(launch);
    ExitCode::SUCCESS
}

/// Set on a daemon re-launched by `daemon_main`, so it never re-launches again.
const RESPAWNED_ENV: &str = "GANTRY_DAEMON_RESPAWNED";

fn daemon_main() -> ExitCode {
    match detach_from_terminal() {
        Ok(true) => {}
        // A process group leader (e.g. a shell job) cannot start a session: re-launch as a child that can.
        Ok(false) if std::env::var_os(RESPAWNED_ENV).is_none() => {
            let relaunched = std::env::current_exe().and_then(|exe| {
                std::process::Command::new(exe)
                    .arg("daemon")
                    .env(RESPAWNED_ENV, "1")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
            });
            return match relaunched {
                Ok(_) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("gantry daemon: failed to re-launch detached: {err}");
                    ExitCode::FAILURE
                }
            };
        }
        Ok(false) => {}
        Err(err) => eprintln!("gantry daemon: could not detach from the terminal: {err}"),
    }
    let paths = Paths::for_profile(&current_profile());
    let appender = tracing_appender::rolling::daily(&paths.log_dir, "daemon.log");
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(appender)
        .with_ansi(false)
        .init();
    match run_daemon(DaemonOptions { paths, base_env: None, idle_timeout: Duration::from_secs(10), claude_dir: None, docker: Some("docker".into()) }) {
        Ok(DaemonOutcome::Stopped | DaemonOutcome::AlreadyRunning) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("daemon failed: {err:#}");
            ExitCode::FAILURE
        }
    }
}
