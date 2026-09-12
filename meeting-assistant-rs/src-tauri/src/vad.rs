//! Voice activity detection, so whisper only ever sees speech.
//!
//! # The problem this exists for
//!
//! Whisper is a sequence model: hand it thirty seconds of anything and it emits
//! text, because emitting text is all it does. Handed silence it invents the
//! most plausible sentence it can. A real user's microphone track, measured:
//!
//! ```text
//! duration   90.85s
//! peak       0.027130  (-31.3 dBFS)
//! non-zero   169,003 of 4,360,907 samples  ->  3.9%
//! ```
//!
//! 96.1% exactly zero — they listen on a headset, so the microphone captures
//! nothing. It cost 8.9s of transcription and produced **25 segments, every one
//! fabricated**, which then reached the summary. Against ~14s for a real 90s
//! speech track that is roughly 39% of the meeting's transcription time, spent
//! entirely on invention.
//!
//! `whisper::SILENCE_PEAK` should have caught it, but it takes the peak of the
//! **whole file**, and one headset bump reaching 0.027 is 27x the threshold. A
//! single transient vetoes ninety seconds of silence.
//!
//! whisper.cpp's own `no_speech_thold` cannot help: whisper-rs documents it as
//! "currently (as of v1.3.0) not implemented". Rejection has to happen before
//! whisper sees the audio, which is what this module does.
//!
//! # Why the standalone API and not `FullParams::enable_vad`
//!
//! Because that one does nothing here. whisper.cpp applies VAD only inside
//! `whisper_full` and `whisper_full_parallel`; this codebase calls
//! `WhisperState::full`, which reaches `whisper_full_with_state`, and that never
//! looks at `params.vad`. whisper-rs binds no context-level `whisper_full` at
//! all. Setting those parameters would have changed nothing, silently.

use std::path::PathBuf;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

/// Where the silero VAD weights come from.
///
/// A **different** Hugging Face repository from the Whisper weights, which is
/// why this cannot reuse `whisper::BASE_URL` or live in `whisper::MODELS`. It
/// must also stay out of `MODELS` for a second reason: everything there appears
/// in the Settings model picker, and a VAD model offered as something to
/// transcribe with is a visible bug.
const MODEL_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";

/// SHA-256 of the file at [`MODEL_URL`], 885,098 bytes.
///
/// From the Hugging Face LFS pointer, transcribed 2026-09-12 and confirmed
/// against an independent fetch of the same file.
///
/// `whisper::MODELS` pins **two** digests from two publishers, so substituting a
/// model would mean compromising both. That is not possible here:
/// whisper.cpp's `models/README.md` lists no silero row, so there is no second
/// publisher to pin against. One digest, honestly described, rather than a
/// second one invented to satisfy the pattern.
const MODEL_SHA256: &str = "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf";

const MODEL_FILE: &str = "ggml-silero-v5.1.2.bin";

/// Gaps shorter than this are kept rather than skipped.
///
/// Not a quality knob — an arithmetic one. whisper decodes in 30-second
/// windows, so a run of any length costs at least one window. Splitting two
/// 5-second utterances separated by 10 seconds of silence costs two windows;
/// keeping them together costs one. Splitting only pays once the gap approaches
/// a window in its own right.
const MERGE_GAP_SECONDS: f64 = 30.0;

/// whisper.cpp returns 0 segments, with only a log line, for input below 100ms
/// (`whisper.cpp:6845-6848`). Asking it is a waste either way.
const MIN_RUN_SECONDS: f64 = 0.1;

/// VAD runs one tiny graph per 512-sample chunk — about 113,000 of them for an
/// hour of audio. With `GGML_OPENMP=OFF` ggml's thread pool spin-waits at
/// barriers, so a high thread count here means a great many threads contending
/// at a great many barriers; this is the opposite shape of work from
/// transcription, and deliberately does **not** use `policy::whisper_threads`.
/// Four is whisper.cpp's own default for VAD.
const VAD_THREADS: i32 = 4;

/// A stretch of audio to hand to whisper, as sample indices into the track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub start: usize,
    pub end: usize,
}

impl Run {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
pub enum VadError {
    Model(String),
    Detect(String),
}

impl std::fmt::Display for VadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model(e) => write!(f, "voice activity model unavailable: {e}"),
            Self::Detect(e) => write!(f, "voice activity detection failed: {e}"),
        }
    }
}

/// The model file, downloading it once if it is not already there.
///
/// # Why this is downloaded rather than bundled
///
/// It was briefly embedded with `include_bytes!`. At 885 KB that is invisible in
/// a download, and shipping it inside the binary makes us a **distributor** of
/// someone else's MIT-licensed work — which attaches a notice obligation that
/// then has to reach every end user, not just the repository. Fetching it puts
/// the copy in the user's hands directly, exactly as the Whisper weights
/// already are, and that obligation does not arise.
///
/// # Why the care with the bytes
///
/// A corrupt file of the right length does not fail gracefully.
/// `whisper_vad_init_with_params` reads `n_encoder_layers` from the header and
/// allocates from it with no sanity bound, and tensor creation can
/// `throw std::runtime_error`. That exception unwinds out of an `extern "C"`
/// boundary into Rust, which is an **abort**, not something to fall back from.
/// So the digest is checked before whisper.cpp is allowed near the file.
///
/// Written to a temporary file and renamed, the shape `whisper::download_model`
/// already uses: `rec` and `process` can run beside the app, and
/// `single-instance` does not cover them, so two processes may race here.
fn model_path() -> Result<&'static PathBuf, VadError> {
    static PATH: OnceLock<Result<PathBuf, String>> = OnceLock::new();

    PATH.get_or_init(|| {
        let dir = crate::platform::models_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let target = dir.join(MODEL_FILE);

        if digest_of(&target).as_deref() == Some(MODEL_SHA256) {
            return Ok(target);
        }

        let bytes = crate::whisper::fetch(MODEL_URL).map_err(|e| e.to_string())?;

        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let got = hex(&hasher.finalize());
        if got != MODEL_SHA256 {
            return Err(format!(
                "the downloaded model does not match its digest (expected {MODEL_SHA256}, got {got})"
            ));
        }

        // `.part`, then rename: a reader can never observe a half-written file,
        // and a loser in a race overwrites with identical, verified bytes.
        let part = dir.join(format!("{MODEL_FILE}.{}.part", std::process::id()));
        std::fs::write(&part, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&part, &target).map_err(|e| e.to_string())?;

        Ok(target)
    })
    .as_ref()
    .map_err(|e| VadError::Model(e.clone()))
}

/// Fetch the model now, so the first meeting does not wait for it.
///
/// Called at startup and deliberately ignores its result: without the model,
/// transcription falls back to the whole track, which is slower but correct. A
/// machine that is offline at launch must still be able to record a meeting.
pub fn prefetch() {
    if let Err(e) = model_path() {
        eprintln!("[vad] {e}");
    }
}

fn digest_of(path: &PathBuf) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The stretches of `samples` that contain speech, ready to hand to whisper.
///
/// `samples` is 16 kHz mono, which is what silero expects and what
/// `read_wav_as_16k_mono` already produces.
pub fn speech_runs(
    samples: &[f32],
    resume_from_seconds: f64,
    sample_rate: u32,
) -> Result<Vec<Run>, VadError> {
    let path = model_path()?;
    let path = path
        .to_str()
        .ok_or_else(|| VadError::Model("the model path is not valid UTF-8".into()))?;

    let mut params = WhisperVadContextParams::new();
    params.set_n_threads(VAD_THREADS);

    let mut context = WhisperVadContext::new(path, params)
        .map_err(|e| VadError::Model(format!("{e:?}")))?;

    let segments = context
        .segments_from_samples(WhisperVadParams::new(), samples)
        .map_err(|e| VadError::Detect(format!("{e:?}")))?;

    // Centiseconds, at 16 kHz, per `samples_to_cs` in whisper.cpp.
    let mut spans = Vec::with_capacity(segments.num_segments().max(0) as usize);
    for i in 0..segments.num_segments() {
        if let (Some(start), Some(end)) = (
            segments.get_segment_start_timestamp(i),
            segments.get_segment_end_timestamp(i),
        ) {
            spans.push((start as f64 / 100.0, end as f64 / 100.0));
        }
    }

    Ok(plan_runs(
        &spans,
        resume_from_seconds,
        samples.len(),
        sample_rate,
    ))
}

/// Turn speech spans into the runs whisper will be asked to transcribe.
///
/// Pure, and separated from the model for exactly that reason: every rule below
/// came from a specific failure, and none of them can be tested if testing them
/// requires an 885 KB model and a speech fixture.
///
/// * **Merge** across gaps below [`MERGE_GAP_SECONDS`] — see that constant.
/// * **Clamp the end to the buffer.** VAD reports against the sample count
///   rounded *up* to a 512-sample chunk, so the last span can end past the real
///   end of the audio. Left alone, whisper would then decode windows of pure
///   zero padding — producing exactly the phantom segments this module exists
///   to remove.
/// * **Clamp the start to the resume point**, rather than dropping runs that
///   end before it. A run aborted half-way through would otherwise be replayed
///   from its beginning, and `pipeline::run` extends the partial transcript
///   without deduplicating, so every sentence already transcribed would appear
///   twice.
/// * **Drop runs below [`MIN_RUN_SECONDS`]**, which whisper refuses anyway.
pub fn plan_runs(
    spans: &[(f64, f64)],
    resume_from_seconds: f64,
    total_samples: usize,
    sample_rate: u32,
) -> Vec<Run> {
    let rate = sample_rate.max(1) as f64;
    let to_samples = |seconds: f64| (seconds.max(0.0) * rate) as usize;

    let resume = to_samples(resume_from_seconds);
    let min_run = to_samples(MIN_RUN_SECONDS);
    let merge_gap = to_samples(MERGE_GAP_SECONDS);

    let mut runs: Vec<Run> = Vec::new();

    for &(start, end) in spans {
        let start = to_samples(start).max(resume);
        let end = to_samples(end).min(total_samples);

        if end <= start {
            continue;
        }

        match runs.last_mut() {
            Some(last) if start.saturating_sub(last.end) < merge_gap => last.end = end.max(last.end),
            _ => runs.push(Run { start, end }),
        }
    }

    runs.retain(|run| run.len() >= min_run);
    runs
}

/// One line for `meeting.json`, so a run reports its own saving.
pub fn summary(runs: &[Run], total_samples: usize, sample_rate: u32) -> String {
    let rate = sample_rate.max(1) as f64;
    let kept: usize = runs.iter().map(Run::len).sum();

    format!(
        "kept {:.1}s of {:.1}s in {} run{}",
        kept as f64 / rate,
        total_samples as f64 / rate,
        runs.len(),
        if runs.len() == 1 { "" } else { "s" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn seconds(n: f64) -> usize {
        (n * RATE as f64) as usize
    }

    /// Run the real model over a real file, and say what it found.
    ///
    /// Ignored by default: it loads the bundled model and needs a WAV, neither
    /// of which belongs in the build gate. It exists because every constant in
    /// this module — centiseconds, the merge gap, the thread count — is a claim
    /// about whisper.cpp's behaviour, and this is the cheapest place to find out
    /// that one of them is wrong.
    ///
    /// ```text
    /// VAD_PROBE_WAV=/path/to/microphone.wav cargo test -p meeting-assistant \
    ///     vad::tests::probe -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a WAV in VAD_PROBE_WAV"]
    fn probe() {
        let path = std::env::var("VAD_PROBE_WAV").expect("set VAD_PROBE_WAV");
        let samples = crate::whisper::read_wav_as_16k_mono(std::path::Path::new(&path))
            .expect("read the wav");
        let total = samples.len() as f64 / meeting_core::convert::WHISPER_SAMPLE_RATE as f64;

        let started = std::time::Instant::now();
        let runs = speech_runs(&samples, 0.0, meeting_core::convert::WHISPER_SAMPLE_RATE)
            .expect("run the vad");
        let elapsed = started.elapsed();

        println!("audio      : {total:.2}s");
        println!("vad took   : {:.3}s", elapsed.as_secs_f64());
        println!("{}", summary(&runs, samples.len(), meeting_core::convert::WHISPER_SAMPLE_RATE));
        for run in &runs {
            println!(
                "  run {:.2}s -> {:.2}s",
                run.start as f64 / meeting_core::convert::WHISPER_SAMPLE_RATE as f64,
                run.end as f64 / meeting_core::convert::WHISPER_SAMPLE_RATE as f64
            );
        }
    }

    #[test]
    fn the_pinned_digest_is_well_formed() {
        // Not a substitute for the download-time check, which is where the real
        // verification happens — this only catches a typo in the constant,
        // which would otherwise present as every machine failing to fetch it.
        assert_eq!(MODEL_SHA256.len(), 64);
        assert!(MODEL_SHA256.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(MODEL_SHA256, MODEL_SHA256.to_ascii_lowercase());
    }

    #[test]
    fn a_track_with_no_speech_produces_no_runs() {
        // The case the whole module exists for: a microphone that captured
        // nothing, which today costs 39% of a meeting's transcription and
        // returns 25 fabricated sentences.
        assert!(plan_runs(&[], 0.0, seconds(90.0), RATE).is_empty());
    }

    #[test]
    fn close_spans_are_merged_into_one_run() {
        // Two utterances 10s apart cost two whisper windows if split and one if
        // merged, so merging is cheaper as well as simpler.
        let runs = plan_runs(&[(0.0, 5.0), (15.0, 20.0)], 0.0, seconds(90.0), RATE);
        assert_eq!(runs, vec![Run { start: 0, end: seconds(20.0) }]);
    }

    #[test]
    fn a_long_silence_splits_the_runs() {
        let runs = plan_runs(&[(0.0, 5.0), (60.0, 65.0)], 0.0, seconds(90.0), RATE);
        assert_eq!(
            runs,
            vec![
                Run { start: 0, end: seconds(5.0) },
                Run { start: seconds(60.0), end: seconds(65.0) },
            ],
            "60s of silence is worth skipping; it is two whisper windows"
        );
    }

    #[test]
    fn the_last_run_cannot_end_past_the_audio() {
        // VAD measures against the sample count rounded up to a 512-sample
        // chunk, so it can report an end beyond the real end of the file.
        // Unclamped, whisper would decode windows of zero padding and invent
        // segments for them.
        let total = seconds(90.0);
        let runs = plan_runs(&[(80.0, 90.1)], 0.0, total, RATE);
        assert_eq!(runs, vec![Run { start: seconds(80.0), end: total }]);
    }

    #[test]
    fn a_run_containing_the_resume_point_restarts_from_it() {
        // The mid-run abort. Keeping the run whole would re-transcribe
        // everything between its start and the pause, and the pipeline appends
        // to the partial transcript without deduplicating — so every sentence
        // already captured would appear a second time.
        let runs = plan_runs(&[(10.0, 40.0)], 25.0, seconds(90.0), RATE);
        assert_eq!(runs, vec![Run { start: seconds(25.0), end: seconds(40.0) }]);
    }

    #[test]
    fn work_already_done_is_not_repeated() {
        let runs = plan_runs(&[(0.0, 20.0), (60.0, 70.0)], 55.0, seconds(90.0), RATE);
        assert_eq!(
            runs,
            vec![Run { start: seconds(60.0), end: seconds(70.0) }],
            "a run entirely before the resume point must not be transcribed again"
        );
    }

    #[test]
    fn a_span_too_short_to_transcribe_is_dropped() {
        // whisper.cpp returns zero segments below 100ms, with only a log line.
        assert!(plan_runs(&[(10.0, 10.05)], 0.0, seconds(90.0), RATE).is_empty());
    }

    #[test]
    fn the_summary_says_what_was_kept() {
        let runs = vec![Run { start: 0, end: seconds(12.4) }];
        assert_eq!(
            summary(&runs, seconds(90.9), RATE),
            "kept 12.4s of 90.9s in 1 run"
        );
        assert_eq!(summary(&[], seconds(90.9), RATE), "kept 0.0s of 90.9s in 0 runs");
    }
}
