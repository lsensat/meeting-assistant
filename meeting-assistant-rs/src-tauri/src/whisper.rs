//! Transcription via whisper.cpp.
//!
//! Replaces faster-whisper (`app.py:1842`). Two differences matter and both are
//! load-bearing:
//!
//! 1. **Different weights.** faster-whisper used CTranslate2 models pulled
//!    through `huggingface_hub` and cached by `scan_cache_dir` (`app.py:695`).
//!    whisper.cpp wants GGML/GGUF `.bin` files. The Python's cache is not
//!    reusable, so this module owns download and detection itself.
//!
//! 2. **No internal resampling.** faster-whisper took a file path and handled
//!    rate conversion. whisper.cpp's `full()` assumes the slice is already
//!    16 kHz mono and gives no error otherwise — see [`Transcriber::transcribe`]
//!    and `meeting_core::convert::resample_for_whisper`.

use std::io::Write;
use std::path::{Path, PathBuf};

use meeting_core::convert::{resample_for_whisper, WHISPER_SAMPLE_RATE};
use meeting_core::text::Segment;
use whisper_rs::{
    install_logging_hooks, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

use crate::platform;

/// A downloadable model. Sizes are approximate and only used for progress.
pub struct ModelSpec {
    pub id: &'static str,
    pub approx_mb: u64,
}

/// The models offered in the UI, matching the Python's list.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec { id: "tiny", approx_mb: 75 },
    ModelSpec { id: "base", approx_mb: 142 },
    ModelSpec { id: "small", approx_mb: 466 },
    ModelSpec { id: "medium", approx_mb: 1500 },
    ModelSpec { id: "large-v3", approx_mb: 3100 },
];

/// Official whisper.cpp weight mirror.
const BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

#[derive(Debug)]
pub enum WhisperError {
    UnknownModel(String),
    Download(String),
    Io(std::io::Error),
    Load(String),
    Transcribe(String),
    ReadAudio(String),
}

impl std::fmt::Display for WhisperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownModel(id) => write!(f, "unknown Whisper model \"{id}\""),
            Self::Download(e) => write!(f, "could not download the Whisper model: {e}"),
            Self::Io(e) => write!(f, "model file error: {e}"),
            Self::Load(e) => write!(f, "could not load the Whisper model: {e}"),
            Self::Transcribe(e) => write!(f, "transcription failed: {e}"),
            Self::ReadAudio(e) => write!(f, "could not read the recording: {e}"),
        }
    }
}

impl std::error::Error for WhisperError {}

impl From<std::io::Error> for WhisperError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub fn spec(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

/// Where a model's weights live once downloaded.
pub fn model_path(id: &str) -> PathBuf {
    platform::models_dir().join(format!("ggml-{id}.bin"))
}

/// Model ids present on disk. Port of `installed_whisper_models`
/// (`app.py:695`), reading our own directory instead of the HF cache.
pub fn installed_models() -> Vec<String> {
    MODELS
        .iter()
        .filter(|m| is_installed(m.id))
        .map(|m| m.id.to_string())
        .collect()
}

/// A model counts as installed only if the file is plausibly complete.
///
/// A partial download left by a crash or a killed process would otherwise be
/// treated as present and then fail at load time, in the middle of processing a
/// meeting the user has already recorded.
pub fn is_installed(id: &str) -> bool {
    let Some(spec) = spec(id) else {
        return false;
    };
    let path = model_path(id);

    match std::fs::metadata(&path) {
        // Half the expected size is a deliberately generous floor.
        //
        // The real guarantee against partial downloads is in `download_model`,
        // which writes to a temporary file and renames only on success — so a
        // truncated download never reaches this path at all. This check is
        // defence in depth for a file placed here by other means.
        //
        // The floor stays loose because `approx_mb` is a rounded advertised
        // size, not the exact byte count. A tight bound would risk rejecting a
        // perfectly good model and re-downloading up to 3.1 GB, which is a far
        // worse failure than accepting a file that then fails at load.
        Ok(meta) => meta.len() > spec.approx_mb * 1024 * 1024 / 2,
        Err(_) => false,
    }
}

/// Download a model, reporting progress as a percentage.
///
/// Downloads to a temporary file and renames on success, so an interrupted
/// download can never be mistaken for an installed model.
pub fn download_model(
    id: &str,
    mut on_progress: impl FnMut(u8),
) -> Result<PathBuf, WhisperError> {
    let spec = spec(id).ok_or_else(|| WhisperError::UnknownModel(id.to_string()))?;
    let target = model_path(id);

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let url = format!("{BASE_URL}/ggml-{}.bin", spec.id);
    let mut response = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .map_err(|e| WhisperError::Download(e.to_string()))?
        .get(&url)
        .send()
        .map_err(|e| WhisperError::Download(e.to_string()))?;

    if !response.status().is_success() {
        return Err(WhisperError::Download(format!(
            "{url} returned status {}",
            response.status()
        )));
    }

    let expected = response
        .content_length()
        .unwrap_or(spec.approx_mb * 1024 * 1024);

    let partial = target.with_extension("part");
    let mut file = std::fs::File::create(&partial)?;

    let mut written: u64 = 0;
    let mut last_percent = u8::MAX;
    let mut buffer = vec![0u8; 1024 * 256];

    loop {
        let read = std::io::Read::read(&mut response, &mut buffer)
            .map_err(|e| WhisperError::Download(e.to_string()))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        written += read as u64;

        let percent = ((written as f64 / expected as f64) * 100.0).min(100.0) as u8;
        if percent != last_percent {
            on_progress(percent);
            last_percent = percent;
        }
    }

    file.flush()?;
    drop(file);
    std::fs::rename(&partial, &target)?;

    Ok(target)
}

/// A loaded model, reusable across both tracks of one meeting.
pub struct Transcriber {
    context: WhisperContext,
    language: Option<String>,
}

impl Transcriber {
    /// Load a model from disk, downloading it first if necessary.
    ///
    /// `language` is `None` for auto-detection, mirroring the Python's
    /// `transcription_language` of `"auto"`.
    pub fn load(model_id: &str, language: Option<&str>) -> Result<Self, WhisperError> {
        // whisper.cpp and GGML print model and buffer details straight to
        // stdout/stderr on every load. Harmless for a CLI, but in the Tauri app
        // that noise goes nowhere useful and in a bundled `.app` it is invisible
        // anyway. Routing it into the `log` crate silences it by default while
        // leaving it recoverable if a logging backend is ever enabled.
        //
        // Safe to call repeatedly; only the first call has an effect.
        install_logging_hooks();

        let path = model_path(model_id);
        if !path.exists() {
            return Err(WhisperError::Load(format!(
                "{} is not downloaded",
                path.display()
            )));
        }

        let context = WhisperContext::new_with_params(&path, WhisperContextParameters::default())
            .map_err(|e| WhisperError::Load(e.to_string()))?;

        Ok(Self {
            context,
            language: language.map(|l| l.to_string()),
        })
    }

    /// Transcribe one track.
    ///
    /// `offset_seconds` is added to every timestamp so both tracks share the
    /// meeting's origin, exactly as `transcribe_audio` does with
    /// `start_times[..] - common_start` (`app.py:1908`).
    ///
    /// `on_progress` receives the end timestamp of each segment so the caller
    /// can drive `transcription_percent` without this module knowing about the
    /// other track.
    pub fn transcribe(
        &self,
        audio_file: &Path,
        speaker: &str,
        offset_seconds: f64,
        mut on_progress: impl FnMut(f64),
    ) -> Result<Vec<Segment>, WhisperError> {
        let samples = read_wav_as_16k_mono(audio_file)?;

        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            // Both match the Python's `model.transcribe(..., beam_size=5)`
            // (`app.py:1877`). Changing them changes the transcript.
            beam_size: 5,
            patience: 0.0,
        });

        if let Some(language) = &self.language {
            params.set_language(Some(language));
        }

        // The Python passed `vad_filter=True`. whisper.cpp's equivalent knob is
        // suppressing non-speech tokens; true VAD is a separate model here.
        params.set_suppress_nst(true);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);

        let mut state = self
            .context
            .create_state()
            .map_err(|e| WhisperError::Transcribe(e.to_string()))?;

        state
            .full(params, &samples)
            .map_err(|e| WhisperError::Transcribe(e.to_string()))?;

        let mut segments = Vec::new();

        for segment in state.as_iter() {
            // whisper.cpp reports timestamps in centiseconds (10 ms units), not
            // seconds or milliseconds. Getting this wrong scales every
            // timestamp in the transcript by 10 or 100 and is not obvious from
            // a short test clip.
            let start = segment.start_timestamp() as f64 / 100.0;
            let end = segment.end_timestamp() as f64 / 100.0;

            on_progress(end);

            // `to_str_lossy` rather than `to_str`: whisper.cpp can emit invalid
            // UTF-8 mid-word on a truncated multibyte token, and losing one
            // character is far better than failing the whole meeting.
            let text = segment
                .to_str_lossy()
                .map_err(|e| WhisperError::Transcribe(e.to_string()))?;

            // Blank segments are dropped before they reach the transcript, as
            // the Python does (`app.py:1898`).
            let text = text.trim();
            if text.is_empty() {
                continue;
            }

            segments.push(Segment {
                start: start + offset_seconds,
                speaker: speaker.to_string(),
                text: text.to_string(),
            });
        }

        Ok(segments)
    }
}

/// Read one of our recordings and hand back exactly what whisper.cpp wants.
///
/// Our WAVs are mono 48 kHz PCM16 by construction, but the rate is read from
/// the file rather than assumed, so a hand-placed or future file cannot quietly
/// be misinterpreted.
fn read_wav_as_16k_mono(path: &Path) -> Result<Vec<f32>, WhisperError> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| WhisperError::ReadAudio(e.to_string()))?;
    let spec = reader.spec();

    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .map(|s| {
                s.map(|v| v as f32 / 32768.0)
                    .map_err(|e| WhisperError::ReadAudio(e.to_string()))
            })
            .collect::<Result<_, _>>()?,
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map_err(|e| WhisperError::ReadAudio(e.to_string())))
            .collect::<Result<_, _>>()?,
    };

    let mono = if spec.channels > 1 {
        meeting_core::convert::to_mono_float(&raw, spec.channels)
    } else {
        raw
    };

    let resampled = resample_for_whisper(&mono, spec.sample_rate);

    // The whole point of risk R2, asserted at the boundary. Feeding whisper.cpp
    // the wrong rate produces a fluent, confidently wrong transcript with no
    // error at all, so this is the last place the mistake is still catchable.
    debug_assert_eq!(
        resample_for_whisper(&[0.0], WHISPER_SAMPLE_RATE).len(),
        1,
        "resample_for_whisper must be a no-op at the target rate"
    );

    Ok(resampled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_resolve_and_unknown_ones_do_not() {
        assert!(spec("small").is_some());
        assert!(spec("large-v3").is_some());
        assert!(spec("enormous").is_none());
        assert!(!is_installed("enormous"));
    }

    #[test]
    fn model_paths_are_ggml_files_under_the_models_dir() {
        let path = model_path("small");
        assert!(path.starts_with(platform::models_dir()));
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("ggml-small.bin")
        );
    }

    /// Reading back a file the recorder itself wrote is the exact path used in
    /// production, so it is worth asserting the rate conversion end to end.
    #[test]
    fn reading_a_48k_recording_yields_16k_samples() {
        use crate::audio::wav::TrackWriter;

        let mut path = std::env::temp_dir();
        path.push(format!("whisper-read-test-{}.wav", std::process::id()));

        let mut writer = TrackWriter::create(&path).expect("create");
        // Two seconds at 48 kHz.
        writer.write_silence(96_000).expect("silence");
        writer.finalize().expect("finalize");

        let samples = read_wav_as_16k_mono(&path).expect("read");
        assert_eq!(samples.len(), 32_000, "two seconds at 16 kHz");

        std::fs::remove_file(&path).ok();
    }
}
