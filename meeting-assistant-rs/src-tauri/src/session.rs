//! A recording session: both tracks, started together and stopped together.
//!
//! Both tracks are started from one place and stopped from one place, so
//! neither can outlive the other.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{unbounded, Receiver};

use crate::audio::devices::SourceKind;
use crate::audio::recorder::{
    self, Event, Recorder, RecorderConfig, StopEvent, TrackSummary,
};

pub const MIC_FILENAME: &str = "microphone.wav";
pub const SYSTEM_FILENAME: &str = "system_audio.wav";

pub struct RecordingSession {
    stop: Arc<StopEvent>,
    muted: Arc<AtomicBool>,
    started: Instant,
    folder: PathBuf,
    tracks: Vec<Recorder>,
    /// Kept so the caller can drain UI events while recording.
    pub events: Receiver<Event>,
    /// The system-wide microphone mute, while this recording holds one.
    ///
    /// A field rather than something the commands take and release, for the
    /// reason spelled out on `_queue_hold` below: every way out of a recording
    /// drops this session, and dropping it hands the microphone back.
    ///
    /// Listed before `_queue_hold` so it drops first. Nothing depends on the
    /// order, but releasing the user's microphone is the more urgent of the two.
    system_mute: std::sync::Mutex<Option<crate::audio::system_mute::MuteGuard>>,

    /// The microphone actually capturing, which is not always the configured
    /// one.
    ///
    /// On Windows a headset that is not plugged in falls back to whatever is
    /// there, and the first version of the mute looked up the configured name:
    /// it reported "the device refused — no input device named ..." and muted
    /// nothing, while the recording ran happily on a device it had never been
    /// told about. The mute has to follow the device the audio is coming from.
    ///
    /// `None` until the stream opens — which took 18 seconds on the machine
    /// that found this, long enough for the user to have pressed mute first.
    mic_device: std::sync::Mutex<Option<String>>,

    /// Stops the processing queue for as long as this session exists.
    ///
    /// A field rather than something the commands acquire and release, so the
    /// hold cannot outlive or under-live the recording. Every way out of a
    /// recording — the `?` returns in `stop_recording` and `cancel_recording`,
    /// the early return for a folder outside the output directory — drops this
    /// session, and dropping this session releases the queue.
    ///
    /// Note what this does **not** cover: a panic in a recorder thread. That is
    /// absorbed by `handle.join()` and reported as a failed track, leaving this
    /// session alive in `AppState` and the hold in place until someone stops or
    /// cancels. The guard covers a panic in whichever thread owns the session,
    /// which is a different thing.
    ///
    /// It is listed **last** deliberately. `stop` moves out `folder` and
    /// `tracks` and lets the rest drop at the end of the function, which is
    /// after the writer threads are joined and the WAVs are closed. Releasing
    /// the queue any earlier would let transcription start while this
    /// recording's own files were still being flushed.
    _queue_hold: crate::queue::RecordingHold,
}

impl RecordingSession {
    /// Take or release the system-wide microphone mute.
    ///
    /// Separate from [`set_muted`](Self::set_muted), which silences the samples
    /// this recording writes. This one silences the device for **every**
    /// application, and is only ever held while a recording is running — the
    /// guard is a field of this session, so every way out of a recording
    /// releases it.
    ///
    /// Returns the error when the OS refuses, so the caller can say the mute is
    /// recording-only rather than let the button claim more than it did.
    pub fn set_system_mute(
        &self,
        muted: bool,
        device: &str,
        remember: impl Fn(Option<&str>) + Send + Sync + 'static,
    ) -> Result<(), crate::audio::system_mute::MuteError> {
        let mut slot = self.system_mute.lock().expect("system mute poisoned");

        if !muted {
            // Dropping the guard is what unmutes and clears the flag.
            *slot = None;
            return Ok(());
        }

        // Already muting the right device.
        if slot.as_ref().map(|g| g.device() == device).unwrap_or(false) {
            return Ok(());
        }

        // Held, but on a different device — the recording followed a
        // disconnect. Release the old one first: leaving it muted would strand
        // a device the user is no longer recording with.
        *slot = None;

        *slot = Some(crate::audio::system_mute::MuteGuard::acquire(device, remember)?);
        Ok(())
    }

    /// Record which microphone is capturing now. Returns true if this is a
    /// change from what was there before, so the caller can re-take a mute that
    /// is now held on the wrong device.
    pub fn note_mic_device(&self, name: &str) -> bool {
        let mut slot = self.mic_device.lock().expect("mic device poisoned");
        let changed = slot.as_deref() != Some(name);
        *slot = Some(name.to_string());
        changed
    }

    /// The microphone to act on: the one capturing, if it is known yet.
    pub fn mic_device(&self) -> Option<String> {
        self.mic_device
            .lock()
            .expect("mic device poisoned")
            .clone()
    }

    /// Whether this recording currently holds a system-wide mute.
    pub fn holds_system_mute(&self) -> bool {
        self.system_mute
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod mic_device_tests {
    /// The rule that was wrong on Windows, as a test of the piece that decides
    /// it: the device capturing wins, and only a real change is a change.
    ///
    /// `RecordingSession` cannot be built without opening audio hardware, so
    /// this covers `note_mic_device`'s logic through the same `Mutex<Option<_>>`
    /// it uses. What it does not cover is the wiring in `commands.rs`, which is
    /// why the log line there names the device — that is the evidence on the
    /// next Windows run.
    #[test]
    fn only_a_real_change_counts_as_a_change() {
        let slot: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

        let note = |name: &str| {
            let mut slot = slot.lock().expect("poisoned");
            let changed = slot.as_deref() != Some(name);
            *slot = Some(name.to_string());
            changed
        };

        // First sight of a device is a change: this is the case that was
        // broken, where the user muted before the stream had opened.
        assert!(note("Varios micrófonos (Realtek(R) Audio)"));
        // The same device reported again is not.
        assert!(!note("Varios micrófonos (Realtek(R) Audio)"));
        // Following a disconnect to another device is.
        assert!(note("Plantronics Blackwire 3225 Series"));
    }
}

#[derive(Debug)]
pub struct SessionSummary {
    pub folder: PathBuf,
    pub elapsed_seconds: f64,
    pub tracks: Vec<Result<TrackSummary, String>>,
}

impl SessionSummary {
    /// Difference in written length between the two tracks. The alignment
    /// check: both files should cover the same wall-clock span, so anything
    /// beyond ~100 ms means silence-gap accounting went wrong somewhere.
    pub fn skew_seconds(&self) -> Option<f64> {
        let durations: Vec<f64> = self
            .tracks
            .iter()
            .filter_map(|t| t.as_ref().ok())
            .map(|t| t.duration_seconds)
            .collect();

        if durations.len() < 2 {
            return None;
        }

        let max = durations.iter().cloned().fold(f64::MIN, f64::max);
        let min = durations.iter().cloned().fold(f64::MAX, f64::min);
        Some(max - min)
    }
}

impl RecordingSession {
    /// Start both tracks.
    ///
    /// The single `Instant::now()` here is passed to both recorders. Stamping
    /// it once, before either thread is spawned, is what makes the two files
    /// share a common origin. Stamping it inside each thread instead would put
    /// thread-spawn jitter directly into the alignment between the tracks.
    /// `muted` is owned by the caller and outlives the session, so a mute set
    /// before recording starts is already in force on the first sample.
    pub fn start(
        folder: impl AsRef<Path>,
        microphone_name: &str,
        system_audio_name: &str,
        muted: Arc<AtomicBool>,
        queue: Arc<crate::queue::Queue>,
    ) -> std::io::Result<Self> {
        let folder = folder.as_ref().to_path_buf();
        std::fs::create_dir_all(&folder)?;

        // After the fallible setup above, so a recording that never starts does
        // not leave the queue held. Acquired before the capture threads, so no
        // transcription can be dispatched into the window where they are
        // opening their devices.
        let queue_hold = crate::queue::RecordingHold::acquire(queue);

        let stop = Arc::new(StopEvent::new());
        let (tx, events) = unbounded();

        let started = Instant::now();

        let tracks = vec![
            recorder::spawn(
                RecorderConfig {
                    kind: SourceKind::Microphone,
                    configured_name: microphone_name.to_string(),
                    output_file: folder.join(MIC_FILENAME),
                },
                Arc::clone(&stop),
                Arc::clone(&muted),
                tx.clone(),
                started,
            ),
            recorder::spawn(
                RecorderConfig {
                    kind: SourceKind::SystemAudio,
                    configured_name: system_audio_name.to_string(),
                    output_file: folder.join(SYSTEM_FILENAME),
                },
                Arc::clone(&stop),
                // System audio is never muted; the mute button is the
                // microphone's. Passing a separate flag keeps that explicit
                // rather than relying on the caller never setting it.
                Arc::new(AtomicBool::new(false)),
                tx,
                started,
            ),
        ];

        Ok(Self {
            stop,
            muted,
            started,
            folder,
            tracks,
            events,
            system_mute: std::sync::Mutex::new(None),
            mic_device: std::sync::Mutex::new(None),
            _queue_hold: queue_hold,
        })
    }

    /// Mute affects the microphone samples only. The file keeps advancing at
    /// realtime, so muting never shortens the track or shifts alignment.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn elapsed_seconds(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Signal both threads and wait for them to finish writing.
    ///
    /// Both WAVs are closed by the time this returns. Any folder rename must
    /// happen *after* this call: renaming with the files still open leaves a
    /// truncated RIFF header behind, and a file whose header says it is empty.
    pub fn stop(self) -> SessionSummary {
        self.stop.set();

        let elapsed_seconds = self.started.elapsed().as_secs_f64();

        let tracks = self
            .tracks
            .into_iter()
            .map(|track| match track.handle.join() {
                Ok(Ok(summary)) => Ok(summary),
                Ok(Err(e)) => Err(format!("{e}")),
                Err(_) => Err("recorder thread panicked".to_string()),
            })
            .collect();

        SessionSummary {
            folder: self.folder,
            elapsed_seconds,
            tracks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::devices::SourceKind;

    fn summary(kind: SourceKind, duration: f64) -> TrackSummary {
        TrackSummary {
            kind,
            path: PathBuf::from("x.wav"),
            frames: (duration * 48_000.0) as u64,
            duration_seconds: duration,
            automatic_fallback: false,
            final_device: "test".into(),
            overflows: 0,
            glitches: 0,
            gap_frames: 0,
            lead_in_frames: 0,
        }
    }

    #[test]
    fn skew_is_the_spread_between_track_durations() {
        let s = SessionSummary {
            folder: PathBuf::from("."),
            elapsed_seconds: 10.0,
            tracks: vec![
                Ok(summary(SourceKind::Microphone, 10.0)),
                Ok(summary(SourceKind::SystemAudio, 9.95)),
            ],
        };
        assert!((s.skew_seconds().expect("two tracks") - 0.05).abs() < 1e-9);
    }

    #[test]
    fn skew_needs_two_successful_tracks() {
        let s = SessionSummary {
            folder: PathBuf::from("."),
            elapsed_seconds: 10.0,
            tracks: vec![
                Ok(summary(SourceKind::Microphone, 10.0)),
                Err("no device".into()),
            ],
        };
        assert!(s.skew_seconds().is_none());
    }
}
