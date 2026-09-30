//! Command-line arguments: `ccm [--cwd DIR] [-- PROGRAM ARGS...]`.

use std::path::PathBuf;

use daemon::pty::{SpawnSpec, default_shell};

/// Parsed command line.
#[derive(Debug, PartialEq, Eq)]
pub struct LaunchArgs {
    /// Working directory for the program (default: current directory).
    pub cwd: Option<PathBuf>,
    /// Program and arguments (default: the user's shell).
    pub command: Vec<String>,
}

impl LaunchArgs {
    /// Parse arguments (without the program name).
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut cwd = None;
        let mut command = Vec::new();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cwd" => cwd = Some(PathBuf::from(args.next().ok_or("--cwd needs a directory")?)),
                "--" => {
                    command.extend(args);
                    break;
                }
                other => {
                    return Err(format!(
                        "unknown argument `{other}` (usage: ccm [--cwd DIR] [-- PROGRAM ARGS...])"
                    ));
                }
            }
        }
        Ok(LaunchArgs { cwd, command })
    }

    /// Turn the arguments into a PTY spawn request of the given initial size.
    pub fn into_spawn_spec(self, cols: u16, rows: u16) -> std::io::Result<SpawnSpec> {
        let cwd = match self.cwd {
            Some(dir) => dir,
            None => std::env::current_dir()?,
        };
        let mut command = self.command.into_iter();
        let program = command.next().unwrap_or_else(default_shell);
        Ok(SpawnSpec { program, args: command.collect(), cwd, env: Vec::new(), base_env: None, cols, rows })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<LaunchArgs, String> {
        LaunchArgs::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_args_means_default_shell_in_current_dir() {
        let spec = parse(&[]).unwrap().into_spawn_spec(80, 24).unwrap();
        assert_eq!(spec.program, daemon::pty::default_shell());
        assert_eq!(spec.cwd, std::env::current_dir().unwrap());
        assert_eq!((spec.cols, spec.rows), (80, 24));
    }

    #[test]
    fn cwd_and_command_are_parsed() {
        let args = parse(&["--cwd", "/tmp", "--", "claude", "--resume", "abc"]).unwrap();
        assert_eq!(args.cwd, Some(PathBuf::from("/tmp")));
        let spec = args.into_spawn_spec(80, 24).unwrap();
        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args, vec!["--resume", "abc"]);
    }

    #[test]
    fn bad_arguments_are_rejected() {
        assert!(parse(&["--cwd"]).is_err());
        assert!(parse(&["--nope"]).is_err());
    }
}
