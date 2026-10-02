//! Delivering an alert outside the window: a native notification and a built-in sound.

use app_core::attention::Channels;
use app_core::settings::AlertTone;
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

/// Show `notice` and play the sound as `channels` says, each on a thread of its own (both block). The sound plays at
/// `volume`. When the user clicks the notification, `notice.leaf` is sent to `clicked` (Linux and Windows; macOS
/// notifications have no click action here).
pub fn deliver(notice: Notice, channels: Channels, volume: f32, clicked: async_channel::Sender<NodeId>) {
    if let Some(tone) = channels.sound {
        play(tone, volume);
    }
    if channels.notification {
        std::thread::spawn(move || show(notice, clicked));
    }
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

/// The embedded recording of `tone`.
fn recording(tone: AlertTone) -> &'static [u8] {
    match tone {
        AlertTone::ClassicBlip => include_bytes!("../assets/sounds/classic-blip.wav"),
        AlertTone::GentleRise => include_bytes!("../assets/sounds/gentle-rise.wav"),
        AlertTone::SoftBloop => include_bytes!("../assets/sounds/soft-bloop.wav"),
        AlertTone::SoftDrop => include_bytes!("../assets/sounds/soft-drop.wav"),
        AlertTone::FallingBlip => include_bytes!("../assets/sounds/falling-blip.wav"),
        AlertTone::SoftFall => include_bytes!("../assets/sounds/soft-fall.wav"),
        AlertTone::SoftFlutter => include_bytes!("../assets/sounds/soft-flutter.wav"),
        AlertTone::WarmHum => include_bytes!("../assets/sounds/warm-hum.wav"),
    }
}

/// Silence before every sound: a device that wakes late swallows this instead of the start of a short blip.
const LEAD_IN: std::time::Duration = std::time::Duration::from_millis(100);

/// Play `tone` at `volume` (0 to 1) on a thread of its own. Without an audio device it does nothing.
pub fn play(tone: AlertTone, volume: f32) {
    std::thread::spawn(move || {
        let mut sink = match rodio::DeviceSinkBuilder::open_default_sink() {
            Ok(sink) => sink,
            Err(err) => {
                tracing::debug!("no audio output for the alert sound: {err}");
                return;
            }
        };
        sink.log_on_drop(false);
        let source = match rodio::Decoder::new(std::io::Cursor::new(recording(tone))) {
            Ok(source) => source,
            Err(err) => {
                tracing::warn!("decoding the alert sound {tone:?} failed: {err}");
                return;
            }
        };
        let player = rodio::Player::connect_new(sink.mixer());
        player.set_volume(volume);
        player.append(rodio::Source::delay(source, LEAD_IN));
        player.sleep_until_end();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shows a real notification and plays every tone in turn. Run by hand:
    /// `cargo test -p ui-gpui alerts -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn a_notification_and_the_tones_reach_the_desktop() {
        for tone in AlertTone::ALL {
            play(tone, 0.4);
            std::thread::sleep(std::time::Duration::from_millis(900));
        }
        // Do not wait for a click: pass as soon as it is shown.
        let shown = notify_rust::Notification::new().appname("Gantry").summary("Gantry test").body("Delivered by the alerts module test").show();
        assert!(shown.is_ok(), "{:?}", shown.err());
    }

    #[test]
    fn every_tone_is_a_decodable_recording() {
        for tone in AlertTone::ALL {
            let source = rodio::Decoder::new(std::io::Cursor::new(recording(tone))).unwrap_or_else(|err| panic!("{tone:?}: {err}"));
            assert!(source.count() > 0, "{tone:?}");
        }
    }
}
