//! Post-recording processing: transcribe both tracks, merge, summarize, write.
//!
//! Port of `process_meeting` (`app.py:2211`) and `summarize_with_ollama`
//! (`app.py:2076`). The ordering here is not incidental — several steps are
//! sequenced the way they are for reasons that only show up when something goes
//! wrong. Each is commented at the point it matters.

use std::path::{Path, PathBuf};

use meeting_core::config::{Language, SummaryType};
use meeting_core::progress::transcription_percent;
use meeting_core::text;
use meeting_core::prompts;

use crate::ollama;
use crate::session::{MIC_FILENAME, SYSTEM_FILENAME};
use crate::whisper::{self, Transcriber};

/// Which processing stage the UI chip should show as working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Audio,
    Whisper,
    Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageState {
    Pending,
    Working,
    Done,
    Error,
}

/// What the pipeline reports while it runs. Mirrors the Python queue tags.
#[derive(Debug, Clone)]
pub enum Progress {
    Stage(Stage, StageState),
    Status(String),
}

#[derive(Debug)]
pub enum PipelineError {
    /// The recording contained no speech at all.
    NoVoice,
    Whisper(whisper::WhisperError),
    Ollama(ollama::OllamaError),
    Io(std::io::Error),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoVoice => write!(f, "No speech was detected in the recording."),
            Self::Whisper(e) => write!(f, "{e}"),
            Self::Ollama(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PipelineError {}

impl From<whisper::WhisperError> for PipelineError {
    fn from(e: whisper::WhisperError) -> Self {
        Self::Whisper(e)
    }
}
impl From<ollama::OllamaError> for PipelineError {
    fn from(e: ollama::OllamaError) -> Self {
        Self::Ollama(e)
    }
}
impl From<std::io::Error> for PipelineError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Everything the pipeline needs, owned. Same discipline as `RecorderConfig`:
/// nothing is read from live UI state once processing starts.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub folder: PathBuf,
    /// Appended to the folder name once the WAVs are closed.
    pub meeting_title: String,
    /// Where recordings should end up, read at *stop* time.
    ///
    /// The folder is created when recording starts, so changing this in
    /// Settings mid-meeting would otherwise only affect the next one. Reading
    /// it here means "I picked the wrong folder, let me fix it before I stop"
    /// does what the user expects.
    pub output_folder: PathBuf,
    pub whisper_model: String,
    /// `None` means auto-detect.
    pub transcription_language: Option<String>,
    pub ollama_model: String,
    pub language: Language,
    pub summary_type: SummaryType,
    pub custom_summary_prompt: String,
    pub keep_audio: bool,
    /// Speaker labels, already localized by the caller.
    pub speaker_me: String,
    pub speaker_meeting: String,
}

#[derive(Debug)]
pub struct PipelineOutput {
    pub folder: PathBuf,
    pub transcript_file: PathBuf,
    pub summary_file: PathBuf,
    pub segment_count: usize,
}

/// Run the whole post-recording pipeline.
///
/// `on_progress` is called from this thread; the caller forwards it to the UI.
pub fn run(
    config: PipelineConfig,
    mut on_progress: impl FnMut(Progress),
) -> Result<PipelineOutput, PipelineError> {
    on_progress(Progress::Stage(Stage::Audio, StageState::Working));

    // Rename the folder only now.
    //
    // The WAVs are closed by the time `RecordingSession::stop` returns, and the
    // rename must not happen before that: renaming a directory out from under
    // an open file handle leaves a truncated RIFF header. The Python has an
    // explicit comment about this at `app.py:2221-2237`; preserve the ordering.
    let folder = rename_folder(&config.folder, &config.meeting_title, &config.output_folder)?;

    let mic_file = folder.join(MIC_FILENAME);
    let system_file = folder.join(SYSTEM_FILENAME);
    let transcript_file = folder.join("transcript.txt");
    let summary_file = folder.join("summary.md");

    on_progress(Progress::Stage(Stage::Audio, StageState::Done));

    // --- transcription ---------------------------------------------------
    on_progress(Progress::Stage(Stage::Whisper, StageState::Working));

    if !whisper::is_installed(&config.whisper_model) {
        on_progress(Progress::Status(format!(
            "Downloading Whisper model {}...",
            config.whisper_model
        )));
        whisper::download_model(&config.whisper_model, |percent| {
            on_progress(Progress::Status(format!(
                "Downloading Whisper model {} ({percent}%)...",
                config.whisper_model
            )));
        })?;
    } else {
        on_progress(Progress::Status(format!(
            "Loading Whisper model {}...",
            config.whisper_model
        )));
    }

    let transcriber = Transcriber::load(
        &config.whisper_model,
        config.transcription_language.as_deref(),
    )?;

    let mic_duration = wav_duration(&mic_file).unwrap_or(0.0);
    let system_duration = wav_duration(&system_file).unwrap_or(0.0);
    let total_duration = mic_duration + system_duration;

    // Both tracks already share one origin: `RecordingSession::start` stamps a
    // single `Instant` and hands it to both recorders, and each track's
    // lead-in silence covers its own open latency. So `common_start` — which
    // the Python had to compute from two separate stamps (`app.py:2280`) — is
    // zero here by construction, and both offsets are zero.
    let mut segments = transcriber.transcribe(&mic_file, &config.speaker_me, 0.0, |end| {
        let percent = transcription_percent(end, mic_duration, 0.0, total_duration);
        on_progress(Progress::Status(format!(
            "Transcribing {}... {percent}%",
            config.speaker_me
        )));
    })?;

    let system_segments =
        transcriber.transcribe(&system_file, &config.speaker_meeting, 0.0, |end| {
            let percent =
                transcription_percent(end, system_duration, mic_duration, total_duration);
            on_progress(Progress::Status(format!(
                "Transcribing {}... {percent}%",
                config.speaker_meeting
            )));
        })?;

    segments.extend(system_segments);

    // Interleaves the two speakers by timestamp and formats
    // `[HH:MM:SS] SPEAKER: text`. The exact format and ordering are part of the
    // parity contract with the Python.
    let transcript = text::build_transcript(&mut segments);

    // Deliberately NOT written before the emptiness check.
    //
    // The Python writes `transcript.txt` and only then raises `error_no_voice`
    // (`app.py:2343` vs `2348`), leaving a stray empty file behind on that
    // path. That is deferred fix #5. Since writing the file at all is the bug,
    // and this is the ordering the register already records as wrong, the file
    // is written after the check — the observable difference is only that a
    // failed run leaves no empty artifact.
    if transcript.trim().is_empty() {
        return Err(PipelineError::NoVoice);
    }

    std::fs::write(&transcript_file, &transcript)?;
    on_progress(Progress::Stage(Stage::Whisper, StageState::Done));

    // --- summary ---------------------------------------------------------
    on_progress(Progress::Stage(Stage::Summary, StageState::Working));

    let summary = summarize(&transcript, &config, &mut on_progress)?;
    std::fs::write(&summary_file, &summary)?;

    on_progress(Progress::Stage(Stage::Summary, StageState::Done));

    // --- cleanup ---------------------------------------------------------
    //
    // Only after both artifacts exist. Deleting the audio earlier would make a
    // failure at any later step unrecoverable — the meeting would be gone.
    if !config.keep_audio {
        for file in [&mic_file, &system_file] {
            let _ = std::fs::remove_file(file);
        }
    }

    Ok(PipelineOutput {
        folder,
        transcript_file,
        summary_file,
        segment_count: segments.len(),
    })
}

/// Chunk, extract per chunk, then synthesize. Port of `summarize_with_ollama`.
fn summarize(
    transcript: &str,
    config: &PipelineConfig,
    on_progress: &mut impl FnMut(Progress),
) -> Result<String, PipelineError> {
    let chunks = text::split_transcript(transcript, text::DEFAULT_CHUNK_CHARS);
    let system = prompts::system_prompt(config.language);

    let mut partials = Vec::with_capacity(chunks.len());
    let total = chunks.len();

    for (index, chunk) in chunks.iter().enumerate() {
        on_progress(Progress::Status(format!(
            "Summarizing block {}/{total} with {}...",
            index + 1,
            config.ollama_model
        )));

        let user = prompts::extraction_message(config.language, chunk);
        partials.push(ollama::chat(&config.ollama_model, system, &user)?);
    }

    on_progress(Progress::Status("Generating the final summary...".into()));

    let combined = prompts::combine_partials(&partials);
    let user = prompts::final_message(
        config.language,
        config.summary_type,
        &config.custom_summary_prompt,
        &combined,
    );

    Ok(ollama::chat(&config.ollama_model, system, &user)?)
}

/// Append the user's meeting title to the folder name, if they gave one.
///
/// Collisions get a timestamp suffix rather than failing or overwriting, which
/// is what the Python does (`app.py:2228-2233`).
fn rename_folder(
    folder: &Path,
    title: &str,
    output_folder: &Path,
) -> std::io::Result<PathBuf> {
    let title = text::sanitize_name(title);

    // The destination may have changed in Settings since recording began.
    // Falling back to the current parent keeps this a no-op in the normal case.
    let parent = if output_folder.as_os_str().is_empty() {
        folder.parent()
    } else {
        Some(output_folder)
    };

    let Some(parent) = parent else {
        return Ok(folder.to_path_buf());
    };

    let moving = folder.parent() != Some(parent);
    if title.is_empty() && !moving {
        return Ok(folder.to_path_buf());
    }

    if moving {
        std::fs::create_dir_all(parent)?;
    }

    let base = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut target = if title.is_empty() {
        parent.join(&base)
    } else {
        parent.join(format!("{base}_{title}"))
    };
    if target.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        target = if title.is_empty() {
            parent.join(format!("{base}_{stamp}"))
        } else {
            parent.join(format!("{base}_{title}_{stamp}"))
        };
    }

    // A cross-filesystem move fails with EXDEV. Recordings that are safely on
    // disk must not be lost to a relocation the user asked for as an
    // afterthought, so keep them where they are and report the original path.
    match std::fs::rename(folder, &target) {
        Ok(()) => Ok(target),
        Err(e) if moving => {
            eprintln!(
                "could not move the recording to {}: {e}; leaving it at {}",
                target.display(),
                folder.display()
            );
            Ok(folder.to_path_buf())
        }
        Err(e) => Err(e),
    }
}

fn wav_duration(path: &Path) -> Option<f64> {
    let reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    if spec.sample_rate == 0 {
        return None;
    }
    Some(reader.len() as f64 / spec.sample_rate as f64 / spec.channels.max(1) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meeting-pipeline-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn an_empty_title_leaves_the_folder_alone() {
        let dir = temp_dir("no-title");
        let result = rename_folder(&dir, "", Path::new("")).expect("rename");
        assert_eq!(result, dir);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A hyphenated title is ordinary and must rename normally.
    #[test]
    fn a_hyphenated_title_renames_normally() {
        let parent = temp_dir("hyphen");
        let folder = parent.join("2026-09-03_10-00");
        std::fs::create_dir_all(&folder).expect("mkdir");

        let result = rename_folder(&folder, "daily-standup", Path::new("")).expect("rename");

        assert_eq!(
            result.file_name().unwrap().to_string_lossy(),
            "2026-09-03_10-00_daily-standup"
        );
        assert!(result.is_dir());
        std::fs::remove_dir_all(&parent).ok();
    }

    /// A title made only of stripped characters sanitizes to empty. The folder
    /// must be left exactly as it was — not renamed to a trailing underscore,
    /// and above all not treated as an error, because this runs immediately
    /// after the WAVs close and a failure here would strand the recording.
    #[test]
    fn a_title_of_only_hyphens_leaves_the_folder_alone() {
        let parent = temp_dir("all-hyphens");
        let folder = parent.join("2026-09-03_10-00");
        std::fs::create_dir_all(&folder).expect("mkdir");

        let result = rename_folder(&folder, "--", Path::new("")).expect("rename");

        assert_eq!(result, folder);
        assert!(result.is_dir());
        std::fs::remove_dir_all(&parent).ok();
    }

    /// Changing the output folder in Settings *during* a meeting must move the
    /// recording, not just apply to the next one. The folder is created when
    /// recording starts, so without this the change would appear to do nothing.
    #[test]
    fn a_changed_output_folder_relocates_the_recording() {
        let old_parent = temp_dir("relocate-from");
        let new_parent = temp_dir("relocate-to");
        let folder = old_parent.join("2026-09-03_10-00");
        std::fs::create_dir_all(&folder).expect("mkdir");
        std::fs::write(folder.join("microphone.wav"), b"x").expect("write");

        let result = rename_folder(&folder, "standup", &new_parent).expect("rename");

        assert!(result.starts_with(&new_parent), "got {}", result.display());
        assert!(result.join("microphone.wav").exists(), "the audio moved with it");
        assert!(!folder.exists(), "the old folder is gone");

        std::fs::remove_dir_all(&old_parent).ok();
        std::fs::remove_dir_all(&new_parent).ok();
    }

    /// Relocating with no title must still move, and must not leave a trailing
    /// underscore on the folder name.
    #[test]
    fn relocation_without_a_title_keeps_the_original_name() {
        let old_parent = temp_dir("relocate-untitled-from");
        let new_parent = temp_dir("relocate-untitled-to");
        let folder = old_parent.join("2026-09-03_10-00");
        std::fs::create_dir_all(&folder).expect("mkdir");

        let result = rename_folder(&folder, "", &new_parent).expect("rename");

        assert_eq!(result, new_parent.join("2026-09-03_10-00"));

        std::fs::remove_dir_all(&old_parent).ok();
        std::fs::remove_dir_all(&new_parent).ok();
    }

    #[test]
    fn a_title_is_appended_and_sanitized() {
        let parent = temp_dir("titled");
        let folder = parent.join("2026-09-03_10-00");
        std::fs::create_dir_all(&folder).expect("mkdir");

        let result = rename_folder(&folder, "Q3 review: roadmap/plan", Path::new("")).expect("rename");

        let name = result.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("2026-09-03_10-00_"), "got {name}");
        // sanitize_name strips <>:"/\|?* and collapses whitespace to _
        assert!(!name.contains('/'), "path separator survived: {name}");
        assert!(!name.contains(':'), "colon survived: {name}");
        assert!(result.exists());

        std::fs::remove_dir_all(&parent).ok();
    }

    /// A second meeting with the same title must not collide with the first.
    #[test]
    fn a_colliding_title_gets_a_timestamp_suffix() {
        let parent = temp_dir("collide");
        let first = parent.join("meeting");
        std::fs::create_dir_all(&first).expect("mkdir");
        let renamed = rename_folder(&first, "standup", Path::new("")).expect("first rename");

        let second = parent.join("meeting");
        std::fs::create_dir_all(&second).expect("mkdir");
        let renamed2 = rename_folder(&second, "standup", Path::new("")).expect("second rename");

        assert_ne!(renamed, renamed2);
        assert!(renamed.exists() && renamed2.exists());

        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn wav_duration_reads_a_real_file() {
        use crate::audio::wav::TrackWriter;

        let dir = temp_dir("duration");
        let path = dir.join("x.wav");
        let mut writer = TrackWriter::create(&path).expect("create");
        writer.write_silence(48_000).expect("silence");
        writer.finalize().expect("finalize");

        let duration = wav_duration(&path).expect("duration");
        assert!((duration - 1.0).abs() < 1e-9, "got {duration}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_audio_has_no_duration() {
        assert!(wav_duration(Path::new("/nonexistent/x.wav")).is_none());
    }
}
