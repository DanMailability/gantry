#![cfg(unix)]
//! The daemon must keep running when started from an interactive terminal session.

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use daemon::pty::{PtyProcess, SpawnSpec};

#[test]
fn daemon_started_from_a_terminal_job_is_not_stopped_by_job_control() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().display().to_string();
    let gantry = env!("CARGO_BIN_EXE_gantry");
    // `set -m` gives the background job its own process group on the terminal, as when a GUI
    // started from a terminal launches the daemon; the daemon's `bash -l -i` then contends for it.
    let script = format!("set -m; '{gantry}' daemon >/dev/null 2>&1 & sleep 10");
    let spec = SpawnSpec {
        program: "sh".into(),
        args: vec!["-c".into(), script],
        cwd: dir.path().into(),
        env: vec![
            ("HOME".into(), home.clone()),
            ("XDG_RUNTIME_DIR".into(), home.clone()),
            ("XDG_DATA_HOME".into(), format!("{home}/data")),
            ("GANTRY_PROFILE".into(), "t".into()),
            ("SHELL".into(), "/bin/bash".into()),
        ],
        base_env: None,
        cols: 80,
        rows: 24,
    };
    let (_pty, _reader) = PtyProcess::spawn(&spec).unwrap();
    let socket = dir.path().join("gantry-t").join("daemon.sock");
    let deadline = Instant::now() + Duration::from_secs(8);
    while UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "daemon never accepted connections (stopped by job control?)");
        std::thread::sleep(Duration::from_millis(100));
    }
}
