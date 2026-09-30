#![cfg(unix)]
//! DaemonClient against a real daemon.

use std::time::Duration;

use daemon::client::{ClientEvent, DaemonClient, Launch, StartError, restart_daemon};
use daemon::launch::{DaemonOptions, run_daemon};
use daemon::paths::Paths;
use protocol::message::SpawnRequest;

fn sh(script: &str) -> SpawnRequest {
    SpawnRequest { program: "sh".into(), args: vec!["-c".into(), script.into()], cwd: std::env::temp_dir(), cols: 80, rows: 24 }
}

fn daemon_spawner(paths: Paths) -> impl FnOnce() -> std::io::Result<()> + Send + 'static {
    move || {
        let options = DaemonOptions { paths, base_env: Some(std::env::vars().collect()), idle_timeout: Duration::from_secs(5) };
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

#[test]
fn start_spawns_then_reattaches_after_detach() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path(), "t");
    let launch = || Launch { explicit: None, default: sh("printf 'first pane'; sleep 10") };
    let (client, pane, events) = DaemonClient::start(&paths, launch(), daemon_spawner(paths.clone())).unwrap();
    output_until(&events, "first pane");
    drop((client, events)); // detach
    std::thread::sleep(Duration::from_millis(300));
    let (_client, again, events) = DaemonClient::start(&paths, launch(), || panic!("daemon should still run")).unwrap();
    assert_eq!(again, pane);
    assert!(matches!(events.recv_blocking().unwrap(), ClientEvent::Resync { .. }));
    output_until(&events, "first pane");
}

#[test]
fn input_reaches_the_pane() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path(), "t");
    let launch = Launch { explicit: Some(sh("read l; printf \"got:$l\"; sleep 5")), default: sh("true") };
    let (client, pane, events) = DaemonClient::start(&paths, launch, daemon_spawner(paths.clone())).unwrap();
    client.input(pane, b"xyz\r");
    output_until(&events, "got:xyz");
}

#[test]
fn stop_all_stops_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path(), "t");
    let launch = Launch { explicit: Some(sh("sleep 30")), default: sh("true") };
    let (client, _pane, _events) = DaemonClient::start(&paths, launch, daemon_spawner(paths.clone())).unwrap();
    client.stop_all_and_wait(Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(500));
    let launch = Launch { explicit: None, default: sh("true") };
    let result = DaemonClient::start(&paths, launch, || Err(std::io::Error::other("not spawning")));
    assert!(matches!(result, Err(StartError::Io(_))), "daemon still accepting connections");
}

#[test]
fn restart_daemon_stops_a_running_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path(), "t");
    let launch = Launch { explicit: Some(sh("sleep 30")), default: sh("true") };
    let (_client, _pane, events) = DaemonClient::start(&paths, launch, daemon_spawner(paths.clone())).unwrap();
    restart_daemon(&paths).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match events.recv_blocking() {
            Ok(ClientEvent::TakenOver | ClientEvent::Disconnected) | Err(_) => break,
            Ok(_) => assert!(std::time::Instant::now() < deadline),
        }
    }
}
