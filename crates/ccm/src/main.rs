//! ccm — Claude Control Manager.

mod args;

use std::process::ExitCode;

use args::LaunchArgs;

fn main() -> ExitCode {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args = match LaunchArgs::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("ccm: {message}");
            return ExitCode::from(2);
        }
    };
    let spec = match args.into_spawn_spec(80, 24) {
        Ok(spec) => spec,
        Err(err) => {
            eprintln!("ccm: cannot determine working directory: {err}");
            return ExitCode::FAILURE;
        }
    };
    ui_gpui::run(spec);
    ExitCode::SUCCESS
}
