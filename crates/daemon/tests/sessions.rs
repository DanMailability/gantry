#![cfg(target_os = "linux")]
//! Session index → daemon → client, with a temp Claude config directory.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use daemon::client::{ClientEvent, Connection, DaemonClient};
use daemon::launch::{DaemonOptions, run_daemon};
use daemon::paths::Paths;
use protocol::message::{PaneId, SessionInfo, SessionState, SessionStatus, SpawnRequest};

struct Setup {
    _dir: tempfile::TempDir,
    config: PathBuf,
    work: PathBuf,
    conn: Connection,
}

/// A daemon indexing a temp config dir; `prepare(config, work)` runs before it starts.
fn start(prepare: impl FnOnce(&Path, &Path)) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let (config, work) = (dir.path().join("claude"), dir.path().join("work"));
    std::fs::create_dir_all(&work).unwrap();
    prepare(&config, &work);
    let paths = Paths::in_dir(dir.path(), "t");
    let (spawn_paths, claude_dir) = (paths.clone(), config.clone());
    let conn = DaemonClient::connect(&paths, move || {
        let options = DaemonOptions {
            paths: spawn_paths,
            base_env: Some(std::env::vars().collect()),
            idle_timeout: Duration::from_secs(5),
            claude_dir: Some(claude_dir),
        };
        std::thread::spawn(move || run_daemon(options));
        Ok(())
    })
    .unwrap();
    Setup { _dir: dir, config, work, conn }
}

fn transcript(config: &Path, id: &str, cwd: &Path, prompt: &str) {
    let folder = config.join("projects").join("-work");
    std::fs::create_dir_all(&folder).unwrap();
    let line = serde_json::json!({"type": "user", "cwd": cwd, "message": {"content": prompt}});
    std::fs::write(folder.join(format!("{id}.jsonl")), format!("{line}\n")).unwrap();
}

/// Wait until a session event carries session `id` satisfying `check`.
fn wait_for(conn: &Connection, id: &str, check: impl Fn(&SessionInfo) -> bool) -> SessionInfo {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "timed out waiting for session {id}");
        let sessions = match conn.events.try_recv() {
            Ok(ClientEvent::Sessions { sessions }) => sessions,
            Ok(ClientEvent::SessionsChanged { upserted, .. }) => upserted,
            Ok(_) => continue,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
        };
        if let Some(found) = sessions.into_iter().find(|s| s.id == id && check(s)) {
            return found;
        }
    }
}

/// Spawn `program args` in `cwd`; returns the pane and the output seen until `needle`.
fn spawn(conn: &Connection, program: &str, args: &[&str], cwd: &Path, needle: &str) -> (PaneId, String) {
    let spec = SpawnRequest {
        program: program.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        cwd: cwd.into(),
        cols: 80,
        rows: 24,
        cwd_from: None,
    };
    conn.client.spawn(1, spec);
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut pane, mut text) = (None, String::new());
    while pane.is_none() || !text.contains(needle) {
        assert!(Instant::now() < deadline, "timed out; output {text:?}");
        match conn.events.try_recv() {
            Ok(ClientEvent::Spawned { pane: p, .. }) => pane = Some(p),
            Ok(ClientEvent::SpawnFailed { error, .. }) => panic!("spawn failed: {error}"),
            Ok(ClientEvent::Output { bytes, .. }) => text.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    (pane.unwrap(), text)
}

#[test]
fn closed_sessions_are_listed_on_connect() {
    let s = start(|config, work| transcript(config, "s1", work, "hello there"));
    let found = wait_for(&s.conn, "s1", |s| s.state == SessionState::Closed);
    assert_eq!((found.title.as_str(), found.cwd.as_path(), found.cwd_exists), ("hello there", s.work.as_path(), true));
}

#[test]
fn new_transcripts_arrive_as_changes() {
    let s = start(|config, work| transcript(config, "s1", work, "first"));
    wait_for(&s.conn, "s1", |_| true);
    transcript(&s.config, "s2", &s.work, "second");
    wait_for(&s.conn, "s2", |s| s.title == "second");
}

#[test]
fn a_pane_whose_process_wrote_a_pid_file_is_managed() {
    let s = start(|config, work| transcript(config, "s1", work, "first"));
    let (pane, output) = spawn(&s.conn, "sh", &["-c", "echo PID:$$; sleep 30"], &s.work, "PID:");
    let digits: String = output.split("PID:").nth(1).unwrap().chars().take_while(char::is_ascii_digit).collect();
    let pid: u32 = digits.parse().unwrap();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start = claude_index::host::proc_start_time(&stat).unwrap();
    std::fs::create_dir_all(s.config.join("sessions")).unwrap();
    let pid_file = serde_json::json!({"pid": pid, "sessionId": "s3", "cwd": s.work, "procStart": start, "updatedAt": 1,
        "status": "waiting", "waitingFor": "input needed", "statusUpdatedAt": 7});
    std::fs::write(s.config.join("sessions").join(format!("{pid}.json")), pid_file.to_string()).unwrap();
    let found = wait_for(&s.conn, "s3", |s| s.state == SessionState::Managed { pane });
    assert_eq!((found.status, found.status_since_ms), (Some(SessionStatus::NeedsInput { reason: Some("input needed".into()) }), 7));
}

#[test]
fn a_claude_pane_is_managed_by_its_session_id_until_it_exits() {
    let s = start(|config, work| {
        transcript(config, "s4", work, "resumable");
        let script = work.join("claude");
        std::fs::write(&script, "#!/bin/sh\necho started\nsleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    });
    wait_for(&s.conn, "s4", |s| s.state == SessionState::Closed);
    let program = s.work.join("claude").display().to_string();
    // Return at `Spawned`: the Managed change follows it directly and must not be consumed here.
    let (pane, _) = spawn(&s.conn, &program, &["--session-id", "s4"], &s.work, "");
    wait_for(&s.conn, "s4", |s| s.state == SessionState::Managed { pane });
    s.conn.client.kill(pane);
    wait_for(&s.conn, "s4", |s| s.state == SessionState::Closed);
}
