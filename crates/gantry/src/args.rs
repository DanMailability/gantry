//! Command-line arguments: `gantry [--cwd DIR] [-- PROGRAM ARGS...]`.

use std::path::PathBuf;

use app_core::workspace::{Launch, PaneLaunch};
use daemon::pty::default_shell;

/// Parsed command line.
#[derive(Debug, PartialEq, Eq)]
pub struct LaunchArgs {
    /// Working directory for the program (default: current directory).
    pub cwd: Option<PathBuf>,
    /// Program and arguments (default: the user's shell).
    pub command: Vec<String>,
    /// Resume finished sessions on this start (set by Restart and update; not in the usage text).
    pub resume_sessions: bool,
}

impl LaunchArgs {
    /// Parse arguments (without the program name).
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut cwd = None;
        let mut command = Vec::new();
        let mut resume_sessions = false;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cwd" => cwd = Some(PathBuf::from(args.next().ok_or("--cwd needs a directory")?)),
                "--resume-sessions" => resume_sessions = true,
                "--" => {
                    command.extend(args);
                    break;
                }
                other => {
                    return Err(format!("unknown argument `{other}` (usage: gantry [--cwd DIR] [-- PROGRAM ARGS...])"));
                }
            }
        }
        Ok(LaunchArgs { cwd, command, resume_sessions })
    }

    /// Turn the arguments into a launch request.
    pub fn into_launch(self) -> std::io::Result<Launch> {
        let current = std::env::current_dir()?;
        let default = PaneLaunch {
            program: default_shell(),
            args: Vec::new(),
            cwd: current.clone(),
            container: None,
            claude: false,
            claude_dir: None,
            env: Vec::new(),
        };
        let explicit = (self.cwd.is_some() || !self.command.is_empty()).then(|| {
            let mut command = self.command.into_iter();
            PaneLaunch {
                program: command.next().unwrap_or_else(default_shell),
                args: command.collect(),
                cwd: self.cwd.unwrap_or(current),
                container: None,
                claude: false,
                claude_dir: None,
                env: Vec::new(),
            }
        });
        Ok(Launch { explicit, default })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<LaunchArgs, String> {
        LaunchArgs::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_args_restores_or_opens_the_default_shell() {
        let launch = parse(&[]).unwrap().into_launch().unwrap();
        assert!(launch.explicit.is_none());
        assert_eq!(launch.default.program, daemon::pty::default_shell());
        assert_eq!(launch.default.cwd, std::env::current_dir().unwrap());
    }

    #[test]
    fn command_or_cwd_makes_an_explicit_launch() {
        let launch = parse(&["--cwd", "/tmp", "--", "claude", "--resume", "abc"]).unwrap().into_launch().unwrap();
        let explicit = launch.explicit.unwrap();
        assert_eq!(
            (explicit.program.as_str(), explicit.args.clone(), explicit.cwd.clone()),
            ("claude", vec!["--resume".into(), "abc".into()], PathBuf::from("/tmp"))
        );
        let cwd_only = parse(&["--cwd", "/tmp"]).unwrap().into_launch().unwrap();
        assert_eq!(cwd_only.explicit.unwrap().program, daemon::pty::default_shell());
    }

    #[test]
    fn resume_sessions_flag() {
        assert!(parse(&["--resume-sessions"]).unwrap().resume_sessions);
        assert!(!parse(&[]).unwrap().resume_sessions);
        let args = parse(&["--resume-sessions", "--cwd", "/tmp"]).unwrap();
        assert!(args.resume_sessions);
        assert_eq!(args.cwd, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn bad_arguments_are_rejected() {
        assert!(parse(&["--cwd"]).is_err());
        assert!(parse(&["--nope"]).is_err());
    }
}
