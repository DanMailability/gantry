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

/// One rounded sine note of a tone: a raised-cosine attack, a flat hold, a decay that starts with zero slope, an eased pitch
/// glide and a cosine fade-out, with faint harmonics and a slightly detuned twin for body.
struct Note {
    start: f32,
    seconds: f32,
    from_hz: f32,
    to_hz: f32,
    attack: f32,
    hold: f32,
    decay: f32,
    fade: f32,
    /// Weight and time constant of a second, slower decay (the long ring of a beep).
    slow: Option<(f32, f32)>,
}

const fn note(start: f32, seconds: f32, from_hz: f32, to_hz: f32, attack: f32, hold: f32, decay: f32, fade: f32) -> Note {
    Note { start, seconds, from_hz, to_hz, attack, hold, decay, fade, slow: None }
}

const RING: Option<(f32, f32)> = Some((0.2, 0.14));

/// The notes of `tone` and its level against the loudest tone (the beeps are quieter than the blips, as in the desktop's own sounds).
fn voice(tone: AlertTone) -> (f32, Vec<Note>) {
    const SOFT: f32 = 1.0;
    const LOW: f32 = 0.45;
    match tone {
        AlertTone::Blip => (SOFT, vec![note(0.0, 0.075, 850.0, 960.0, 0.006, 0.0, 0.014, 0.03)]),
        AlertTone::BlipLong => (SOFT, vec![note(0.0, 0.14, 850.0, 960.0, 0.008, 0.0, 0.032, 0.04)]),
        AlertTone::Chime => (SOFT, vec![note(0.0, 0.09, 700.0, 700.0, 0.010, 0.0, 0.026, 0.04), note(0.08, 0.12, 930.0, 930.0, 0.010, 0.0, 0.036, 0.05)]),
        AlertTone::Droplet => (SOFT, vec![note(0.0, 0.12, 600.0, 1100.0, 0.008, 0.0, 0.032, 0.04)]),
        AlertTone::Beep => (LOW, vec![Note { slow: RING, ..note(0.0, 0.5, 500.0, 500.0, 0.012, 0.03, 0.06, 0.05) }]),
        AlertTone::BeepSlow => (LOW, vec![Note { slow: RING, ..note(0.0, 0.6, 500.0, 500.0, 0.025, 0.05, 0.11, 0.06) }]),
        AlertTone::Rise => (
            LOW,
            vec![
                note(0.0, 0.22, 440.0, 440.0, 0.012, 0.02, 0.05, 0.05),
                note(0.15, 0.22, 523.0, 523.0, 0.012, 0.02, 0.05, 0.05),
                note(0.30, 0.37, 659.0, 659.0, 0.012, 0.03, 0.08, 0.06),
            ],
        ),
    }
}

/// The samples of one note, before mixing.
fn note_samples(note: &Note) -> Vec<f32> {
    let rate = SAMPLE_RATE as f64;
    let count = (note.seconds as f64 * rate) as usize;
    let squared_decay = |t: f32, tau: f32| {
        let c = tau * 0.8;
        (-((t * t + c * c).sqrt() - c) / tau).exp()
    };
    let (mut phase, mut twin) = (0.0_f64, 0.0_f64);
    (0..count)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            let x = t / note.seconds;
            let hz = note.from_hz + (note.to_hz - note.from_hz) * (1.0 - (1.0 - x) * (1.0 - x));
            phase += std::f64::consts::TAU * hz as f64 / rate;
            twin += std::f64::consts::TAU * hz as f64 * 1.004 / rate;
            let wave = (phase.sin() + 0.10 * (2.0 * phase).sin() + 0.03 * (3.0 * phase).sin() + 0.5 * twin.sin()) / 1.5;
            let envelope = if t < note.attack {
                0.5 - 0.5 * (std::f32::consts::PI * t / note.attack).cos()
            } else {
                let since = (t - note.attack - note.hold).max(0.0);
                let fast = squared_decay(since, note.decay);
                match note.slow {
                    Some((weight, tau)) => (1.0 - weight) * fast + weight * squared_decay(since, tau),
                    None => fast,
                }
            };
            let left = (count - i) as f32 / (note.fade * SAMPLE_RATE as f32);
            let tail = if left >= 1.0 { 1.0 } else { 0.5 - 0.5 * (std::f32::consts::PI * left).cos() };
            wave as f32 * envelope * tail
        })
        .collect()
}

/// The samples of `tone` (mono, 44.1 kHz), peaking at `0.3 * volume` for the loudest tones and lower for the beeps.
fn tone_samples(tone: AlertTone, volume: f32) -> Vec<f32> {
    let (level, notes) = voice(tone);
    let total = notes.iter().map(|note| (note.start * SAMPLE_RATE as f32) as usize + (note.seconds * SAMPLE_RATE as f32) as usize).max().unwrap_or(0);
    let mut samples = vec![0.0_f32; total];
    for note in &notes {
        let offset = (note.start * SAMPLE_RATE as f32) as usize;
        for (i, sample) in note_samples(note).into_iter().enumerate() {
            samples[offset + i] += sample;
        }
    }
    let peak = samples.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    if peak > 0.0 {
        let gain = volume * 0.3 * level / peak;
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

    fn seconds(tone: AlertTone) -> f32 {
        tone_samples(tone, 1.0).len() as f32 / SAMPLE_RATE as f32
    }

    #[test]
    fn blips_are_short_and_beeps_are_longer() {
        assert!(seconds(AlertTone::Blip) < 0.1);
        assert!(seconds(AlertTone::BlipLong) < 0.2 && seconds(AlertTone::Chime) < 0.25 && seconds(AlertTone::Droplet) < 0.2);
        assert!(seconds(AlertTone::Beep) >= 0.45 && seconds(AlertTone::BeepSlow) >= 0.55 && seconds(AlertTone::Rise) >= 0.6);
    }

    #[test]
    fn every_tone_starts_and_ends_at_silence() {
        for tone in AlertTone::ALL {
            let samples = tone_samples(tone, 1.0);
            assert!(samples[0].abs() < 1e-3 && samples.last().unwrap().abs() < 1e-3, "{tone:?}");
        }
    }

    #[test]
    fn a_beep_rings_out() {
        let samples = tone_samples(AlertTone::Beep, 1.0);
        let peak = |from: f32, to: f32| samples[(from * SAMPLE_RATE as f32) as usize..(to * SAMPLE_RATE as f32) as usize].iter().fold(0.0_f32, |p, x| p.max(x.abs()));
        assert!(peak(0.0, 0.05) > 10.0 * peak(0.3, 0.35));
    }

    #[test]
    fn volume_scales_peak_and_beeps_are_quieter() {
        for tone in AlertTone::ALL {
            let (level, _) = voice(tone);
            for volume in [0.2_f32, 0.4, 1.0] {
                let peak = tone_samples(tone, volume).iter().fold(0.0_f32, |peak, x| peak.max(x.abs()));
                let expected = 0.3 * volume * level;
                assert!((peak - expected).abs() < 1e-4, "{tone:?} at {volume}: {peak} against {expected}");
            }
        }
        assert!(voice(AlertTone::Beep).0 < voice(AlertTone::Blip).0);
    }
}
