//! How loud the microphone is, measured while the meeting is still running.
//!
//! # Why during the recording
//!
//! The pipeline already reports a quiet microphone — `quiet_recording`, from
//! the track's peak — but only once the meeting has been transcribed. By then
//! the meeting is over and the level cannot be fixed. Measured here, the same
//! finding arrives in the first minute, while raising the input level still
//! saves the rest of the meeting.
//!
//! # The shape
//!
//! [`LevelMeter`] is written by the microphone's writer thread and read by a
//! watcher thread, through atomics only: the writer is one step removed from the
//! realtime callback and must never wait on anything. [`LevelWatch`] is the
//! decision — when to warn and when to take the warning back — kept pure so it
//! can be tested without audio hardware.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use meeting_core::convert::TARGET_SAMPLE_RATE;

/// The loudest sample and the amount of audio measured, for one track.
///
/// Only **unmuted** audio is observed. A deliberately muted microphone is
/// silence by request, and counting it would warn the user about the mute
/// they just pressed.
#[derive(Debug, Default)]
pub struct LevelMeter {
    /// `f32` bits of the peak. Peaks are non-negative, and non-negative `f32`s
    /// order the same way as their bit patterns, so `fetch_max` on the bits is
    /// a lock-free max on the value.
    peak_bits: AtomicU32,
    /// Samples observed, at [`TARGET_SAMPLE_RATE`].
    samples: AtomicU64,
}

impl LevelMeter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a block of samples as they are written to the track.
    pub fn observe(&self, samples: &[f32]) {
        let loudest = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        // NaN would compare as enormous in bit order; a corrupt block must not
        // make a quiet microphone read as loud.
        if loudest.is_finite() {
            self.peak_bits.fetch_max(loudest.to_bits(), Ordering::Relaxed);
        }
        self.samples.fetch_add(samples.len() as u64, Ordering::Relaxed);
    }

    /// Start again, for a different device.
    ///
    /// A microphone that fails over to another keeps its track, but the level
    /// measured on the first device says nothing about the second.
    pub fn reset(&self) {
        self.peak_bits.store(0, Ordering::Relaxed);
        self.samples.store(0, Ordering::Relaxed);
    }

    /// `(peak, seconds of unmuted audio measured)`.
    pub fn reading(&self) -> (f32, f64) {
        let peak = f32::from_bits(self.peak_bits.load(Ordering::Relaxed));
        let seconds = self.samples.load(Ordering::Relaxed) as f64 / TARGET_SAMPLE_RATE as f64;
        (peak, seconds)
    }
}

/// How much unmuted audio to hear before judging the level.
///
/// Long enough that the user has probably said something — a greeting, their
/// name — and short enough that the warning still arrives near the start of
/// the meeting. A user who stays silent for the whole first half-minute gets a
/// warning that says "if you have been speaking", which is still true.
pub const JUDGE_AFTER_SECONDS: f64 = 30.0;

/// What the watcher should tell the UI, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The microphone is too quiet. Show the warning.
    Quiet,
    /// It was reported quiet and is now fine. Take the warning back.
    Recovered,
}

/// The decision, separate from the threads and the atomics.
///
/// Warns **once** per quiet spell: the notice stays up until the level
/// recovers or the recording ends, rather than reappearing every tick.
#[derive(Debug, Default)]
pub struct LevelWatch {
    warned: bool,
}

impl LevelWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a reading; get back a change to report, if there is one.
    ///
    /// `quiet_peak` is the threshold the pipeline uses after the fact
    /// (`whisper::QUIET_PEAK`), passed in so the two can never disagree about
    /// what "quiet" means.
    pub fn update(&mut self, peak: f32, seconds: f64, quiet_peak: f32) -> Option<Change> {
        if seconds < JUDGE_AFTER_SECONDS {
            return None;
        }
        let quiet = peak < quiet_peak;
        match (quiet, self.warned) {
            (true, false) => {
                self.warned = true;
                Some(Change::Quiet)
            }
            (false, true) => {
                self.warned = false;
                Some(Change::Recovered)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: f32 = 0.05;

    #[test]
    fn the_meter_keeps_the_loudest_sample_and_counts_time() {
        let meter = LevelMeter::new();
        meter.observe(&[0.01, -0.03, 0.02]);
        meter.observe(&[0.005]);
        let (peak, seconds) = meter.reading();
        assert!((peak - 0.03).abs() < 1e-6, "negative samples count by magnitude");
        assert!((seconds - 4.0 / TARGET_SAMPLE_RATE as f64).abs() < 1e-9);

        meter.reset();
        assert_eq!(meter.reading(), (0.0, 0.0));
    }

    #[test]
    fn a_nan_does_not_read_as_loud() {
        let meter = LevelMeter::new();
        meter.observe(&[0.01, f32::NAN]);
        assert!(meter.reading().0 < QUIET);
    }

    #[test]
    fn nothing_is_judged_before_enough_audio() {
        let mut watch = LevelWatch::new();
        assert_eq!(watch.update(0.0, JUDGE_AFTER_SECONDS - 1.0, QUIET), None);
    }

    #[test]
    fn a_quiet_microphone_is_reported_once() {
        let mut watch = LevelWatch::new();
        assert_eq!(watch.update(0.02, 31.0, QUIET), Some(Change::Quiet));
        assert_eq!(watch.update(0.02, 32.0, QUIET), None, "not again every tick");
        assert_eq!(watch.update(0.03, 60.0, QUIET), None);
    }

    #[test]
    fn a_microphone_that_is_raised_takes_the_warning_back() {
        let mut watch = LevelWatch::new();
        watch.update(0.02, 31.0, QUIET);
        assert_eq!(watch.update(0.3, 40.0, QUIET), Some(Change::Recovered));
        assert_eq!(watch.update(0.3, 41.0, QUIET), None);
    }

    #[test]
    fn a_normal_microphone_is_never_reported() {
        let mut watch = LevelWatch::new();
        for second in 0..120 {
            assert_eq!(watch.update(0.4, second as f64, QUIET), None);
        }
    }

    #[test]
    fn a_reset_meter_is_judged_afresh() {
        // After a failover the meter restarts from zero, so the watch sees too
        // little audio and says nothing — the warning, if up, stays up until
        // the new device proves itself.
        let mut watch = LevelWatch::new();
        watch.update(0.02, 31.0, QUIET);
        assert_eq!(watch.update(0.0, 0.0, QUIET), None);
        assert_eq!(watch.update(0.2, 31.0, QUIET), Some(Change::Recovered));
    }
}
