#![cfg(unix)]
//! DaemonClient against a real daemon.

use std::time::Duration;

use daemon::client::{ClientEvent, DaemonClient, StartError, restart_daemon};
use daemon::launch::{DaemonOptions, run_daemon};
use daemon::paths::Paths;
use protocol::message::SpawnRequest;

fn sh(script: &str) -> SpawnRequest {
    SpawnRequest { program: "sh".into(), args: vec!["-c".into(), script.into()], cwd: std::env::temp_dir(), cols: 80, rows: 24, cwd_from: None }
}

fn daemon_spawner(paths: Paths) -> impl FnOnce() -> std::io::Result<()> + Send + 'static {
    move || {
        let claude_dir = Some(paths.runtime_dir.with_file_name("claude"));
        let options = DaemonOptions { paths, base_env: Some(std::env::vars().collect()), idle_timeout: Duration::from_secs(5), claude_dir };
        std::thread::spawn(move || run_daemon(options));
        Ok(())
    }
}

fn output_until(events: &async_channel::Receiver<ClientEvent>, needle: &str) -> String {
    let mut text = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !text.contains(needle) {
        assert!(std::time::Instant::now() < deadline, "timed out; got {text:?}");
        if let Ok(ClientEvent::Output { bytes, .. }) = events.recv_blocking() {
            text.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    text
}

/// Connect, spawn `script`, and wait until it started.
fn connect_with(paths: &Paths, script: &str) -> (daemon::client::Connection, protocol::message::PaneId) {
    let conn = DaemonClient::connect(paths, daemon_spawner(paths.clone())).unwrap();
    conn.client.spawn(1, sh(script));
    let ClientEvent::Spawned { pane, .. } = next_event(&conn.events, |e| matches!(e, ClientEvent::Spawned { .. })) else {
        unreachable!()
    };
    (conn, pane)
}

#[test]
fn input_reaches_the_pane() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path(), "t");
    let (conn, pane) = connect_with(&paths, "read l; printf \"got:$l\"; sleep 5");
    conn.client.input(pane, b"xyz\r");
    output_until(&conn.events, "got:xyz");
}

#[test]
fn stop_all_stops_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path(), "t");
    let (conn, _pane) = connect_with(&paths, "sleep 30");
    conn.client.stop_all_and_wait(Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(500));
    let result = DaemonClient::connect(&paths, || Err(std::io::Error::other("not spawning")));
    assert!(matches!(result, Err(StartError::Io(_))), "daemon still accepting connections");
}

#[test]
fn restart_daemon_stops_a_running_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path(), "t");
    let (conn, _pane) = connect_with(&paths, "sleep 30");
    restart_daemon(&paths).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match conn.events.recv_blocking() {
            Ok(ClientEvent::TakenOver | ClientEvent::Disconnected) | Err(_) => break,
            Ok(_) => assert!(std::time::Instant::now() < deadline),
        }
    }
}

fn next_event(events: &async_channel::Receiver<ClientEvent>, wanted: impl Fn(&ClientEvent) -> bool) -> ClientEvent {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for an event");
        let event = events.recv_blocking().unwrap();
        if wanted(&event) {
            return event;
        }
    }
}

#[test]
fn connect_then_spawn_attach_and_kill() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path(), "t");
    let first = DaemonClient::connect(&paths, daemon_spawner(paths.clone())).unwrap();
    assert!(first.panes.is_empty());
    first.client.spawn(7, sh("printf 'hello pane'; sleep 10"));
    let ClientEvent::Spawned { request: 7, pane, cwd } = next_event(&first.events, |e| matches!(e, ClientEvent::Spawned { .. })) else {
        unreachable!()
    };
    assert_eq!(cwd, std::env::temp_dir());
    output_until(&first.events, "hello pane");
    let instance = first.instance.clone();
    drop(first); // detach
    std::thread::sleep(Duration::from_millis(300));

    let again = DaemonClient::connect(&paths, || panic!("daemon should still run")).unwrap();
    assert_eq!(again.instance, instance);
    assert_eq!(again.panes.len(), 1);
    assert_eq!(again.panes[0].pane, pane);
    assert_eq!(again.panes[0].args, vec!["-c".to_string(), "printf 'hello pane'; sleep 10".to_string()]);
    again.client.attach(pane);
    let not_sessions = |e: &ClientEvent| !matches!(e, ClientEvent::Sessions { .. } | ClientEvent::SessionsChanged { .. });
    assert!(matches!(next_event(&again.events, not_sessions), ClientEvent::Resync { pane: p, .. } if p == pane));
    output_until(&again.events, "hello pane");
    again.client.kill(pane);
    assert!(matches!(next_event(&again.events, |e| matches!(e, ClientEvent::Exited { .. })), ClientEvent::Exited { pane: p, .. } if p == pane));
}

#[test]
fn spawn_failure_is_an_event() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path(), "t");
    let conn = DaemonClient::connect(&paths, daemon_spawner(paths.clone())).unwrap();
    let mut spec = sh("true");
    spec.program = "/nonexistent/ccm-test-program".into();
    conn.client.spawn(3, spec);
    assert!(matches!(
        next_event(&conn.events, |e| matches!(e, ClientEvent::SpawnFailed { .. })),
        ClientEvent::SpawnFailed { request: 3, .. }
    ));
}
