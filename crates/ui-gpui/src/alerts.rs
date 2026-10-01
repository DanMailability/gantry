//! Delivering an alert outside the window: a native notification and a short chime.

use std::time::Duration;

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

/// Show `notice` and play the chime, each on a thread of its own (both block). When the user clicks the notification,
/// `notice.leaf` is sent to `clicked` (Linux and Windows; macOS notifications have no click action here).
pub fn deliver(notice: Notice, clicked: async_channel::Sender<NodeId>) {
    std::thread::spawn(chime);
    std::thread::spawn(move || show(notice, clicked));
}

fn show(notice: Notice, clicked: async_channel::Sender<NodeId>) {
    let mut notification = notify_rust::Notification::new();
    notification.appname("Gantry").summary(&notice.summary).body(&notice.body).action("default", "Show");
    #[cfg(all(unix, not(target_os = "macos")))]
    notification.hint(notify_rust::Hint::DesktopEntry(crate::APP_ID.into()));
    let shown = notification.show();
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

/// Two quiet rising tones (E6, A6). Without an audio device it does nothing.
fn chime() {
    use rodio::Source;
    let mut sink = match rodio::DeviceSinkBuilder::open_default_sink() {
        Ok(sink) => sink,
        Err(err) => {
            tracing::debug!("no audio output for the chime: {err}");
            return;
        }
    };
    sink.log_on_drop(false);
    let player = rodio::Player::connect_new(sink.mixer());
    for frequency in [1318.5, 1760.0] {
        player.append(rodio::source::SineWave::new(frequency).take_duration(Duration::from_millis(110)).amplify(0.12));
    }
    player.sleep_until_end();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shows a real notification on this desktop and plays the chime. Run by hand:
    /// `cargo test -p ui-gpui alerts -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn a_notification_and_the_chime_reach_the_desktop() {
        std::thread::spawn(chime).join().unwrap();
        // Do not wait for a click: pass as soon as it is shown.
        let shown = notify_rust::Notification::new().appname("Gantry").summary("Gantry test").body("Delivered by the alerts module test").show();
        assert!(shown.is_ok(), "{:?}", shown.err());
    }
}
