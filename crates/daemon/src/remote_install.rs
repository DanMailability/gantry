//! The steps of the SSH install wizard, each one ssh command on the transport's attempt machinery: sign-in dialogs
//! through askpass, Cancel through the attempt handle, no step that hangs. Every function blocks.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use app_core::host_install::{Probe, UploadPath, parse_probe, probe_command, upload_path};
use app_core::host_state::{FailKind, classify_failure};
use protocol::frame::{Frame, read_frame, write_frame};
use protocol::message::{ClientMessage, PROTOCOL_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

use crate::askpass::{AskpassServer, AskpassState};
use crate::ssh::{
    Attempt, AttemptHandle, SshHostSpec, prepare, spawn_stderr_tail, ssh_args_for, timeout_without_questions,
};

/// How long a step may make no progress while no sign-in question waits.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// The idle limit; a chunk of stdin written restarts it.
    pub idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts { idle: Duration::from_secs(30) }
    }
}

const UPLOAD_IDLE: Duration = Duration::from_secs(60);
const CLEANUP_IDLE: Duration = Duration::from_secs(10);
const STOP_IDLE: Duration = Duration::from_secs(30);
const CHUNK: usize = 64 * 1024;

fn other(reason: impl Into<String>) -> FailKind {
    FailKind::Other { reason: reason.into() }
}

fn runtime() -> Result<tokio::runtime::Runtime, FailKind> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|err| other(err.to_string()))
}

/// A running ssh child with its sign-in server and its stderr tail.
struct Started {
    child: tokio::process::Child,
    state: Arc<AskpassState>,
    /// The sign-in socket lives as long as the step does.
    _askpass: Option<AskpassServer>,
    stderr: tokio::task::JoinHandle<String>,
    /// ssh may have asked a question (an interactive attempt with a working sign-in server).
    can_ask: bool,
}

async fn start(
    program: &str,
    spec: &SshHostSpec,
    attempt: Attempt,
    remote_command: &str,
    piped_stdin: bool,
) -> Result<Started, FailKind> {
    let interactive = matches!(attempt, Attempt::Interactive { .. });
    let mut command = tokio::process::Command::new(program);
    command
        .stdin(if piped_stdin { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let (askpass, state, background) = prepare(&mut command, spec, &attempt).await;
    command.args(ssh_args_for(spec, background, remote_command));
    let mut child = command.spawn().map_err(|err| other(format!("could not run {program}: {err}")))?;
    let Some(stderr) = child.stderr.take() else { return Err(other("ssh has no pipes")) };
    Ok(Started {
        child,
        state,
        _askpass: askpass,
        stderr: spawn_stderr_tail(stderr),
        can_ask: interactive && !background,
    })
}

/// Why an attempt was ended from outside.
enum Abort {
    Cancelled,
    TimedOut,
}

impl Abort {
    fn kind(&self, idle: Duration) -> FailKind {
        match self {
            Abort::Cancelled => FailKind::NeedsInput,
            Abort::TimedOut => other(format!("timed out: no progress for {} s", idle.as_secs())),
        }
    }
}

/// What ssh's end says about a failed step: its exit code and last words.
async fn failure(started: Started, code: Option<i32>) -> FailKind {
    if started.state.cancelled() {
        return FailKind::NeedsInput;
    }
    let stderr = tokio::time::timeout(Duration::from_secs(2), started.stderr).await;
    let stderr = stderr.ok().and_then(Result::ok).unwrap_or_default();
    let unasked = started.can_ask && !started.state.asked();
    classify_failure(code, &stderr, None, unasked, None)
}

/// Run `remote_command` (text for the remote shell) through `program` (normally `ssh`) and return its stdout. `stdin`
/// is sent in 64 KiB chunks, `on_progress` gets the total after each; the step fails after `timeouts.idle` without
/// progress while no question waits, and `handle.cancel()` ends it as `NeedsInput`. A non-zero exit is classified as
/// the transport does, so `Other` carries ssh's or the remote's last words.
#[allow(clippy::too_many_arguments)]
pub fn run_remote(
    program: &str,
    spec: &SshHostSpec,
    attempt: Attempt,
    handle: &AttemptHandle,
    remote_command: &str,
    stdin: Option<Box<dyn Read + Send>>,
    on_progress: &mut dyn FnMut(u64),
    timeouts: Timeouts,
) -> Result<String, FailKind> {
    runtime()?.block_on(async {
        let mut started = start(program, spec, attempt, remote_command, stdin.is_some()).await?;
        let pipe = started.child.stdin.take();
        let Some(mut stdout) = started.child.stdout.take() else { return Err(other("ssh has no pipes")) };
        let state = started.state.clone();
        let activity = Notify::new();
        let outcome = {
            let work = async {
                let feed = async {
                    if let (Some(mut pipe), Some(mut input)) = (pipe, stdin) {
                        let mut buffer = vec![0u8; CHUNK];
                        let mut sent = 0u64;
                        loop {
                            let read = match input.read(&mut buffer) {
                                Ok(0) => break,
                                Ok(read) => read,
                                Err(err) => return Err(format!("could not read the file: {err}")),
                            };
                            // A closed pipe means the remote command ended early: its exit status says why.
                            if pipe.write_all(&buffer[..read]).await.is_err() {
                                break;
                            }
                            sent += read as u64;
                            on_progress(sent);
                            activity.notify_one();
                        }
                        let _ = pipe.shutdown().await;
                    }
                    Ok(())
                };
                let read_out = async {
                    let mut out = Vec::new();
                    let _ = stdout.read_to_end(&mut out).await;
                    out
                };
                let (fed, out) = tokio::join!(feed, read_out);
                (fed, out, started.child.wait().await)
            };
            let idle = async {
                loop {
                    tokio::select! {
                        () = timeout_without_questions(&state, timeouts.idle) => return,
                        () = activity.notified() => {}
                    }
                }
            };
            tokio::select! {
                done = work => Ok(done),
                () = handle.cancel.notified() => Err(Abort::Cancelled),
                () = state.wait_cancelled() => Err(Abort::Cancelled),
                () = idle => Err(Abort::TimedOut),
            }
        };
        match outcome {
            Err(abort) => {
                let _ = started.child.start_kill();
                Err(abort.kind(timeouts.idle))
            }
            Ok((fed, out, status)) => {
                if let Err(reason) = fed {
                    let _ = started.child.start_kill();
                    return Err(other(reason));
                }
                match status {
                    Ok(status) if status.success() => Ok(String::from_utf8_lossy(&out).into_owned()),
                    Ok(status) => Err(failure(started, status.code()).await),
                    Err(err) => Err(other(err.to_string())),
                }
            }
        }
    })
}

/// Run one short command without input or progress.
fn run_short(
    program: &str,
    spec: &SshHostSpec,
    attempt: Attempt,
    handle: &AttemptHandle,
    remote_command: &str,
) -> Result<String, FailKind> {
    run_remote(program, spec, attempt, handle, remote_command, None, &mut |_| {}, Timeouts::default())
}

/// Look at the remote: OS, home, Gantry's version and lingering.
pub fn probe(program: &str, spec: &SshHostSpec, attempt: Attempt, handle: &AttemptHandle) -> Result<Probe, FailKind> {
    let stdout = run_short(program, spec, attempt, handle, &probe_command(&spec.command))?;
    parse_probe(&stdout).map_err(other)
}

/// Send the file `local` to the host's Gantry path, through `<path>.new` so a cut upload never replaces a working
/// binary; a failed or cancelled upload removes `<path>.new` (best effort). `on_progress` gets the bytes sent.
pub fn upload(
    program: &str,
    spec: &SshHostSpec,
    attempt: Attempt,
    handle: &AttemptHandle,
    local: &Path,
    on_progress: &mut dyn FnMut(u64),
) -> Result<(), FailKind> {
    let UploadPath { dir, file } = upload_path(&spec.command).map_err(other)?;
    let input =
        std::fs::File::open(local).map_err(|err| other(format!("could not open {}: {err}", local.display())))?;
    let command = format!("mkdir -p {dir} && cat > {file}.new && chmod 755 {file}.new && mv -f {file}.new {file}");
    let result = run_remote(
        program,
        spec,
        attempt,
        handle,
        &command,
        Some(Box::new(input)),
        on_progress,
        Timeouts { idle: UPLOAD_IDLE },
    );
    if result.is_err() {
        let _ = run_remote(
            program,
            spec,
            Attempt::Background,
            &AttemptHandle::default(),
            &format!("rm -f {file}.new"),
            None,
            &mut |_| {},
            Timeouts { idle: CLEANUP_IDLE },
        );
    }
    result.map(|_| ())
}

/// Run `<command> --version` on the host: its first line, or why it failed.
pub fn verify(program: &str, spec: &SshHostSpec, attempt: Attempt, handle: &AttemptHandle) -> Result<String, FailKind> {
    let stdout = run_short(program, spec, attempt, handle, &format!("{} --version", spec.command))?;
    Ok(stdout.lines().next().unwrap_or_default().trim().to_string())
}

/// Turn lingering on for the remote user; the error is what the remote said.
pub fn enable_linger(
    program: &str,
    spec: &SshHostSpec,
    attempt: Attempt,
    handle: &AttemptHandle,
) -> Result<(), String> {
    match run_short(program, spec, attempt, handle, "loginctl enable-linger \"$(id -un)\"") {
        Ok(_) => Ok(()),
        Err(FailKind::Other { reason }) => Err(reason),
        Err(FailKind::NotInstalled) => Err("loginctl was not found on the host".into()),
        Err(FailKind::NeedsInput) => Err("cancelled".into()),
        Err(FailKind::UpdateNeeded { .. }) => Err("unexpected answer".into()),
    }
}

/// Stop the remote daemon (and with it every session there) through `<command> bridge`: `Hello` and the frozen
/// `StopAll`, whatever protocol the daemon speaks, then wait for the bridge to end.
pub fn stop_remote_daemon(
    program: &str,
    spec: &SshHostSpec,
    attempt: Attempt,
    handle: &AttemptHandle,
) -> Result<(), FailKind> {
    runtime()?.block_on(async {
        let mut started = start(program, spec, attempt, &format!("{} bridge", spec.command), true).await?;
        let (Some(mut stdin), Some(mut stdout)) = (started.child.stdin.take(), started.child.stdout.take()) else {
            return Err(other("ssh has no pipes"));
        };
        let state = started.state.clone();
        let outcome = {
            let work = async {
                // A write error means the bridge is gone: the exit status below says why.
                let _ = write_frame(&mut stdin, &Frame::control(&ClientMessage::Hello { protocol: PROTOCOL_VERSION }))
                    .await;
                let _ = write_frame(&mut stdin, &Frame::control(&ClientMessage::StopAll)).await;
                let _ = stdin.flush().await;
                // The bridge only ends once its stdin does; the daemon has both frames by then, in order.
                drop(stdin);
                let mut answered = false;
                while let Ok(Some(_)) = read_frame(&mut stdout).await {
                    answered = true;
                }
                (answered, started.child.wait().await)
            };
            tokio::select! {
                done = work => Ok(done),
                () = handle.cancel.notified() => Err(Abort::Cancelled),
                () = state.wait_cancelled() => Err(Abort::Cancelled),
                () = timeout_without_questions(&state, STOP_IDLE) => Err(Abort::TimedOut),
            }
        };
        match outcome {
            Err(abort) => {
                let _ = started.child.start_kill();
                Err(abort.kind(STOP_IDLE))
            }
            Ok((true, _)) => Ok(()),
            Ok((false, Ok(status))) if status.success() => Ok(()),
            Ok((false, status)) => Err(failure(started, status.ok().and_then(|status| status.code())).await),
        }
    })
}
