//! The SSH transport end to end with a fake `ssh` that runs `gantry bridge` locally.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use app_core::host_state::FailKind;
use daemon::askpass::{AskAnswer, AskPrompt};
use daemon::client::ClientEvent;
use daemon::ssh::{Attempt, AttemptHandle, HANDSHAKE_TIMEOUT, SshHostSpec, connect as connect_with};
use protocol::message::SpawnRequest;

const GANTRY: &str = env!("CARGO_BIN_EXE_gantry");

/// `connect` with the real handshake timeout and nobody cancelling.
fn connect(program: &str, spec: &SshHostSpec, attempt: Attempt) -> Result<daemon::client::Connection, FailKind> {
    connect_with(program, spec, attempt, &AttemptHandle::default(), HANDSHAKE_TIMEOUT)
}

/// What the fake ssh does, besides running the bridge.
#[derive(Default)]
struct Fake {
    /// Ask this through the askpass helper first (when ssh was given one); a cancel is what real ssh prints, exit 255.
    ask: Option<&'static str>,
    /// Fail with this exit code and message instead of running the bridge.
    fail: Option<(i32, &'static str)>,
    /// Run this shell text instead of the bridge (after the asking above); `$GANTRY` is the gantry binary.
    body: Option<String>,
}

struct Env {
    dir: tempfile::TempDir,
    fake: std::path::PathBuf,
}

impl Env {
    fn path(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }
}

/// A fake ssh script. The workspace forbids `unsafe`, so the tests cannot set environment variables: the script
/// exports the isolated environment itself (runtime dir, HOME, profile) before it runs `gantry bridge`. It ignores
/// ssh's arguments, records whether it was given `SSH_ASKPASS`, and runs `gantry askpass` itself (the transport sets
/// `SSH_ASKPASS` to the test binary, which is not `gantry`).
fn setup(profile: &str, fake: Fake) -> Env {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("run")).unwrap();
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    let root = dir.path().display();
    let mut script = format!(
        "#!/bin/sh\nexport XDG_RUNTIME_DIR='{root}/run' HOME='{root}/home' SHELL=/bin/sh GANTRY_PROFILE='{profile}'\n\
         printf '%s' \"$SSH_ASKPASS\" > '{root}/askpass-env'\n"
    );
    if let Some(prompt) = fake.ask {
        let prompt = prompt.replace('\'', "'\\''");
        script.push_str(&format!(
            "if [ -n \"$SSH_ASKPASS\" ]; then\n  answer=$('{GANTRY}' askpass '{prompt}') || \
             {{ echo 'Permission denied (publickey,password).' >&2; exit 255; }}\n  printf '%s' \"$answer\" > '{root}/answer'\nfi\n"
        ));
    }
    if let Some(body) = fake.body {
        script.push_str(&format!("GANTRY='{GANTRY}'\n{body}\n"));
    } else if let Some((code, message)) = fake.fail {
        script.push_str(&format!("echo '{message}' >&2\nexit {code}\n"));
    } else {
        script.push_str(&format!("exec '{GANTRY}' bridge\n"));
    }
    let path = dir.path().join("fake-ssh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    Env { dir, fake: path }
}

fn spec() -> SshHostSpec {
    SshHostSpec { name: "devbox-name".into(), target: "devbox".into(), command: "gantry".into() }
}

fn sh(script: &str) -> SpawnRequest {
    SpawnRequest {
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: std::env::temp_dir(),
        cols: 80,
        rows: 24,
        cwd_from: None,
        container: None,
        claude: false,
        env: Vec::new(),
        scrollback: None,
    }
}

fn output_until(events: &async_channel::Receiver<ClientEvent>, needle: &str) -> String {
    let mut text = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !text.contains(needle) {
        assert!(Instant::now() < deadline, "timed out waiting for {needle:?}; got {text:?}");
        if let Ok(ClientEvent::Output { bytes, .. }) = events.recv_blocking() {
            text.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    text
}

fn stop_daemon(connection: &daemon::client::Connection) {
    connection.client.stop_all_and_wait(Duration::from_secs(3));
}

#[test]
fn a_pane_works_through_the_bridge_and_survives_the_connection() {
    let env = setup("sshbridge1", Fake::default());
    let first = connect(env.fake.to_str().unwrap(), &spec(), Attempt::Background).unwrap();
    assert!(!first.instance.is_empty());
    first.client.spawn(1, sh("printf through-ssh; sleep 30"));
    output_until(&first.events, "through-ssh");
    let instance = first.instance.clone();
    // Dropping the connection ends ssh and the bridge; the persistent daemon and its pane stay.
    drop(first);
    std::thread::sleep(Duration::from_millis(500));
    let second = connect(env.fake.to_str().unwrap(), &spec(), Attempt::Background).unwrap();
    assert_eq!(second.instance, instance, "the same daemon");
    assert_eq!(second.panes.len(), 1, "the pane is still there");
    // Attaching replays the screen: the earlier text arrives again.
    second.client.attach(second.panes[0].pane);
    output_until(&second.events, "through-ssh");
    stop_daemon(&second);
}

#[test]
fn an_interactive_attempt_asks_through_askpass_and_gets_the_answer() {
    let env = setup("sshbridge2", Fake { ask: Some("me@devbox's password: "), ..Fake::default() });
    let (prompts_tx, prompts_rx) = async_channel::unbounded::<AskPrompt>();
    let responder = std::thread::spawn(move || {
        let prompt = prompts_rx.recv_blocking().unwrap();
        assert_eq!(prompt.host, "devbox-name", "the dialog names the host, not the ssh target");
        assert_eq!(prompt.prompt, "me@devbox's password: ");
        prompt.reply.send_blocking(AskAnswer::Answer("hunter2".into())).unwrap();
    });
    let attempt = Attempt::Interactive { prompts: prompts_tx, runtime_dir: env.path("run") };
    let connection = connect(env.fake.to_str().unwrap(), &spec(), attempt).unwrap();
    responder.join().unwrap();
    assert_eq!(std::fs::read_to_string(env.path("answer")).unwrap(), "hunter2");
    assert!(!std::fs::read_to_string(env.path("askpass-env")).unwrap().is_empty(), "ssh was given the helper");
    stop_daemon(&connection);
}

#[test]
fn a_cancelled_sign_in_ends_the_attempt_as_needs_input() {
    let env = setup("sshbridge3", Fake { ask: Some("Password: "), ..Fake::default() });
    let (prompts_tx, prompts_rx) = async_channel::unbounded::<AskPrompt>();
    std::thread::spawn(move || {
        let prompt = prompts_rx.recv_blocking().unwrap();
        prompt.reply.send_blocking(AskAnswer::Cancel).unwrap();
    });
    let attempt = Attempt::Interactive { prompts: prompts_tx, runtime_dir: env.path("run") };
    let started = Instant::now();
    let error = connect(env.fake.to_str().unwrap(), &spec(), attempt).err().expect("the attempt fails");
    assert_eq!(error, FailKind::NeedsInput);
    assert!(started.elapsed() < Duration::from_secs(10), "no hang");
    // The sign-in socket was removed with the attempt.
    let leftovers = std::fs::read_dir(env.path("run"))
        .unwrap()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with("askpass-"));
    assert!(!leftovers, "the askpass socket is cleaned up");
}

#[test]
fn a_background_attempt_never_gets_the_askpass_helper() {
    let fake =
        Fake { ask: Some("Password: "), fail: Some((255, "me@devbox: Permission denied (publickey).")), body: None };
    let env = setup("sshbridge4", fake);
    let error = connect(env.fake.to_str().unwrap(), &spec(), Attempt::Background).err().expect("fails");
    assert_eq!(error, FailKind::NeedsInput);
    // ssh got no SSH_ASKPASS, so the fake never asked.
    assert_eq!(std::fs::read_to_string(env.path("askpass-env")).unwrap(), "");
    assert!(!env.path("answer").exists());
}

#[test]
fn a_missing_binary_is_not_installed_and_a_dead_ssh_is_a_reason() {
    let env =
        setup("sshbridge5", Fake { fail: Some((127, "bash: line 1: gantry: command not found")), ..Fake::default() });
    let error = connect(env.fake.to_str().unwrap(), &spec(), Attempt::Background).err().unwrap();
    assert_eq!(error, FailKind::NotInstalled);
    let refused =
        Fake { fail: Some((255, "ssh: connect to host devbox port 22: Connection refused")), ..Fake::default() };
    let env = setup("sshbridge5", refused);
    match connect(env.fake.to_str().unwrap(), &spec(), Attempt::Background).err().unwrap() {
        FailKind::Other { reason } => assert!(reason.contains("Connection refused"), "{reason}"),
        other => panic!("{other:?}"),
    }
    // A program that does not exist.
    match connect("/no/such/ssh", &spec(), Attempt::Background).err().unwrap() {
        FailKind::Other { reason } => assert!(reason.contains("/no/such/ssh"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_second_question_after_a_cancel_never_reaches_the_dialog_and_ssh_is_killed() {
    // Real ssh asks again after a failed helper (an empty password, up to three times) and does not exit by itself.
    let body = "\"$GANTRY\" askpass 'Password: ' >/dev/null\n\"$GANTRY\" askpass 'Password: ' >/dev/null\n\
                touch \"$(dirname \"$0\")/asked-twice\"\nsleep 60";
    let env = setup("sshbridge6", Fake { body: Some(body.into()), ..Fake::default() });
    let (prompts_tx, prompts_rx) = async_channel::unbounded::<AskPrompt>();
    let responder = std::thread::spawn(move || {
        let mut asked = 0;
        while let Ok(prompt) = prompts_rx.recv_blocking() {
            asked += 1;
            let _ = prompt.reply.send_blocking(AskAnswer::Cancel);
        }
        asked
    });
    let attempt = Attempt::Interactive { prompts: prompts_tx, runtime_dir: env.path("run") };
    let started = Instant::now();
    let error = connect(env.fake.to_str().unwrap(), &spec(), attempt).err().expect("the attempt fails");
    assert_eq!(error, FailKind::NeedsInput);
    assert!(started.elapsed() < Duration::from_secs(10), "ssh was killed instead of sleeping 60 s");
    assert_eq!(responder.join().unwrap(), 1, "only the first question was ever asked");
}

#[test]
fn a_handshake_that_never_finishes_times_out_with_a_reason() {
    let env = setup("sshbridge7", Fake { body: Some("sleep 60".into()), ..Fake::default() });
    let started = Instant::now();
    let error = connect_with(
        env.fake.to_str().unwrap(),
        &spec(),
        Attempt::Background,
        &AttemptHandle::default(),
        Duration::from_millis(500),
    )
    .err()
    .expect("times out");
    assert_eq!(error, FailKind::Other { reason: "timed out waiting for Gantry on the host".into() });
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn cancelling_a_connecting_attempt_kills_ssh_and_ends_it() {
    let env = setup("sshbridge8", Fake { body: Some("sleep 60".into()), ..Fake::default() });
    let handle = AttemptHandle::default();
    let canceller = handle.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        canceller.cancel();
    });
    let started = Instant::now();
    let error = connect_with(env.fake.to_str().unwrap(), &spec(), Attempt::Background, &handle, HANDSHAKE_TIMEOUT)
        .err()
        .unwrap();
    assert_eq!(error, FailKind::NeedsInput);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_remote_daemon_with_another_protocol_is_update_needed() {
    let body = "cat \"$(dirname \"$0\")/mismatch.bin\"\nsleep 5";
    let env = setup("sshbridge9", Fake { body: Some(body.into()), ..Fake::default() });
    let mut frame = Vec::new();
    let message = protocol::message::DaemonMessage::VersionMismatch { daemon_protocol: 99 };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(protocol::frame::write_frame(&mut frame, &protocol::frame::Frame::control(&message))).unwrap();
    std::fs::write(env.path("mismatch.bin"), frame).unwrap();
    let error = connect(env.fake.to_str().unwrap(), &spec(), Attempt::Background).err().expect("fails");
    assert_eq!(error, FailKind::UpdateNeeded { remote: Some("protocol 99".into()) });
}

#[test]
fn an_interactive_attempt_that_is_refused_without_a_question_says_authentication_failed() {
    let fake = Fake { fail: Some((255, "me@devbox: Permission denied (publickey).")), ..Fake::default() };
    let env = setup("sshbridge10", fake);
    let (prompts_tx, _prompts_rx) = async_channel::unbounded::<AskPrompt>();
    let attempt = Attempt::Interactive { prompts: prompts_tx, runtime_dir: env.path("run") };
    match connect(env.fake.to_str().unwrap(), &spec(), attempt).err().unwrap() {
        FailKind::Other { reason } => {
            assert!(reason.starts_with("Authentication failed (ssh asked for nothing)"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_handshake_timeout_waits_while_a_question_is_open() {
    let env = setup("sshbridge11", Fake { ask: Some("me@devbox's password: "), ..Fake::default() });
    let (prompts_tx, prompts_rx) = async_channel::unbounded::<AskPrompt>();
    std::thread::spawn(move || {
        let prompt = prompts_rx.recv_blocking().unwrap();
        // The user takes longer than the 1 s handshake timeout.
        std::thread::sleep(Duration::from_secs(2));
        prompt.reply.send_blocking(AskAnswer::Answer("hunter2".into())).unwrap();
    });
    let attempt = Attempt::Interactive { prompts: prompts_tx, runtime_dir: env.path("run") };
    let connection =
        connect_with(env.fake.to_str().unwrap(), &spec(), attempt, &AttemptHandle::default(), Duration::from_secs(1))
            .expect("the time spent answering does not count");
    // The sign-in socket is gone once the handshake is done.
    let leftovers = std::fs::read_dir(env.path("run"))
        .unwrap()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with("askpass-"));
    assert!(!leftovers, "the askpass socket is removed after a successful handshake");
    stop_daemon(&connection);
}

#[test]
fn an_interactive_attempt_that_asks_nothing_still_times_out() {
    let env = setup("sshbridge12", Fake { body: Some("sleep 60".into()), ..Fake::default() });
    let (prompts_tx, _prompts_rx) = async_channel::unbounded::<AskPrompt>();
    let attempt = Attempt::Interactive { prompts: prompts_tx, runtime_dir: env.path("run") };
    let error = connect_with(
        env.fake.to_str().unwrap(),
        &spec(),
        attempt,
        &AttemptHandle::default(),
        Duration::from_millis(500),
    )
    .err()
    .expect("times out");
    assert_eq!(error, FailKind::Other { reason: "timed out waiting for Gantry on the host".into() });
}
