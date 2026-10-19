//! The wizard's background steps: each one runs `daemon::remote_install` on a thread of its own (they block) and
//! reports through a channel the view reads.

use std::path::PathBuf;

use app_core::host_install::Probe;
use app_core::host_state::FailKind;
use daemon::askpass::AskPrompt;
use daemon::remote_install;
use daemon::ssh::{Attempt, AttemptHandle, SshHostSpec};

/// What a running step reports.
pub(crate) enum Msg {
    /// The probe ended.
    Probed(Result<Probe, FailKind>),
    /// Bytes of the upload sent so far.
    Progress(u64),
    /// Upload and verify ended: the version line the remote printed.
    Installed(Result<String, FailKind>),
    /// `loginctl enable-linger` ended.
    LingerDone(Result<(), String>),
    /// The remote daemon was stopped.
    Stopped(Result<(), FailKind>),
}

/// A running step: its cancel handle and its messages.
pub(crate) struct Job {
    pub(crate) handle: AttemptHandle,
    pub(crate) messages: async_channel::Receiver<Msg>,
}

/// Where a step's sign-in questions go.
#[derive(Clone)]
pub(crate) struct Questions {
    pub(crate) prompts: async_channel::Sender<AskPrompt>,
    pub(crate) runtime_dir: PathBuf,
}

impl Questions {
    fn attempt(&self) -> Attempt {
        Attempt::Interactive { prompts: self.prompts.clone(), runtime_dir: self.runtime_dir.clone() }
    }
}

fn spawn(work: impl FnOnce(&AttemptHandle, &async_channel::Sender<Msg>) + Send + 'static) -> Job {
    let handle = AttemptHandle::default();
    let (tx, messages) = async_channel::unbounded();
    let thread_handle = handle.clone();
    std::thread::spawn(move || work(&thread_handle, &tx));
    Job { handle, messages }
}

/// Look at the remote.
pub(crate) fn probe(spec: SshHostSpec, questions: &Questions) -> Job {
    let questions = questions.clone();
    spawn(move |handle, tx| {
        let result = remote_install::probe("ssh", &spec, questions.attempt(), handle);
        let _ = tx.send_blocking(Msg::Probed(result));
    })
}

/// Upload this binary, then ask the remote for its version.
pub(crate) fn install(spec: SshHostSpec, questions: &Questions, local: PathBuf) -> Job {
    let questions = questions.clone();
    spawn(move |handle, tx| {
        let progress = tx.clone();
        let uploaded = remote_install::upload("ssh", &spec, questions.attempt(), handle, &local, &mut |sent| {
            let _ = progress.send_blocking(Msg::Progress(sent));
        });
        let result = uploaded.and_then(|()| remote_install::verify("ssh", &spec, questions.attempt(), handle));
        let _ = tx.send_blocking(Msg::Installed(result));
    })
}

/// Turn lingering on for the remote user.
pub(crate) fn linger(spec: SshHostSpec, questions: &Questions) -> Job {
    let questions = questions.clone();
    spawn(move |handle, tx| {
        let result = remote_install::enable_linger("ssh", &spec, questions.attempt(), handle);
        let _ = tx.send_blocking(Msg::LingerDone(result));
    })
}

/// Stop the remote daemon.
pub(crate) fn stop(spec: SshHostSpec, questions: &Questions) -> Job {
    let questions = questions.clone();
    spawn(move |handle, tx| {
        let result = remote_install::stop_remote_daemon("ssh", &spec, questions.attempt(), handle);
        let _ = tx.send_blocking(Msg::Stopped(result));
    })
}
