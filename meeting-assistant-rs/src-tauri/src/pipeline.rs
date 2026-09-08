//! Post-recording processing: transcribe both tracks, merge, summarize, write.
//!
//! Port of `process_meeting` (`app.py:2211`) and `summarize_with_ollama`
//! (`app.py:2076`). The ordering here is not incidental — several steps are
//! sequenced the way they are for reasons that only show up when something goes
//! wrong. Each is commented at the point it matters.

use std::path::{Path, PathBuf};

use meeting_core::config::{Language, SummaryType};
use meeting_core::text::{self, Segment};
use meeting_core::prompts;

use crate::summary::{self, ProviderConfig, SummaryError};
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
    Summary(SummaryError),
    Io(std::io::Error),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoVoice => write!(f, "No speech was detected in the recording."),
            Self::Whisper(e) => write!(f, "{e}"),
            Self::Summary(e) => write!(f, "{e}"),
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
impl From<SummaryError> for PipelineError {
    fn from(e: SummaryError) -> Self {
        Self::Summary(e)
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
    pub provider: ProviderConfig,
    pub language: Language,
    pub summary_type: SummaryType,
    pub custom_summary_prompt: String,
    pub keep_audio: bool,
    /// Speaker labels, already localized by the caller.
    pub speaker_me: String,
    pub speaker_meeting: String,
    /// Where a previous, paused attempt got to. `Default` for a new meeting.
    pub resume: ResumePoint,
}

/// Segments from a paused run, so resuming appends instead of starting over.
///
/// `.partial` like the model download, and for the same reason: a file that is
/// not yet the real thing must not be mistaken for it. `transcript.txt` is
/// written only when the transcription is complete.
const PARTIAL_SEGMENTS: &str = "transcript.partial.json";
/// Per-chunk summary extractions from a paused run.
const PARTIAL_SUMMARY: &str = "summary.partial.json";

fn read_partial<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_partial<T: serde::Serialize>(path: &Path, value: &T) {
    if let Ok(text) = serde_json::to_string(value) {
        let _ = std::fs::write(path, text);
    }
}

/// Where to pick a paused meeting up from.
///
/// All three default to zero, which is "start at the beginning" — so a fresh
/// meeting and a resumed one go down exactly the same code path.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ResumePoint {
    pub mic_offset_seconds: f64,
    pub system_offset_seconds: f64,
    pub summary_chunk: usize,
}

/// How a run ended.
///
/// Pausing is not an error and must not be reported as one: it is the user
/// getting what they asked for. Making it a variant of the success type rather
/// than a `PipelineError` is what keeps that distinction from being lost at
/// every `?` between here and the UI.
pub enum RunOutcome {
    Finished(PipelineOutput),
    /// Stopped on request, with everything done so far already on disk.
    Paused(ResumePoint),
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
    control: &whisper::TranscriptionControl,
    mut on_progress: impl FnMut(Progress),
) -> Result<RunOutcome, PipelineError> {
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

    // Only the microphone's length is needed here: it is where the system
    // track's share of the meeting's timeline begins. The queue worker measures
    // both itself, to size the bar.
    let mic_duration = wav_duration(&mic_file).unwrap_or(0.0);

    // Both tracks already share one origin: `RecordingSession::start` stamps a
    // single `Instant` and hands it to both recorders, and each track's
    // lead-in silence covers its own open latency. So `common_start` — which
    // the Python had to compute from two separate stamps (`app.py:2280`) — is
    // zero here by construction, and both offsets are zero.
    let partial_segments_file = folder.join(PARTIAL_SEGMENTS);
    let partial_summary_file = folder.join(PARTIAL_SUMMARY);

    // Anything a previous, paused attempt already transcribed. Empty for a new
    // meeting, which is why resuming needs no special case below.
    let mut segments: Vec<Segment> = read_partial(&partial_segments_file);
    let mut resume = config.resume;

    // No percentage here, deliberately.
    //
    // `full()` blocks this thread for the whole file, so this line is written
    // once and cannot be updated. It used to carry a number, which meant it
    // announced "0%" and sat there for the length of the track — a progress
    // report that never progresses is worse than none, because it reads as a
    // stall. The live figure is `control.seconds_done()`, which the queue worker
    // polls from another thread and shows on the meeting's card.
    on_progress(Progress::Status(format!(
        "Transcribing {}...",
        config.speaker_me
    )));

    // The microphone track opens the meeting's timeline.
    control.set_base_seconds(0.0);
    let mic = transcriber.transcribe(
        &mic_file,
        &config.speaker_me,
        resume.mic_offset_seconds,
        control,
    )?;
    segments.extend(mic.segments);
    resume.mic_offset_seconds = mic.last_end_seconds;

    if mic.aborted {
        // Written before returning, so the work survives a quit as well as a
        // pause. Nothing distinguishes the two by the time the app restarts.
        write_partial(&partial_segments_file, &segments);
        return Ok(RunOutcome::Paused(resume));
    }

    on_progress(Progress::Status(format!(
        "Transcribing {}...",
        config.speaker_meeting
    )));

    // The system track continues it, so progress keeps climbing instead of
    // restarting when the first track finishes.
    control.set_base_seconds(mic_duration);
    let system = transcriber.transcribe(
        &system_file,
        &config.speaker_meeting,
        resume.system_offset_seconds,
        control,
    )?;
    segments.extend(system.segments);
    resume.system_offset_seconds = system.last_end_seconds;

    if system.aborted {
        write_partial(&partial_segments_file, &segments);
        return Ok(RunOutcome::Paused(resume));
    }

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

    let summary = match summarize(
        &transcript,
        &config,
        resume.summary_chunk,
        &partial_summary_file,
        control,
        &mut on_progress,
    )? {
        Some(summary) => summary,
        None => {
            resume.summary_chunk = read_partial::<String>(&partial_summary_file).len();
            return Ok(RunOutcome::Paused(resume));
        }
    };
    std::fs::write(&summary_file, &summary)?;

    on_progress(Progress::Stage(Stage::Summary, StageState::Done));

    // Both artifacts exist, so the working files have nothing left to protect.
    let _ = std::fs::remove_file(&partial_segments_file);
    let _ = std::fs::remove_file(&partial_summary_file);

    // --- cleanup ---------------------------------------------------------
    //
    // Only after both artifacts exist. Deleting the audio earlier would make a
    // failure at any later step unrecoverable — the meeting would be gone.
    if !config.keep_audio {
        for file in [&mic_file, &system_file] {
            let _ = std::fs::remove_file(file);
        }
    }

    Ok(RunOutcome::Finished(PipelineOutput {
        folder,
        transcript_file,
        summary_file,
        segment_count: segments.len(),
    }))
}

/// Chunk, extract per chunk, then synthesize. Port of `summarize_with_ollama`.
/// `Ok(None)` means paused, not failed — see [`RunOutcome`].
///
/// The chunk loop is the natural place to stop: each iteration is one request,
/// so pausing costs at most one chunk of repeated work and never interrupts a
/// request mid-flight. Extractions completed so far are written to disk after
/// every chunk, so a pause here — or a quit, or a crash — resumes from the next
/// one rather than re-summarising the whole meeting.
fn summarize(
    transcript: &str,
    config: &PipelineConfig,
    start_chunk: usize,
    partial_file: &Path,
    control: &whisper::TranscriptionControl,
    on_progress: &mut impl FnMut(Progress),
) -> Result<Option<String>, PipelineError> {
    let chunks = text::split_transcript(transcript, text::DEFAULT_CHUNK_CHARS);
    let system = prompts::system_prompt(config.language);

    let mut partials: Vec<String> = read_partial(partial_file);
    // Trust the file over the recorded index if they ever disagree: the file is
    // what the next stage actually consumes.
    partials.truncate(start_chunk.min(chunks.len()));
    let total = chunks.len();

    for (index, chunk) in chunks.iter().enumerate().skip(partials.len()) {
        if control.is_aborted() {
            write_partial(partial_file, &partials);
            return Ok(None);
        }

        on_progress(Progress::Status(format!(
            "Summarizing block {}/{total} with {}...",
            index + 1,
            config.provider.ollama_model
        )));

        let user = prompts::extraction_message(config.language, chunk);
        let started = std::time::Instant::now();
        let extracted = summary::chat(&config.provider, system, &user)?;
        debug_timing("extract", chunk.len(), extracted.len(), started);
        partials.push(extracted);
        write_partial(partial_file, &partials);
    }

    if control.is_aborted() {
        write_partial(partial_file, &partials);
        return Ok(None);
    }

    on_progress(Progress::Status("Generating the final summary...".into()));

    let combined = prompts::combine_partials(&partials);
    let user = prompts::final_message(
        config.language,
        config.summary_type,
        &config.custom_summary_prompt,
        &combined,
    );

    let started = std::time::Instant::now();
    let final_summary = summary::chat(&config.provider, system, &user)?;
    debug_timing("final", user.len(), final_summary.len(), started);
    Ok(Some(final_summary))
}

/// Per-request timing for the summary stage, under `MA_DEBUG=1`.
///
/// A stage total cannot say whether the time went into one long request or
/// several, which is the difference between a slow model and too many calls.
fn debug_timing(label: &str, sent: usize, received: usize, started: std::time::Instant) {
    if std::env::var("MA_DEBUG").as_deref() == Ok("1") {
        eprintln!(
            "[llm] {label}: {:.1}s  sent {sent} chars, got {received}",
            started.elapsed().as_secs_f64()
        );
    }
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

    // Already named. A meeting that was paused and resumed comes back through
    // here with the folder the FIRST pass renamed, and appending the title again
    // gave `2026-09-07_14-03-22_Standup_Standup` — once more on every
    // pause. Idempotent here rather than gated by the caller, because this is
    // the function that knows what the folder is called.
    if !title.is_empty() && !moving && base.ends_with(&format!("_{title}")) {
        return Ok(folder.to_path_buf());
    }

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

/// Exposed so the queue worker can size a progress bar without opening the
/// pipeline: it needs the total audio length to turn `TranscriptionControl`'s
/// live position into a percentage.
pub fn wav_duration(path: &Path) -> Option<f64> {
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
    fn a_resumed_meeting_is_not_renamed_twice() {
        // Found by `queue_lifecycle`: every pause and resume appended the title
        // again, so a meeting paused three times became `..._Standup_Standup_Standup`
        // and the state file could no longer be found where it was left.
        let dir = temp_dir("resumed-rename");
        let folder = dir.join("2026-09-07_14-03-22");
        std::fs::create_dir_all(&folder).expect("mkdir");

        let first = rename_folder(&folder, "Standup", Path::new("")).expect("first pass");
        assert_eq!(first.file_name().unwrap(), "2026-09-07_14-03-22_Standup");

        let second = rename_folder(&first, "Standup", Path::new("")).expect("resumed pass");
        assert_eq!(
            second, first,
            "resuming must leave the folder where the first pass put it"
        );
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
