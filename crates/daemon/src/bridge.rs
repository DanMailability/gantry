//! `gantry bridge`: what ssh runs on the remote. It joins its stdin/stdout to the local daemon's socket, starting the
//! daemon when none runs, so a GUI on another machine can talk to the daemon through ssh. Stdout carries only
//! protocol bytes.

use std::io;

use interprocess::local_socket::tokio::prelude::*;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::launch::connect_or_spawn;
use crate::paths::Paths;

/// Join `input`/`output` (the bridge's stdin and stdout) to the profile's daemon socket, starting the daemon with
/// `spawn` when none runs. Returns when either side closes; the daemon keeps running.
pub async fn run_bridge<I, O>(
    paths: &Paths,
    spawn: impl FnOnce() -> io::Result<()>,
    mut input: I,
    mut output: O,
) -> io::Result<()>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let stream = connect_or_spawn(paths, spawn).await?;
    let (mut from_daemon, mut to_daemon) = stream.split();
    let up = async {
        tokio::io::copy(&mut input, &mut to_daemon).await?;
        to_daemon.shutdown().await
    };
    // An explicit loop with a flush per write: frames must reach the GUI at once.
    let down = async {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let read = from_daemon.read(&mut buffer).await?;
            if read == 0 {
                return Ok::<(), io::Error>(());
            }
            output.write_all(&buffer[..read]).await?;
            output.flush().await?;
        }
    };
    tokio::select! {
        result = up => result,
        result = down => result,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use protocol::frame::{Frame, read_frame, write_frame};
    use protocol::message::{ClientMessage, DaemonMessage, PROTOCOL_VERSION};

    use super::*;
    use crate::launch::{DaemonOptions, run_daemon};

    fn options(dir: &std::path::Path) -> DaemonOptions {
        DaemonOptions {
            paths: Paths::in_dir(dir, "bridge"),
            base_env: Some(std::env::vars().collect()),
            idle_timeout: Duration::from_secs(5),
            claude_dir: Some(dir.join("claude")),
            docker: None,
        }
    }

    #[test]
    fn a_client_talks_to_the_daemon_through_the_bridge_and_stdout_has_only_frames() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path(), "bridge");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (mut gui_in, bridge_in) = tokio::io::duplex(64 * 1024);
            let (bridge_out, mut gui_out) = tokio::io::duplex(64 * 1024);
            let daemon_options = options(dir.path());
            let spawn = move || {
                std::thread::spawn(move || run_daemon(daemon_options));
                Ok(())
            };
            let bridge = tokio::spawn(async move { run_bridge(&paths, spawn, bridge_in, bridge_out).await });
            write_frame(&mut gui_in, &Frame::control(&ClientMessage::Hello { protocol: PROTOCOL_VERSION }))
                .await
                .unwrap();
            // The very first bytes out of the bridge parse as a frame: nothing else was written to stdout.
            let frame = read_frame(&mut gui_out).await.unwrap().expect("a frame");
            match frame.parse::<DaemonMessage>().expect("a control frame").unwrap() {
                DaemonMessage::Welcome { instance, .. } => assert!(!instance.is_empty()),
                other => panic!("expected Welcome, got {other:?}"),
            }
            write_frame(&mut gui_in, &Frame::control(&ClientMessage::ListPanes)).await.unwrap();
            let frame = read_frame(&mut gui_out).await.unwrap().expect("a frame");
            assert!(matches!(frame.parse::<DaemonMessage>().unwrap().unwrap(), DaemonMessage::Panes { .. }));
            // The GUI going away ends the bridge without an error; the daemon stays.
            drop(gui_in);
            assert!(bridge.await.unwrap().is_ok());
        });
    }
}
