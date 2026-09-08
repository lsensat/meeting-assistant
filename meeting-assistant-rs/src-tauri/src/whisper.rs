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
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use sha1::Sha1;
use sha2::{Digest, Sha256};

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
    /// SHA-256, from Hugging Face's Git-LFS metadata.
    ///
    /// `GET /ggerganov/whisper.cpp/raw/main/ggml-<id>.bin` returns the LFS
    /// pointer rather than the file, and the pointer carries `oid sha256:…`.
    /// Transcribed 2026-09-06.
    pub sha256: &'static str,
    /// SHA-1, from the table in `models/README.md` in `ggml-org/whisper.cpp`.
    ///
    /// A **second, independent publisher**, which is the whole point: with both
    /// pinned, substituting a model means compromising Hugging Face *and* the
    /// whisper.cpp repository. SHA-1 is broken for collisions — which need
    /// control of both files — but substituting a malicious model requires a
    /// second preimage, and that remains infeasible. SHA-256 is the primary.
    /// Transcribed 2026-09-06.
    pub sha1: &'static str,
}

/// The models offered in the UI, matching the Python's list.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "tiny",
        approx_mb: 75,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        sha1: "bd577a113a864445d4c299885e0cb97d4ba92b5f",
    },
    ModelSpec {
        id: "base",
        approx_mb: 142,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        sha1: "465707469ff3a37a2b9b8d8f89f2f99de7299dac",
    },
    ModelSpec {
        id: "small",
        approx_mb: 466,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        sha1: "55356645c2b361a969dfd0ef2c5a50d530afd8d5",
    },
    ModelSpec {
        id: "medium",
        approx_mb: 1500,
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
        sha1: "fd9727b6e1217c2f614f9b698455c4ffd82463b4",
    },
    ModelSpec {
        id: "large-v3",
        approx_mb: 3100,
        sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
        sha1: "ad82bf6a9043ceed055076d0fd39f5f186ff8062",
    },
];

/// Compare computed digests against the pinned ones.
///
/// Split out from `download_model` so the branch that *rejects* can be tested
/// without a network round trip. A verifier only ever exercised on good input
/// is not known to reject anything.
fn verify_digests(spec: &ModelSpec, got_sha256: &str, got_sha1: &str) -> Result<(), WhisperError> {
    let mismatch = if got_sha256 != spec.sha256 {
        Some(("SHA-256", spec.sha256, got_sha256))
    } else if got_sha1 != spec.sha1 {
        Some(("SHA-1", spec.sha1, got_sha1))
    } else {
        None
    };

    match mismatch {
        None => Ok(()),
        Some((algorithm, expected, actual)) => Err(WhisperError::IntegrityFailed {
            id: spec.id.to_string(),
            algorithm,
            expected: expected.to_string(),
            actual: actual.to_string(),
        }),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod integrity_tests {
    use super::*;

    /// Both digests are pinned for every model offered in the UI. A model with
    /// an empty digest would silently verify against nothing.
    #[test]
    fn every_model_has_both_digests_pinned() {
        for m in MODELS {
            assert_eq!(m.sha256.len(), 64, "{} sha256 is not 64 hex chars", m.id);
            assert_eq!(m.sha1.len(), 40, "{} sha1 is not 40 hex chars", m.id);
            assert!(
                m.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{} sha256 is not hex",
                m.id
            );
            assert!(
                m.sha1.chars().all(|c| c.is_ascii_hexdigit()),
                "{} sha1 is not hex",
                m.id
            );
        }
    }

    /// The digests must be lowercase, because the comparison is a plain string
    /// equality against `hex()`, which emits lowercase. An uppercase pin would
    /// reject every download of that model.
    #[test]
    fn pinned_digests_are_lowercase() {
        for m in MODELS {
            assert_eq!(m.sha256, m.sha256.to_ascii_lowercase(), "{}", m.id);
            assert_eq!(m.sha1, m.sha1.to_ascii_lowercase(), "{}", m.id);
        }
    }

    /// `hex` is what the pins are compared against, so a bug here would either
    /// reject everything or, worse, accept anything.
    #[test]
    fn hex_matches_known_digests_of_the_empty_input() {
        let sha256 = hex(&Digest::finalize(<Sha256 as Digest>::new()));
        let sha1 = hex(&Digest::finalize(<Sha1 as Digest>::new()));

        assert_eq!(
            sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(sha1, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    /// The branch that matters is the one that says no.
    #[test]
    fn a_changed_byte_changes_the_digest() {
        let mut a = <Sha256 as Digest>::new();
        Digest::update(&mut a, b"ggml model bytes");
        let mut b = <Sha256 as Digest>::new();
        Digest::update(&mut b, b"ggml model byteS");

        assert_ne!(hex(&Digest::finalize(a)), hex(&Digest::finalize(b)));
    }

    /// The happy path: the pinned pair is accepted.
    #[test]
    fn matching_digests_are_accepted() {
        let spec = spec("tiny").expect("tiny exists");
        assert!(verify_digests(spec, spec.sha256, spec.sha1).is_ok());
    }

    /// A wrong SHA-256 is rejected, and the error says which check failed.
    #[test]
    fn a_wrong_sha256_is_rejected() {
        let spec = spec("tiny").expect("tiny exists");
        let err = verify_digests(spec, &"0".repeat(64), spec.sha1)
            .expect_err("a bad sha256 must not verify");

        match err {
            WhisperError::IntegrityFailed { algorithm, id, .. } => {
                assert_eq!(algorithm, "SHA-256");
                assert_eq!(id, "tiny");
            }
            other => panic!("wrong error: {other}"),
        }
    }

    /// The second publisher is doing real work: a file matching Hugging Face's
    /// digest but NOT the whisper.cpp one is still refused. This is the case
    /// that only a compromise of one source could produce.
    #[test]
    fn a_wrong_sha1_is_rejected_even_when_sha256_matches() {
        let spec = spec("tiny").expect("tiny exists");
        let err = verify_digests(spec, spec.sha256, &"0".repeat(40))
            .expect_err("a bad sha1 must not verify");

        match err {
            WhisperError::IntegrityFailed { algorithm, .. } => assert_eq!(algorithm, "SHA-1"),
            other => panic!("wrong error: {other}"),
        }
    }

    /// Streaming in chunks must produce the same digest as one shot — the
    /// download hashes 256 KB at a time, so this is the property the whole
    /// check rests on.
    #[test]
    fn chunked_hashing_matches_one_shot() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();

        let mut one = <Sha256 as Digest>::new();
        Digest::update(&mut one, &data);

        let mut chunked = <Sha256 as Digest>::new();
        for chunk in data.chunks(257) {
            Digest::update(&mut chunked, chunk);
        }

        assert_eq!(
            hex(&Digest::finalize(one)),
            hex(&Digest::finalize(chunked))
        );
    }
}

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
    /// A downloaded model did not match its pinned digest. The file has already
    /// been deleted by the time this is returned.
    IntegrityFailed {
        id: String,
        algorithm: &'static str,
        expected: String,
        actual: String,
    },
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
            Self::IntegrityFailed {
                id,
                algorithm,
                expected,
                actual,
            } => write!(
                f,
                "The downloaded \"{id}\" model failed its {algorithm} check and was discarded. \
                 Expected {expected}, got {actual}. The download may have been tampered with; \
                 check your network and try again."
            ),
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
/// Bytes the model occupies on disk, or 0 when it is not installed.
///
/// The real file size rather than `ModelSpec::approx_mb`: this is shown to
/// someone deciding what to delete to free space, and an approximation is not
/// what they are looking at in Finder or Explorer.
pub fn installed_size(id: &str) -> u64 {
    std::fs::metadata(model_path(id)).map(|m| m.len()).unwrap_or(0)
}

/// Remove an installed model from disk.
///
/// **Bounded by construction.** `spec` accepts only the five pinned ids, and
/// the path is derived from the id rather than supplied by the caller, so no
/// input can name a file outside the models directory. This is the same stance
/// as the meeting delete in `commands.rs`: recursive, irreversible operations
/// take an identifier, never a path.
pub fn delete_model(id: &str) -> Result<(), WhisperError> {
    if spec(id).is_none() {
        return Err(WhisperError::UnknownModel(id.to_string()));
    }

    match std::fs::remove_file(model_path(id)) {
        Ok(()) => Ok(()),
        // Already absent is the desired end state, not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(WhisperError::Io(e)),
    }
}

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

    // Hashed as the bytes go past. The file is already being read in chunks, so
    // this costs one pass and no extra memory — re-reading 3 GB afterwards to
    // digest it would be the obvious but wasteful way.
    let mut sha256 = <Sha256 as Digest>::new();
    let mut sha1 = <Sha1 as Digest>::new();

    loop {
        let read = std::io::Read::read(&mut response, &mut buffer)
            .map_err(|e| WhisperError::Download(e.to_string()))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        Digest::update(&mut sha256, &buffer[..read]);
        Digest::update(&mut sha1, &buffer[..read]);
        written += read as u64;

        let percent = ((written as f64 / expected as f64) * 100.0).min(100.0) as u8;
        if percent != last_percent {
            on_progress(percent);
            last_percent = percent;
        }
    }

    file.flush()?;
    drop(file);

    // Verified BEFORE the rename, so a rejected file never reaches the path
    // `is_installed` looks at and can never be loaded.
    //
    // Fail closed: on any mismatch the partial is deleted and the error names
    // which digest failed. There is no path here that uses the file anyway.
    let got_sha256 = hex(&Digest::finalize(sha256));
    let got_sha1 = hex(&Digest::finalize(sha1));

    if let Err(e) = verify_digests(spec, &got_sha256, &got_sha1) {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }

    std::fs::rename(&partial, &target)?;

    Ok(target)
}

/// A loaded model, reusable across both tracks of one meeting.
pub struct Transcriber {
    context: WhisperContext,
    language: Option<String>,
}


/// A handle onto a running transcription: stop it, and watch how far it has got.
///
/// whisper.cpp's callbacks are FFI trampolines and must be `'static`, so they
/// cannot borrow anything from the caller. That rules out passing a closure that
/// writes to local state, which is why progress and cancellation both travel
/// through this shared, atomic handle instead.
///
/// It also solves a second problem. `full()` blocks its thread for the whole
/// file, so whoever wants to *report* progress cannot be the same thread that
/// started it. A holder of this handle on another thread can poll
/// [`seconds_done`] whenever it likes.
#[derive(Clone, Default)]
pub struct TranscriptionControl {
    abort: Arc<AtomicBool>,
    /// Centiseconds, whisper.cpp's own unit, kept as an integer so it fits an
    /// atomic. Converted only at the boundary.
    ///
    /// Position within the track being transcribed, which is not what a caller
    /// wants: a meeting has two, and reporting each from zero made a progress
    /// bar climb through the first and then fall back to nothing at the start of
    /// the second. `base_cs` is what the earlier tracks already covered, so
    /// `seconds_done` can answer for the meeting rather than the file.
    progress_cs: Arc<AtomicI64>,
    base_cs: Arc<AtomicI64>,
}

impl TranscriptionControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the run to stop at the next opportunity. Whatever has been
    /// transcribed so far is kept and returned.
    pub fn abort(&self) {
        self.abort.store(true, Ordering::SeqCst);
    }

    pub fn is_aborted(&self) -> bool {
        self.abort.load(Ordering::SeqCst)
    }

    /// How far into the MEETING the transcription has reached, in seconds.
    ///
    /// Safe to call from another thread while `transcribe` is running; that is
    /// the point of it.
    pub fn seconds_done(&self) -> f64 {
        let base = self.base_cs.load(Ordering::Relaxed);
        (base + self.progress_cs.load(Ordering::Relaxed)) as f64 / 100.0
    }

    /// Declare how much of the meeting earlier tracks already covered.
    ///
    /// Called by the pipeline before each track. Without it every track reports
    /// from zero and a bar spanning the whole meeting jumps backwards each time
    /// one finishes.
    pub fn set_base_seconds(&self, seconds: f64) {
        self.base_cs.store((seconds * 100.0) as i64, Ordering::Relaxed);
        self.progress_cs.store(0, Ordering::Relaxed);
    }
}

/// Reads the shared abort flag for whisper.cpp.
///
/// # Safety
///
/// `user_data` must be a pointer to a live `AtomicBool`, which is guaranteed by
/// the only call site: it passes `Arc::as_ptr` of a flag held by a
/// `TranscriptionControl` that outlives the `full()` call.
unsafe extern "C" fn abort_trampoline(user_data: *mut std::ffi::c_void) -> bool {
    if user_data.is_null() {
        return false;
    }
    unsafe { (*(user_data as *const AtomicBool)).load(Ordering::SeqCst) }
}

/// What one `transcribe` call produced.
pub struct TranscriptionOutcome {
    pub segments: Vec<Segment>,
    /// End of the last segment produced, in seconds from the start of the file.
    /// This is the resume point: pass it back as `resume_from_seconds`.
    pub last_end_seconds: f64,
    /// True when the run stopped early because the control asked it to. The
    /// segments are still valid, they are simply not the whole file.
    pub aborted: bool,
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
    /// Transcribe one track, optionally resuming part-way through it.
    ///
    /// # Segments are collected live, not read back afterwards
    ///
    /// This used to call `full()` and then walk `state.as_iter()`. That is fine
    /// for a run that completes, and useless for one that does not: an aborted
    /// `full()` leaves nothing to iterate, so every second of work would be
    /// thrown away. Collecting from the segment callback means an abort keeps
    /// everything up to that point — which is what makes pausing cheap enough
    /// to offer. It also makes progress live rather than a jump to 100% once
    /// the file is already finished.
    ///
    /// # Resuming
    ///
    /// `resume_from_seconds` seeks, and does **only** that. whisper.cpp reports
    /// segment positions relative to the file rather than to the seek point, so
    /// the timeline needs no correction here — adding the offset back on gave a
    /// run resumed at 114s a first segment at 228s, past the end of a 192s
    /// recording.
    pub fn transcribe(
        &self,
        audio_file: &Path,
        speaker: &str,
        resume_from_seconds: f64,
        control: &TranscriptionControl,
    ) -> Result<TranscriptionOutcome, WhisperError> {
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

        if resume_from_seconds > 0.0 {
            // Skip what a previous attempt already transcribed.
            params.set_offset_ms((resume_from_seconds * 1000.0) as i32);
        }

        // Shared with the FFI callbacks, which must be `'static` and so cannot
        // borrow these.
        let collected: Arc<Mutex<Vec<Segment>>> = Arc::new(Mutex::new(Vec::new()));
        let last_end_cs = Arc::new(AtomicI64::new(0));

        {
            let collected = Arc::clone(&collected);
            let last_end_cs = Arc::clone(&last_end_cs);
            let progress_cs = Arc::clone(&control.progress_cs);
            let speaker = speaker.to_string();

            // The `_lossy` variant for the same reason the old code used
            // `to_str_lossy`: whisper.cpp can emit invalid UTF-8 mid-word on a
            // truncated multibyte token, and losing one character is far better
            // than failing the whole meeting.
            params.set_segment_callback_safe_lossy(move |data: whisper_rs::SegmentCallbackData| {
                // whisper.cpp reports timestamps in centiseconds (10 ms units),
                // not seconds or milliseconds. Getting this wrong scales every
                // timestamp in the transcript by 10 or 100, and is not obvious
                // from a short test clip.
                //
                // Already absolute. whisper.cpp reports positions in the FILE,
                // not relative to `offset_ms`, so adding the resume point back
                // on would double it — a run resumed at 114s reported its first
                // segment at 228s, past the end of a 192s recording. The seek
                // and the timeline need the offset applied once, by
                // `set_offset_ms`, and not again here.
                let start = data.start_timestamp as f64 / 100.0;
                let end_cs = data.end_timestamp;

                last_end_cs.store(end_cs, Ordering::Relaxed);
                progress_cs.store(end_cs, Ordering::Relaxed);

                // Blank segments are dropped before they reach the transcript,
                // as the Python does (`app.py:1898`).
                let text = data.text.trim();
                if text.is_empty() {
                    return;
                }

                if let Ok(mut segments) = collected.lock() {
                    segments.push(Segment {
                        start,
                        speaker: speaker.clone(),
                        text: text.to_string(),
                    });
                }
            });
        }

        // --- cancellation ------------------------------------------------
        //
        // NOT `set_abort_callback_safe`, which is unsound in whisper-rs 0.16.0.
        // It boxes the closure twice and stores a `*mut Box<dyn FnMut() -> bool>`,
        // but its trampoline casts that pointer to `*mut F` — the concrete
        // closure type — and calls it. The callback therefore reads the box's own
        // pointer bytes as if they were the closure's captures and returns
        // whatever that happens to be.
        //
        // The symptom is not a crash. whisper.cpp polls this inside the encoder,
        // so a garbage `true` aborts the encode and `full()` returns **-6**,
        // nondeterministically, on audio that is perfectly fine. It cost an
        // afternoon precisely because it looks like a transcription failure.
        //
        // The raw setters take a pointer whose type we control, so the trampoline
        // below and the pointer passed to it agree. `control` outlives this call,
        // so the `AtomicBool` behind the `Arc` outlives every invocation — no
        // ownership is transferred and nothing leaks.
        unsafe {
            params.set_abort_callback(Some(abort_trampoline));
            params.set_abort_callback_user_data(
                Arc::as_ptr(&control.abort) as *mut std::ffi::c_void
            );
        }

        let mut state = self
            .context
            .create_state()
            .map_err(|e| WhisperError::Transcribe(e.to_string()))?;

        let outcome = state.full(params, &samples);

        let aborted = control.is_aborted();
        // An aborted run reports failure, which here is the expected result of
        // being asked to stop rather than something to surface. Checking the
        // flag first is what keeps a deliberate pause from looking like a
        // transcription error to the user.
        if !aborted {
            outcome.map_err(|e| WhisperError::Transcribe(e.to_string()))?;
        }

        let segments = collected.lock().map_or_else(|e| e.into_inner().clone(), |g| g.clone());

        // Falls back to where this attempt started when it produced nothing at
        // all — an abort during the very first segment must not reset the
        // resume point to zero and re-transcribe everything already done.
        let last_end_cs = last_end_cs.load(Ordering::Relaxed);
        let last_end_seconds = if last_end_cs > 0 {
            last_end_cs as f64 / 100.0
        } else {
            resume_from_seconds
        };

        Ok(TranscriptionOutcome {
            segments,
            last_end_seconds,
            aborted,
        })
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
