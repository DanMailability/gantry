//! Processes running inside a pseudo-terminal (Unix PTY / Windows ConPTY).

use std::io::{Read, Write};
use std::path::PathBuf;

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Everything needed to start a process in a PTY.
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    /// Program to run (looked up on `PATH`).
    pub program: String,
    /// Arguments passed to the program.
    pub args: Vec<String>,
    /// Working directory of the process.
    pub cwd: PathBuf,
    /// Extra environment variables, applied after the terminal defaults.
    pub env: Vec<(String, String)>,
    /// Initial terminal width in columns.
    pub cols: u16,
    /// Initial terminal height in rows.
    pub rows: u16,
}

/// The user's default interactive shell: `$SHELL` (or `/bin/sh`) on Unix, `%COMSPEC%` (or `cmd.exe`) on Windows.
pub fn default_shell() -> String {
    #[cfg(unix)]
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    #[cfg(windows)]
    let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
    shell
}

/// A running process attached to a PTY. Dropping it kills the process.
pub struct PtyProcess {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtyProcess {
    /// Start `spec` in a new PTY. Returns the process and a blocking reader of its output.
    ///
    /// `TERM=xterm-256color` and `COLORTERM=truecolor` are set before `spec.env`.
    pub fn spawn(spec: &SpawnSpec) -> anyhow::Result<(PtyProcess, Box<dyn Read + Send>)> {
        let pair = native_pty_system().openpty(pty_size(spec.cols, spec.rows))?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        Ok((PtyProcess { master: pair.master, writer, child }, reader))
    }

    /// Send bytes to the process's input.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Tell the PTY (and so the process) about a new terminal size.
    pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.master.resize(pty_size(cols, rows))
    }

    /// Terminate the process.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::{Duration, Instant};

    fn read_until(mut reader: Box<dyn Read + Send>, needle: &str, timeout: Duration) -> String {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let deadline = Instant::now() + timeout;
        let mut out = String::new();
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) => {
                    out.push_str(&String::from_utf8_lossy(&chunk));
                    if out.contains(needle) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        out
    }

    #[cfg(unix)]
    fn sh(script: &str, cwd: &std::path::Path) -> SpawnSpec {
        SpawnSpec {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: cwd.to_path_buf(),
            env: Vec::new(),
            cols: 80,
            rows: 24,
        }
    }

    #[cfg(unix)]
    #[test]
    fn output_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let (_pty, reader) = PtyProcess::spawn(&sh("printf hello-pty", dir.path())).unwrap();
        assert!(read_until(reader, "hello-pty", Duration::from_secs(5)).contains("hello-pty"));
    }

    #[cfg(unix)]
    #[test]
    fn terminal_env_is_set() {
        let dir = tempfile::tempdir().unwrap();
        let (_pty, reader) =
            PtyProcess::spawn(&sh("printf \"$TERM/$COLORTERM\"", dir.path())).unwrap();
        let out = read_until(reader, "truecolor", Duration::from_secs(5));
        assert!(out.contains("xterm-256color/truecolor"), "got: {out:?}");
    }

    #[cfg(unix)]
    #[test]
    fn extra_env_and_cwd_are_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = sh("printf \"$CCM_TEST_VAR:\"; pwd -P", dir.path());
        spec.env.push(("CCM_TEST_VAR".into(), "set".into()));
        let (_pty, reader) = PtyProcess::spawn(&spec).unwrap();
        let expected = dir.path().canonicalize().unwrap().display().to_string();
        let out = read_until(reader, &expected, Duration::from_secs(5));
        assert!(out.contains(&format!("set:{expected}")), "got: {out:?}");
    }

    #[cfg(unix)]
    #[test]
    fn resize_reaches_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let (pty, reader) = PtyProcess::spawn(&sh("sleep 0.5; stty size", dir.path())).unwrap();
        pty.resize(100, 30).unwrap();
        let out = read_until(reader, "30 100", Duration::from_secs(5));
        assert!(out.contains("30 100"), "got: {out:?}");
    }

    #[cfg(unix)]
    #[test]
    fn written_input_reaches_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let (mut pty, reader) =
            PtyProcess::spawn(&sh("read line; printf \"got:$line\"", dir.path())).unwrap();
        pty.write(b"abc\r").unwrap();
        let out = read_until(reader, "got:abc", Duration::from_secs(5));
        assert!(out.contains("got:abc"), "got: {out:?}");
    }

    #[cfg(windows)]
    #[test]
    fn output_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SpawnSpec {
            program: "cmd.exe".into(),
            args: vec!["/C".into(), "echo hello-pty".into()],
            cwd: dir.path().to_path_buf(),
            env: Vec::new(),
            cols: 80,
            rows: 24,
        };
        let (_pty, reader) = PtyProcess::spawn(&spec).unwrap();
        assert!(read_until(reader, "hello-pty", Duration::from_secs(10)).contains("hello-pty"));
    }
}
