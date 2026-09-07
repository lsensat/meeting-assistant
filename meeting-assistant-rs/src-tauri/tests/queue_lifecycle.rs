//! Does a paused meeting survive the app closing, and finish when it comes back?
//!
//! `resume.rs` proves the seam in one transcription is sound. This proves the
//! layer above it: that pausing writes enough to disk for a *different process*
//! to pick the meeting up, and that doing so produces a whole meeting rather
//! than half of one.
//!
//! It is the closest an automated test gets to the thing a user does — pause at
//! lunchtime, quit, come back in the evening. The restart is simulated by
//! dropping every in-memory structure and rebuilding from `queue::scan`, which
//! is exactly what `main.rs` does at startup, so nothing carries over that a
//! real relaunch would not.
//!
//! # Running it
//!
//! Ignored by default: it needs a Whisper model, **Ollama running with a
//! model**, real speech, and minutes. See `resume.rs` for how to make the audio.
//!
//! ```sh
//! MA_TEST_AUDIO=/tmp/speech.wav MA_TEST_OLLAMA=gemma4:e4b \
//!   cargo test --release --test queue_lifecycle -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use meeting_assistant::pipeline::{self, PipelineConfig, Progress, ResumePoint, RunOutcome};
use meeting_assistant::queue::{self, MeetingState, Stage};
use meeting_assistant::summary::ProviderConfig;
use meeting_assistant::whisper::TranscriptionControl;
use meeting_core::config::{Language, SummaryProvider, SummaryType};

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "meeting-lifecycle-{name}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn config_for(folder: &Path, output: &Path, title: &str, resume: ResumePoint) -> PipelineConfig {
    PipelineConfig {
        folder: folder.to_path_buf(),
        meeting_title: title.to_string(),
        output_folder: output.to_path_buf(),
        whisper_model: std::env::var("MA_TEST_MODEL").unwrap_or_else(|_| "base".into()),
        transcription_language: Some("en".into()),
        provider: ProviderConfig {
            provider: SummaryProvider::Ollama,
            ollama_model: std::env::var("MA_TEST_OLLAMA")
                .expect("set MA_TEST_OLLAMA to an installed Ollama model"),
            api_base_url: String::new(),
            api_model: String::new(),
        },
        language: Language::En,
        summary_type: SummaryType::MeetingMinutes,
        custom_summary_prompt: String::new(),
        keep_audio: true,
        speaker_me: "ME".into(),
        speaker_meeting: "MEETING".into(),
        resume,
    }
}

/// Abort once the transcription has covered `after_seconds` of audio.
///
/// Keyed on audio position rather than wall-clock so the pause lands in the
/// same place whatever the machine's speed.
fn abort_after(control: &TranscriptionControl, after_seconds: f64) -> Arc<AtomicBool> {
    let watching = Arc::new(AtomicBool::new(true));
    let control = control.clone();
    let flag = Arc::clone(&watching);
    std::thread::spawn(move || {
        while flag.load(Ordering::Relaxed) {
            if control.seconds_done() >= after_seconds {
                control.abort();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    watching
}

#[test]
#[ignore = "needs a Whisper model, Ollama and real audio; see the module docs"]
fn a_paused_meeting_survives_a_restart_and_finishes() {
    let audio = PathBuf::from(std::env::var("MA_TEST_AUDIO").expect("set MA_TEST_AUDIO"));

    let output = temp_root("output");
    let id = "2026-09-07_14-03-22";
    let folder = output.join(id);
    std::fs::create_dir_all(&folder).expect("mkdir");
    for name in ["microphone.wav", "system_audio.wav"] {
        std::fs::copy(&audio, folder.join(name)).expect("copy audio");
    }

    // What `finalize_meeting` writes when the recording stops.
    let state = MeetingState::new(id.into(), "Standup".into(), serde_json::json!({}));
    queue::save(&folder, &state).expect("save");

    // --- session one: pause partway through ------------------------------
    let control = TranscriptionControl::new();
    let watching = abort_after(&control, 20.0);

    let outcome = pipeline::run(
        config_for(&folder, &output, "Standup", ResumePoint::default()),
        &control,
        |_: Progress| {},
    )
    .expect("first pass");
    watching.store(false, Ordering::Relaxed);

    let RunOutcome::Paused(resume) = outcome else {
        panic!("expected the run to pause, not finish");
    };
    assert!(
        resume.mic_offset_seconds > 0.0,
        "a pause must record where it got to, or the work is repeated"
    );

    // The folder is renamed during the audio stage, so the state file has to be
    // written where the meeting IS now, not where it started.
    let folder = output.join(format!("{id}_Standup"));
    assert!(folder.is_dir(), "the audio stage should have renamed the folder");

    let mut paused = queue::load(&folder).expect("state travelled with the rename");
    paused.stage = Stage::Whisper;
    paused.mic_offset_seconds = resume.mic_offset_seconds;
    paused.system_offset_seconds = resume.system_offset_seconds;
    paused.summary_chunk = resume.summary_chunk;
    queue::save(&folder, &paused).expect("persist the pause");

    assert!(
        folder.join("transcript.partial.json").exists(),
        "the segments already transcribed must be on disk, not only in memory"
    );
    assert!(
        !folder.join("transcript.txt").exists(),
        "a partial transcript must never masquerade as a finished one"
    );

    // --- the restart ------------------------------------------------------
    //
    // Everything in memory is dropped. What comes back must come from disk.
    // Nothing from session one may be reachable past this point; what comes
    // back has to come from disk, the way a relaunch would find it.
    drop(paused);

    let found = queue::scan(&output);
    assert_eq!(found.len(), 1, "the scan must find the unfinished meeting");
    let (found_folder, found_state) = &found[0];
    assert_eq!(found_state.id, id);
    assert_eq!(found_state.title, "Standup", "the title must survive the restart");
    assert!(
        found_state.mic_offset_seconds > 0.0,
        "the resume point must survive the restart, or the meeting starts over"
    );

    // --- session two: finish it ------------------------------------------
    let outcome = pipeline::run(
        config_for(
            found_folder,
            &output,
            &found_state.title,
            ResumePoint {
                mic_offset_seconds: found_state.mic_offset_seconds,
                system_offset_seconds: found_state.system_offset_seconds,
                summary_chunk: found_state.summary_chunk,
            },
        ),
        &TranscriptionControl::new(),
        |_: Progress| {},
    )
    .expect("second pass");

    let RunOutcome::Finished(output_files) = outcome else {
        panic!("expected the resumed run to finish");
    };

    let transcript = std::fs::read_to_string(&output_files.transcript_file).expect("transcript");
    let summary = std::fs::read_to_string(&output_files.summary_file).expect("summary");
    assert!(!transcript.trim().is_empty());
    assert!(!summary.trim().is_empty());

    // Both speakers present: resuming must not drop a whole track. The second
    // track had not been started when the pause happened.
    assert!(transcript.contains("ME:"), "the microphone track is missing");
    assert!(transcript.contains("MEETING:"), "the system-audio track is missing");

    // The working files are cleaned up only once both artifacts exist.
    assert!(!folder.join("transcript.partial.json").exists(), "partial left behind");
    assert!(!folder.join("summary.partial.json").exists(), "partial left behind");

    // And the meeting is no longer outstanding, so a third launch does not
    // transcribe it all over again.
    let mut done = queue::load(&folder).expect("state");
    done.stage = Stage::Done;
    queue::save(&folder, &done).expect("save");
    assert!(queue::scan(&output).is_empty(), "a finished meeting must not be rescanned");

    println!("transcript: {} chars, summary: {} chars", transcript.len(), summary.len());
    let _ = std::fs::remove_dir_all(&output);
}
