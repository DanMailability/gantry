//! gantryd — Gantry's daemon and bridge without the window. The SSH wizard installs it on remote hosts: it links
//! none of the window, sound and keyboard libraries that `gantry` needs.

#[path = "../headless.rs"]
mod headless;

use std::process::ExitCode;

fn main() -> ExitCode {
    let command = std::env::args().nth(1);
    headless::dispatch(command.as_deref()).unwrap_or_else(|| {
        eprintln!("usage: gantryd daemon | bridge | --version");
        ExitCode::from(2)
    })
}
