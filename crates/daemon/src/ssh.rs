//! The SSH transport: `ssh -T … -- <target> <command> bridge` as the stream of a daemon connection.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use app_core::host_state::{FailKind, classify_failure};
use tokio::io::AsyncReadExt;
use tokio::sync::Notify;

use crate::askpass::{AskPrompt, AskpassServer, AskpassState};
use crate::client::{ClientEvent, Connection, Hello, Link, Outgoing, StartError, connection, run_stream};

/// Where to connect: what follows `ssh`, and the Gantry command on the remote.
#[derive(Clone, Debug)]
pub struct SshHostSpec {
    /// The host's name in `sshHosts`, for the sign-in dialogs.
    pub name: String,
    /// An alias from `~/.ssh/config` or `user@host`.
    pub target: String,
    /// The remote Gantry binary, as the remote shell will run it.
    pub command: String,
}

/// How long the handshake (`Welcome` and `Panes`) may take once ssh runs.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Lets the caller end an attempt that is still connecting.
#[derive(Clone, Default)]
pub struct AttemptHandle {
    cancel: Arc<Notify>,
}

impl AttemptHandle {
    /// Kill the attempt's ssh; `connect` then fails with `NeedsInput`. No effect once the handshake is done.
    pub fn cancel(&self) {
        self.cancel.notify_one();
    }
}

/// How an attempt may talk to the user.
pub enum Attempt {
    /// The user asked: ssh may prompt, through dialogs.
    Interactive {
        /// Where the questions go.
        prompts: async_channel::Sender<AskPrompt>,
        /// The profile's runtime dir (0700), where the sign-in socket is made.
        runtime_dir: PathBuf,
    },
    /// A reconnect retry: ssh must never prompt (it fails instead).
    Background,
}

/// The arguments of `ssh` for `spec`. The target comes after `--` so it can never be read as an option.
pub fn ssh_args(spec: &SshHostSpec, background: bool) -> Vec<String> {
    let mut args: Vec<String> =
        ["-T", "-o", "ConnectTimeout=20", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"]
            .into_iter()
            .map(String::from)
            .collect();
    if background {
        args.extend(["-o".into(), "BatchMode=yes".into()]);
    }
    args.extend(["--".into(), spec.target.clone(), format!("{} bridge", spec.command)]);
    args
}

/// Connect to the remote daemon through `program` (normally `ssh`). Blocks until the handshake is done or failed;
/// the returned connection keeps running on its own thread. `handle.cancel()` ends the attempt; a handshake that
/// takes longer than `handshake_timeout` fails it.
pub fn connect(
    program: &str,
    spec: &SshHostSpec,
    attempt: Attempt,
    handle: &AttemptHandle,
    handshake_timeout: Duration,
) -> Result<Connection, FailKind> {
    let Link { client, outgoing, events_tx, events } = Link::new();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<Hello, FailKind>>();
    let (program, spec, handle) = (program.to_string(), spec.clone(), handle.clone());
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(err) => {
                let _ = ready_tx.send(Err(FailKind::Other { reason: err.to_string() }));
                return;
            }
        };
        runtime.block_on(run(program, spec, attempt, handle, handshake_timeout, ready_tx, outgoing, events_tx));
    });
    match ready_rx.recv() {
        Ok(Ok(hello)) => Ok(connection(client, hello, events)),
        Ok(Err(kind)) => Err(kind),
        Err(_) => Err(FailKind::Other { reason: "the ssh thread ended".into() }),
    }
}

/// Why an attempt was ended from outside, before its handshake finished.
enum Abort {
    Cancelled,
    TimedOut,
}

#[allow(clippy::too_many_arguments)]
async fn run(
    program: String,
    spec: SshHostSpec,
    attempt: Attempt,
    handle: AttemptHandle,
    handshake_timeout: Duration,
    ready: std::sync::mpsc::Sender<Result<Hello, FailKind>>,
    outgoing: tokio::sync::mpsc::UnboundedReceiver<Outgoing>,
    events: async_channel::Sender<ClientEvent>,
) {
    let fail = |kind: FailKind| {
        let _ = ready.send(Err(kind));
    };
    let interactive = matches!(attempt, Attempt::Interactive { .. });
    let mut background = !interactive;
    let mut command = tokio::process::Command::new(&program);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let state = Arc::new(AskpassState::default());
    // Dropping the server removes the sign-in socket: it goes right after the handshake.
    let mut askpass: Option<AskpassServer> = None;
    if let Attempt::Interactive { prompts, runtime_dir } = &attempt {
        match AskpassServer::start(runtime_dir, &spec.name, prompts.clone(), state.clone()).await {
            Ok(server) => {
                if let Ok(exe) = std::env::current_exe() {
                    command
                        .env("SSH_ASKPASS", exe)
                        .env("SSH_ASKPASS_REQUIRE", "force")
                        .env("GANTRY_ASKPASS_SOCKET", server.path());
                    askpass = Some(server);
                } else {
                    background = true;
                }
            }
            // No sign-in dialogs here (Windows): keys and the agent only.
            Err(_) => background = true,
        }
    }
    if background {
        command.env_remove("SSH_ASKPASS").env_remove("SSH_ASKPASS_REQUIRE");
    }
    command.args(ssh_args(&spec, background));
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return fail(FailKind::Other { reason: format!("could not run {program}: {err}") }),
    };
    let (stdin, stdout, mut stderr) = match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
        (Some(stdin), Some(stdout), Some(stderr)) => (stdin, stdout, stderr),
        _ => return fail(FailKind::Other { reason: "ssh has no pipes".into() }),
    };
    let stderr_text = tokio::spawn(async move {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text).await;
        // Only the tail matters: the last words of ssh.
        let mut keep = text.len().saturating_sub(4096);
        while !text.is_char_boundary(keep) {
            keep += 1;
        }
        text[keep..].to_string()
    });
    let handshaken = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hello_ready = ready.clone();
    let done = handshaken.clone();
    let outcome = {
        let stream = run_stream(stdout, stdin, outgoing, events, move |hello| {
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            // The sign-in socket is only for the handshake.
            drop(askpass);
            let _ = hello_ready.send(Ok(hello));
        });
        // Only the handshake can be cancelled or time out; once it is done this never resolves.
        let abort = async {
            let reason = tokio::select! {
                () = handle.cancel.notified() => Abort::Cancelled,
                () = state.wait_cancelled() => Abort::Cancelled,
                () = tokio::time::sleep(handshake_timeout) => Abort::TimedOut,
            };
            if handshaken.load(std::sync::atomic::Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            reason
        };
        tokio::select! {
            outcome = stream => Ok(outcome),
            reason = abort => Err(reason),
        }
    };
    match outcome {
        Err(reason) => {
            let _ = child.start_kill();
            fail(match reason {
                Abort::Cancelled => FailKind::NeedsInput,
                Abort::TimedOut => FailKind::Other { reason: "timed out waiting for Gantry on the host".into() },
            });
        }
        Ok(Ok(())) => {
            let _ = child.start_kill();
        }
        Ok(Err(StartError::VersionMismatch { daemon_protocol })) => {
            let _ = child.start_kill();
            fail(classify_failure(None, "", Some(daemon_protocol), false, None));
        }
        Ok(Err(StartError::Io(error))) => {
            if state.cancelled() {
                let _ = child.start_kill();
                return fail(FailKind::NeedsInput);
            }
            // Let ssh say why it ended (its exit code and last words), but do not wait for long.
            let code = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(status)) => status.code(),
                _ => {
                    let _ = child.start_kill();
                    None
                }
            };
            let stderr = tokio::time::timeout(Duration::from_secs(2), stderr_text).await;
            let stderr = stderr.ok().and_then(Result::ok).unwrap_or_default();
            let unasked = interactive && !background && !state.asked();
            fail(classify_failure(code, &stderr, None, unasked, Some(&format!("{error:#}"))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SshHostSpec {
        SshHostSpec { name: "devbox".into(), target: "me@devbox".into(), command: "~/.local/bin/gantry".into() }
    }

    #[test]
    fn the_command_line_keeps_the_target_after_the_double_dash_and_batch_mode_for_retries() {
        let interactive = ssh_args(&spec(), false);
        assert_eq!(
            interactive,
            [
                "-T",
                "-o",
                "ConnectTimeout=20",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "--",
                "me@devbox",
                "~/.local/bin/gantry bridge"
            ]
        );
        let background = ssh_args(&spec(), true);
        assert!(background.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]), "{background:?}");
        let dash = background.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(&background[dash + 1..], ["me@devbox", "~/.local/bin/gantry bridge"]);
    }
}
