//! Delivering an alert outside the window: a native notification and a built-in sound.

use std::num::NonZero;

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

const SAMPLE_RATE: u32 = 44_100;

/// The samples of `tone` (mono, 44.1 kHz), peaking at `0.3 * volume`.
fn tone_samples(tone: AlertTone, volume: f32) -> Vec<f32> {
    let (seconds, attack, tau) = match tone {
        AlertTone::Bell => (0.48_f32, 0.005_f32, 0.055_f32),
        AlertTone::Tick => (0.05, 0.002, 0.010),
    };
    let rate = SAMPLE_RATE as f32;
    let count = (seconds * rate) as usize;
    let fade = (0.010 * rate) as usize;
    let mut samples: Vec<f32> = (0..count)
        .map(|i| {
            let t = i as f32 / rate;
            let wave = match tone {
                AlertTone::Bell => 0.7 * (std::f32::consts::TAU * 495.0 * t).sin() + 0.3 * (std::f32::consts::TAU * 990.0 * t).sin(),
                AlertTone::Tick => (std::f32::consts::TAU * 900.0 * t).sin(),
            };
            let envelope = if t < attack { t / attack } else { (-t / tau).exp() };
            let tail = match tone {
                AlertTone::Bell if i + fade > count => (count - i) as f32 / fade as f32,
                _ => 1.0,
            };
            wave * envelope * tail
        })
        .collect();
    let peak = samples.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    if peak > 0.0 {
        let gain = volume * 0.3 / peak;
        samples.iter_mut().for_each(|sample| *sample *= gain);
    }
    samples
}

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
        let player = rodio::Player::connect_new(sink.mixer());
        let (channels, rate) = (NonZero::new(1).unwrap(), NonZero::new(SAMPLE_RATE).unwrap());
        player.append(rodio::buffer::SamplesBuffer::new(channels, rate, tone_samples(tone, volume)));
        player.sleep_until_end();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shows a real notification on this desktop and plays the bell, then the tick. Run by hand:
    /// `cargo test -p ui-gpui alerts -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn a_notification_and_the_tones_reach_the_desktop() {
        play(AlertTone::Bell, 0.4);
        std::thread::sleep(std::time::Duration::from_millis(700));
        play(AlertTone::Tick, 0.4);
        std::thread::sleep(std::time::Duration::from_millis(300));
        // Do not wait for a click: pass as soon as it is shown.
        let shown = notify_rust::Notification::new().appname("Gantry").summary("Gantry test").body("Delivered by the alerts module test").show();
        assert!(shown.is_ok(), "{:?}", shown.err());
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn at(seconds: f32) -> usize {
        (seconds * SAMPLE_RATE as f32) as usize
    }

    #[test]
    fn bell_is_about_half_a_second_and_decays() {
        let samples = tone_samples(AlertTone::Bell, 1.0);
        assert!((samples.len() as f32 / SAMPLE_RATE as f32 - 0.48).abs() < 0.01);
        let peak_at = samples.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
        assert!(peak_at < at(0.02), "peak at sample {peak_at}");
        let early = rms(&samples[..at(0.02)]);
        let late = rms(&samples[at(0.15)..at(0.17)]);
        assert!(20.0 * (early / late).log10() >= 12.0, "early {early}, late {late}");
        assert!(samples.last().unwrap().abs() < 1e-3);
    }

    #[test]
    fn tick_is_short() {
        let samples = tone_samples(AlertTone::Tick, 1.0);
        assert!(samples.len() <= at(0.06));
        assert!(samples[at(0.05).min(samples.len())..].iter().all(|x| x.abs() < 0.01));
    }

    #[test]
    fn volume_scales_peak() {
        for tone in [AlertTone::Bell, AlertTone::Tick] {
            for volume in [0.2_f32, 0.4, 1.0] {
                let peak = tone_samples(tone, volume).iter().fold(0.0_f32, |peak, x| peak.max(x.abs()));
                assert!(peak <= 0.3 * volume + 1e-4 && peak > 0.29 * volume, "{tone:?} at {volume}: {peak}");
            }
        }
    }
}
