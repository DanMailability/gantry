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
        Some("--version") => {
            println!("{}", protocol::build::version_line());
            return ExitCode::SUCCESS;
        }
        Some("daemon") => return daemon_main(),
        Some("bridge") => return bridge_main(),
        Some("askpass") => return askpass_main(argv),
        Some("internal-print-env") => {
            println!("{}", daemon::shell_env::print_env_json());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let parsed = LaunchArgs::parse(argv);
    let resume_sessions = parsed.as_ref().is_ok_and(|args| args.resume_sessions);
    let launch = match parsed.and_then(|a| a.into_launch().map_err(|e| e.to_string())) {
        Ok(launch) => launch,
        Err(message) => {
            eprintln!("gantry: {message}");
            return ExitCode::from(2);
        }
    };
    ui_gpui::run(launch, resume_sessions);
    ExitCode::SUCCESS
}

/// `gantry bridge`: what ssh runs on a remote machine. Joins stdin/stdout to this machine's daemon socket.
/// Stdout carries only protocol bytes, so nothing here logs.
fn bridge_main() -> ExitCode {
    let paths = Paths::for_profile(&current_profile());
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("gantry bridge: {err}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(daemon::bridge::run_bridge(
        &paths,
        daemon::launch::spawn_detached_daemon,
        tokio::io::stdin(),
        tokio::io::stdout(),
    ));
    // Leave now: dropping the runtime would wait for the blocked read of stdin, which keeps the bridge (and its ssh
    // session) alive after the daemon closed the socket.
    match result {
        Ok(()) => std::process::exit(0),
        Err(err) => {
            eprintln!("gantry bridge: {err}");
            std::process::exit(1)
        }
    }
}

/// `gantry askpass <prompt>`: ssh's `SSH_ASKPASS` helper. Asks the GUI that started this attempt and prints the
/// answer; the answer is never logged.
fn askpass_main(mut argv: impl Iterator<Item = String>) -> ExitCode {
    let _ = argv.next();
    let prompt = argv.next().unwrap_or_default();
    let Some(socket) = std::env::var_os("GANTRY_ASKPASS_SOCKET") else {
        eprintln!("gantry askpass: only ssh attempts started by Gantry can ask");
        return ExitCode::FAILURE;
    };
    match daemon::askpass::run_helper(std::path::Path::new(&socket), &prompt) {
        Ok(Some(answer)) => {
            println!("{answer}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("gantry askpass: {err}");
            ExitCode::FAILURE
        }
    }
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
        // Tantivy logs every commit and merge at info: keep its warnings only.
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tantivy=warn".into()),
        )
        .with_writer(appender)
        .with_ansi(false)
        .init();
    match run_daemon(DaemonOptions {
        paths,
        base_env: None,
        idle_timeout: Duration::from_secs(10),
        claude_dir: None,
        docker: Some("docker".into()),
    }) {
        Ok(DaemonOutcome::Stopped | DaemonOutcome::AlreadyRunning) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("daemon failed: {err:#}");
            ExitCode::FAILURE
        }
    }
}
