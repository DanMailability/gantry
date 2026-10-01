//! Delivering an alert outside the window: a native notification and a short chime.

use app_core::split_tree::NodeId;

/// A notification to show for one pane.
pub struct Notice {
    /// The pane's leaf (focused when the notification is clicked).
    pub leaf: NodeId,
    /// First line.
    pub summary: String,
    /// Details.
    pub body: String,
}

/// Show `notice` on a thread of its own (it blocks until the notification closes). When the user clicks it,
/// `notice.leaf` is sent to `clicked` (Linux and Windows; macOS notifications have no click action here).
pub fn deliver(notice: Notice, clicked: async_channel::Sender<NodeId>) {
    std::thread::spawn(move || show(notice, clicked));
}

fn show(notice: Notice, clicked: async_channel::Sender<NodeId>) {
    let shown = notify_rust::Notification::new()
        .appname("ccm")
        .summary(&notice.summary)
        .body(&notice.body)
        .action("default", "Show")
        .show();
    let handle = match shown {
        Ok(handle) => handle,
        Err(err) => {
            tracing::warn!("showing a notification failed: {err}");
            return;
        }
    };
    #[cfg(not(target_os = "macos"))]
    handle.wait_for_action(|action| {
        if action == "default" {
            let _ = clicked.send_blocking(notice.leaf);
        }
    });
    #[cfg(target_os = "macos")]
    let _ = (handle, clicked);
}
