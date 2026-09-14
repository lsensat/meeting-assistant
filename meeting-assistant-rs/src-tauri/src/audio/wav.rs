//! Mono 48 kHz PCM16 track writer.
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
    /// libsndfile. Do not inline a different scale factor here: the tests
    /// compare written bytes exactly, and 32768 would clip full-scale samples.
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

    /// Flush and patch the RIFF header.
    ///
    /// Must be called before the containing folder is renamed: renaming a
    /// directory out from under an open file handle leaves the header
    /// unfinalised, and the file then claims to hold no audio at all. See
    /// [`repair_unfinalised`] for what that costs afterwards.
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

/// Repair the length fields of a WAV whose writer never got to finish.
///
/// # Why this is needed at all
///
/// `hound` writes the header with placeholder lengths and corrects them when the
/// writer is finalised or dropped. A process killed mid-recording — Task
/// Manager, a panic, a power cut — runs neither, so the file on disk claims to
/// hold **zero** samples while holding minutes of audio. Every reader believes
/// the header: the recording is intact on disk and unplayable.
///
/// Both numbers are recoverable from the file's own length, because the format
/// is fixed here: mono 48 kHz PCM16, written by [`TrackWriter::create`], with the
/// canonical 44-byte header.
///
/// # What it will not do
///
/// It only ever *grows* a length to match the bytes actually present, and only
/// when the stored length is short. A file whose header already agrees with its
/// size is left untouched, so this is safe to run over a folder repeatedly and
/// cannot damage a healthy recording.
///
/// Returns the number of sample bytes the file now declares, or `None` if
/// nothing needed repairing.
pub fn repair_unfinalised(path: &Path) -> Result<Option<u64>, std::io::Error> {
    use std::io::{Read, Seek, SeekFrom, Write};

    let mut file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let file_len = file.metadata()?.len();

    // 44 bytes of header and at least one frame. Anything smaller is not a
    // recording that was interrupted, it is a file that never started.
    if file_len < 46 {
        return Ok(None);
    }

    let mut header = [0u8; 12];
    file.read_exact(&mut header)?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Ok(None);
    }

    // Walk the chunks rather than assuming `data` sits at offset 36: the
    // assumption holds for what this app writes today, and would be wrong the
    // day anything writes a LIST or a fact chunk.
    let mut offset: u64 = 12;
    let data_start = loop {
        if offset + 8 > file_len {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk)?;
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;

        if &chunk[0..4] == b"data" {
            break offset + 8;
        }
        // Chunks are padded to an even length.
        offset += 8 + size + (size % 2);
    };

    let stored = {
        file.seek(SeekFrom::Start(data_start - 4))?;
        let mut buf = [0u8; 4];
        file.read_exact(&mut buf)?;
        u32::from_le_bytes(buf) as u64
    };

    // Whole frames only: a kill can land mid-sample, and half a sample would
    // shift every sample after it by one byte.
    let actual = (file_len - data_start) / 2 * 2;
    if stored >= actual || actual == 0 {
        return Ok(None);
    }

    file.seek(SeekFrom::Start(data_start - 4))?;
    file.write_all(&(actual as u32).to_le_bytes())?;

    // The RIFF length counts everything after its own 8 bytes.
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&((data_start + actual - 8) as u32).to_le_bytes())?;
    file.flush()?;

    Ok(Some(actual))
}

#[cfg(test)]
mod repair_tests {
    use super::*;

    /// A writer that is killed rather than finalised, reproduced exactly.
    ///
    /// `std::mem::forget` is the point: it skips `Drop`, which is what corrects
    /// the header, the same way a `kill -9` skips it. Two folders on a real
    /// Windows machine were left in precisely this state by a Task Manager kill.
    fn interrupted_wav(name: &str, frames: usize) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ma-repair-{}-{}-{name}.wav",
            std::process::id(),
            frames
        ));
        std::fs::remove_file(&path).ok();

        let mut writer = TrackWriter::create(&path).expect("create");
        writer
            .write_samples(&vec![0.5f32; frames])
            .expect("write");

        // No flush, no finalize: `BufWriter` has already spilled everything
        // past its 8 KiB buffer to disk on its own, and `mem::forget` skips the
        // `Drop` that would correct the header — which is what a kill skips.
        //
        // Deliberately not `writer.flush()`: hound's flush *also* rewrites the
        // header, so using it produced a healthy file and the first version of
        // this test proved nothing.
        std::mem::forget(writer);
        path
    }

    #[test]
    fn an_interrupted_recording_is_unreadable_and_then_readable() {
        let path = interrupted_wav("basic", 48_000);

        // The state the bug is about: the audio is on disk, the header says
        // there is none.
        let before = hound::WavReader::open(&path).expect("open").len();
        assert_eq!(before, 0, "the fixture is wrong: the header was finalised");

        let repaired = repair_unfinalised(&path).expect("repair").expect("repaired");

        let reader = hound::WavReader::open(&path).expect("open");
        assert_eq!(reader.len() as u64, repaired / 2, "the header now matches the bytes");
        assert_eq!(reader.spec().sample_rate, TARGET_SAMPLE_RATE);

        // Everything the buffer had already spilled is recovered. The tail
        // still in the buffer when the process died is gone — it never reached
        // the disk, and no repair can invent it. At 48 kHz mono PCM16 that is
        // under a tenth of a second.
        assert!(
            reader.len() >= 48_000 - 4_096 && reader.len() <= 48_000,
            "recovered {} of 48000 frames",
            reader.len()
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_healthy_recording_is_left_alone() {
        let path = std::env::temp_dir().join(format!("ma-repair-ok-{}.wav", std::process::id()));
        std::fs::remove_file(&path).ok();
        let mut writer = TrackWriter::create(&path).expect("create");
        writer.write_samples(&vec![0.25f32; 1000]).expect("write");
        writer.finalize().expect("finalize");

        let before = std::fs::read(&path).expect("read");
        assert!(
            repair_unfinalised(&path).expect("repair").is_none(),
            "a finalised file must not be touched"
        );
        assert_eq!(before, std::fs::read(&path).expect("read"), "byte-identical");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_file_that_is_not_a_wav_is_refused() {
        let path = std::env::temp_dir().join(format!("ma-repair-junk-{}.bin", std::process::id()));
        std::fs::write(&path, vec![7u8; 5000]).expect("write");
        assert!(repair_unfinalised(&path).expect("repair").is_none());
        std::fs::remove_file(&path).ok();
    }

    /// A kill can land between the two bytes of a sample.
    #[test]
    fn a_half_written_sample_is_dropped_rather_than_shifting_everything() {
        // Big enough that `BufWriter` has spilled to disk; with a hundred
        // frames nothing had reached the file and there was nothing to repair.
        let path = interrupted_wav("odd", 48_000);
        // One stray byte, as if the process died mid-sample.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).expect("open");
            f.write_all(&[0x42]).expect("append");
        }

        let repaired = repair_unfinalised(&path).expect("repair").expect("repaired");
        assert_eq!(repaired % 2, 0, "a whole number of samples, never half of one");
        assert_eq!(
            hound::WavReader::open(&path).expect("open").len() as u64,
            repaired / 2
        );

        std::fs::remove_file(&path).ok();
    }
}
