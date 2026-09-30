//! ccm — Claude Control Manager.

mod args;

use std::process::ExitCode;
use std::time::Duration;

use args::LaunchArgs;
use daemon::launch::{DaemonOptions, DaemonOutcome, run_daemon};
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
    let launch = match LaunchArgs::parse(argv).map_err(|m| m.to_string()).and_then(|a| a.into_launch(80, 24).map_err(|e| e.to_string())) {
        Ok(launch) => launch,
        Err(message) => {
            eprintln!("ccm: {message}");
            return ExitCode::from(2);
        }
    };
    ui_gpui::run(launch);
    ExitCode::SUCCESS
}

fn daemon_main() -> ExitCode {
    let paths = Paths::for_profile(&current_profile());
    let appender = tracing_appender::rolling::daily(&paths.log_dir, "daemon.log");
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(appender)
        .with_ansi(false)
        .init();
    match run_daemon(DaemonOptions { paths, base_env: None, idle_timeout: Duration::from_secs(10) }) {
        Ok(DaemonOutcome::Stopped | DaemonOutcome::AlreadyRunning) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("daemon failed: {err:#}");
            ExitCode::FAILURE
        }
    }
}
