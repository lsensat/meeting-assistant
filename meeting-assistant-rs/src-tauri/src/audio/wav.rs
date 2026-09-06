//! Mono 48 kHz PCM16 track writer. Port of the `sf.SoundFile` usage in
//! `record_microphone` / `record_system_audio` (`app.py:1493`, `1676`).
//!
//! Every track is written at [`TARGET_SAMPLE_RATE`] regardless of what the
//! device runs at, so the two files are always directly comparable and the
//! transcript timestamps line up. Resampling happens before this type sees the
//! samples.
//!
//! # Ownership rule
//!
//! A `TrackWriter` is owned exclusively by its recorder's writer thread. It is
//! never wrapped in an `Arc<Mutex<..>>`. The reason is the silence-gap
//! bookkeeping: "how much silence do I still owe this file" must never be a
//! question two threads can answer differently, or the tracks drift apart
//! across a device disconnect.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use meeting_core::convert::{f32_to_i16, TARGET_SAMPLE_RATE};

/// Failure writing a track. Kept separate from device errors: a write failure
/// is fatal for the recording, a device failure is recoverable by failover.
#[derive(Debug)]
pub enum WavError {
    Create(PathBuf, hound::Error),
    Write(hound::Error),
    Finalize(hound::Error),
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create(path, e) => write!(f, "could not create {}: {e}", path.display()),
            Self::Write(e) => write!(f, "could not write audio: {e}"),
            Self::Finalize(e) => write!(f, "could not finalize the WAV file: {e}"),
        }
    }
}

impl std::error::Error for WavError {}

pub struct TrackWriter {
    writer: hound::WavWriter<BufWriter<File>>,
    path: PathBuf,
    frames: u64,
}

impl TrackWriter {
    /// Create a mono 48 kHz PCM16 file, matching
    /// `sf.SoundFile(mode="w", samplerate=TARGET_SAMPLE_RATE, channels=1, subtype="PCM_16")`.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, WavError> {
        let path = path.as_ref().to_path_buf();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        let writer = hound::WavWriter::create(&path, spec)
            .map_err(|e| WavError::Create(path.clone(), e))?;

        Ok(Self {
            writer,
            path,
            frames: 0,
        })
    }

    /// Append mono samples already resampled to [`TARGET_SAMPLE_RATE`].
    ///
    /// Conversion goes through [`f32_to_i16`], which scales by 32767 to match
    /// libsndfile. Do not inline a different scale factor here — the byte-diff
    /// parity test against the Python app's WAVs depends on it.
    pub fn write_samples(&mut self, samples: &[f32]) -> Result<(), WavError> {
        for &sample in samples {
            self.writer
                .write_sample(f32_to_i16(sample))
                .map_err(WavError::Write)?;
        }
        self.frames += samples.len() as u64;
        Ok(())
    }

    /// Append `frames` samples of digital silence.
    ///
    /// This is what keeps the two tracks aligned across a disconnect: the file
    /// position has to advance by exactly the wall-clock time the device was
    /// missing, or every later timestamp on this track is early.
    pub fn write_silence(&mut self, frames: usize) -> Result<(), WavError> {
        for _ in 0..frames {
            self.writer.write_sample(0i16).map_err(WavError::Write)?;
        }
        self.frames += frames as u64;
        Ok(())
    }

    /// Samples written so far. At 48 kHz mono this is also the frame count.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn duration_seconds(&self) -> f64 {
        self.frames as f64 / TARGET_SAMPLE_RATE as f64
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Flush and patch the RIFF header. Must be called before the containing
    /// folder is renamed — see the ordering note at `app.py:2221-2237`.
    pub fn finalize(self) -> Result<u64, WavError> {
        let frames = self.frames;
        self.writer.finalize().map_err(WavError::Finalize)?;
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("meeting-assistant-test-{name}-{}.wav", std::process::id()));
        path
    }

    #[test]
    fn writes_a_mono_48k_pcm16_header() {
        let path = temp_path("header");
        let writer = TrackWriter::create(&path).expect("create");
        writer.finalize().expect("finalize");

        let reader = hound::WavReader::open(&path).expect("open");
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, TARGET_SAMPLE_RATE);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, hound::SampleFormat::Int);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn silence_advances_the_frame_count_exactly() {
        let path = temp_path("silence");
        let mut writer = TrackWriter::create(&path).expect("create");

        writer.write_samples(&[0.5, -0.5]).expect("samples");
        writer.write_silence(48_000).expect("silence");
        assert_eq!(writer.frames(), 48_002);
        // One second of silence plus two samples.
        assert!((writer.duration_seconds() - 1.0000416).abs() < 1e-6);

        let frames = writer.finalize().expect("finalize");
        assert_eq!(frames, 48_002);

        let reader = hound::WavReader::open(&path).expect("open");
        assert_eq!(reader.len(), 48_002);

        std::fs::remove_file(&path).ok();
    }

    /// The 32767 scale factor is a parity contract, not an implementation
    /// detail; assert it survives the trip through the file.
    #[test]
    fn full_scale_sample_round_trips_as_32767() {
        let path = temp_path("scale");
        let mut writer = TrackWriter::create(&path).expect("create");
        writer.write_samples(&[1.0, -1.0, 0.0]).expect("samples");
        writer.finalize().expect("finalize");

        let mut reader = hound::WavReader::open(&path).expect("open");
        let samples: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(samples, vec![32767, -32767, 0]);

        std::fs::remove_file(&path).ok();
    }
}
