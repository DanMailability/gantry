#![cfg(unix)]
//! End-to-end tests of the daemon server over a real local socket.

use std::time::Duration;

use daemon::paths::Paths;
use daemon::server::{ServerOptions, serve};
use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::tokio::{RecvHalf, SendHalf, Stream, prelude::*};
use protocol::frame::{Frame, read_frame, write_frame};
use protocol::message::*;
use tokio::time::timeout;

struct Harness {
    paths: Paths,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
    _dir: tempfile::TempDir,
}

async fn start_server(idle_timeout: Duration) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path(), "test");
    std::fs::create_dir_all(&paths.runtime_dir).unwrap();
    let listener = ListenerOptions::new().name(paths.socket_name().unwrap()).create_tokio().unwrap();
    let options = ServerOptions { base_env: std::env::vars().collect(), owner_uid: None, idle_timeout };
    let server = tokio::spawn(serve(listener, options));
    Harness { paths, server, _dir: dir }
}

struct TestClient {
    recv: RecvHalf,
    send: SendHalf,
}

impl TestClient {
    async fn connect(paths: &Paths) -> TestClient {
        let stream = Stream::connect(paths.socket_name().unwrap()).await.unwrap();
        let (recv, send) = stream.split();
        let mut client = TestClient { recv, send };
        client.send(&ClientMessage::Hello { protocol: PROTOCOL_VERSION }).await;
        assert_eq!(client.control().await, DaemonMessage::Welcome { protocol: PROTOCOL_VERSION });
        client
    }

    async fn send(&mut self, message: &ClientMessage) {
        write_frame(&mut self.send, &Frame::control(message)).await.unwrap();
    }

    async fn input(&mut self, pane: PaneId, bytes: &[u8]) {
        write_frame(&mut self.send, &Frame::Data { pane, bytes: bytes.to_vec() }).await.unwrap();
    }

    async fn frame(&mut self) -> Option<Frame> {
        timeout(Duration::from_secs(5), read_frame(&mut self.recv)).await.expect("timed out waiting for a frame").unwrap()
    }

    /// Next control message, skipping data frames.
    async fn control(&mut self) -> DaemonMessage {
        loop {
            match self.frame().await.expect("connection closed") {
                frame @ Frame::Control(_) => return frame.parse().unwrap().unwrap(),
                Frame::Data { .. } => {}
            }
        }
    }

    /// Collect data for `pane` until it contains `needle`; returns the collected text.
    async fn output_until(&mut self, pane: PaneId, needle: &str) -> String {
        let mut text = String::new();
        while !text.contains(needle) {
            match self.frame().await.expect("connection closed") {
                Frame::Data { pane: p, bytes } if p == pane => text.push_str(&String::from_utf8_lossy(&bytes)),
                _ => {}
            }
        }
        text
    }

    async fn spawn(&mut self, script: &str) -> PaneId {
        let spec = SpawnRequest {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            cols: 80,
            rows: 24,
        };
        self.send(&ClientMessage::Spawn { request: 1, spec }).await;
        loop {
            match self.control().await {
                DaemonMessage::Spawned { request: 1, pane } => return pane,
                DaemonMessage::SpawnFailed { error, .. } => panic!("spawn failed: {error}"),
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn spawned_pane_streams_output_and_takes_input() {
    let h = start_server(Duration::from_secs(10)).await;
    let mut c = TestClient::connect(&h.paths).await;
    let pane = c.spawn("printf ready; read line; printf \"got:$line\"; sleep 5").await;
    c.output_until(pane, "ready").await;
    c.input(pane, b"abc\r").await;
    c.output_until(pane, "got:abc").await;
}

#[tokio::test]
async fn panes_are_listed() {
    let h = start_server(Duration::from_secs(10)).await;
    let mut c = TestClient::connect(&h.paths).await;
    let pane = c.spawn("sleep 5").await;
    c.send(&ClientMessage::ListPanes).await;
    loop {
        if let DaemonMessage::Panes { panes } = c.control().await {
            assert_eq!(panes.len(), 1);
            assert_eq!((panes[0].pane, panes[0].program.as_str()), (pane, "sh"));
            break;
        }
    }
}

#[tokio::test]
async fn exit_is_reported_after_final_output() {
    let h = start_server(Duration::from_secs(10)).await;
    let mut c = TestClient::connect(&h.paths).await;
    let pane = c.spawn("printf 'last words'; exit 7").await;
    let mut text = String::new();
    loop {
        match c.frame().await.expect("closed") {
            Frame::Data { pane: p, bytes } if p == pane => text.push_str(&String::from_utf8_lossy(&bytes)),
            frame @ Frame::Control(_) => {
                if let DaemonMessage::Exited { pane: p, code } = frame.parse().unwrap().unwrap() {
                    assert_eq!((p, code), (pane, 7));
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(text.contains("last words"), "final output lost: {text:?}");
}

#[tokio::test]
async fn terminal_queries_are_answered_by_the_daemon() {
    let h = start_server(Duration::from_secs(10)).await;
    let mut c = TestClient::connect(&h.paths).await;
    let pane = c
        .spawn("stty raw -echo; printf '\\033[6n'; reply=$(dd bs=1 count=6 2>/dev/null); stty sane; printf \"reply:%s\" \"$reply\" | od -c | head -2; sleep 5")
        .await;
    let text = c.output_until(pane, "R").await;
    assert!(text.contains("033   [   1   ;   1   R"), "no cursor report reached the program: {text:?}");
}

#[tokio::test]
async fn resize_reaches_the_program() {
    let h = start_server(Duration::from_secs(10)).await;
    let mut c = TestClient::connect(&h.paths).await;
    let pane = c.spawn("sleep 0.5; stty size; sleep 5").await;
    c.send(&ClientMessage::Resize { pane, cols: 100, rows: 30, cell_width: 8, cell_height: 18 }).await;
    c.output_until(pane, "30 100").await;
}
