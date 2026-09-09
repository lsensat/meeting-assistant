//! Persisted per-meeting processing state, and the scan that rebuilds the queue.
//!
//! # Why the state lives in the meeting folder
//!
//! One `meeting.json` per meeting, inside that meeting's own folder, rather than
//! a central index. Three reasons, all of which a central file gets wrong:
//!
//! 1. `pipeline::run` **renames the folder** during its first stage, to append
//!    the meeting title. A file inside travels with it; a central index holding
//!    paths would need updating in the same breath, and a crash between the two
//!    leaves the index pointing at nothing.
//! 2. It survives a crash without a second write path. The folder is already
//!    being written; this is one more file in it.
//! 3. Deleting a meeting folder deletes its job. There is no orphan bookkeeping
//!    to reconcile, which is what makes the discard control safe.
//!
//! # State is read from this file, never inferred from the folder's contents
//!
//! It is tempting to treat "has `transcript.txt`" as done. That is wrong here:
//! `keep_audio` defaults to **true**, so a finished meeting keeps its WAVs and
//! looks exactly like one that never started, and a meeting interrupted midway
//! through the summary stage has a transcript but is not finished. The file is
//! the authority; the folder contents are not evidence.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file, inside each meeting folder.
pub const STATE_FILENAME: &str = "meeting.json";

/// Bumped when the shape below changes incompatibly. A state file from a newer
/// version is left alone rather than guessed at — see [`load`].
pub const SCHEMA_VERSION: u32 = 1;

/// How far a meeting has got.
///
/// Ordered as the pipeline runs, so `stage < Done` is "there is work to do".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Captured and waiting. Nothing has been processed.
    Queued,
    /// The folder rename. Fast, but it is the point the folder path changes.
    Audio,
    Whisper,
    Summary,
    Done,
    /// Processing failed. Stays in the queue with its error so the user can
    /// retry; never retried automatically and never silently dropped.
    Failed,
}

impl Stage {
    /// Whether this meeting still needs the worker.
    ///
    /// `Failed` is **not** outstanding: it needs a decision from the user, and
    /// re-running it on every launch would loop on a permanent failure such as
    /// a missing model or a full disk.
    pub fn is_outstanding(self) -> bool {
        matches!(self, Self::Queued | Self::Audio | Self::Whisper | Self::Summary)
    }
}

/// How long each stage of the pipeline took.
///
/// Written into `meeting.json` so the cost of a real meeting can be read off a
/// real run, rather than inferred from a synthetic one. The first measurements
/// taken by hand were a surprise — the summary was 66s against transcription's
/// 2.2s, which is the opposite of where the effort had been going — and that is
/// exactly the kind of thing that should not need a special build to discover.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Timings {
    pub audio: StageTiming,
    pub whisper: StageTiming,
    pub summary: StageTiming,
}

/// One stage's clock.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StageTiming {
    /// When the stage was first entered, `YYYY-MM-DDTHH:MM:SS`, UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When it last finished. Absent while it is running, or if it never did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Seconds spent in this stage, **totalled across every attempt**.
    ///
    /// The one to compare stages by. A meeting that was paused and resumed has
    /// a wall-clock span between `started_at` and `ended_at` that includes the
    /// time the app was closed, so the difference between those two is not the
    /// work done — this is.
    #[serde(default)]
    pub seconds: f64,
}

impl StageTiming {
    /// Note that the stage has been entered. Only the first entry sets the
    /// start, so a resume does not erase when the meeting really began.
    pub fn begin(&mut self, now: String) {
        if self.started_at.is_none() {
            self.started_at = Some(now);
        }
        self.ended_at = None;
    }

    /// Note that the stage has left off, having run for `seconds`.
    pub fn end(&mut self, now: String, seconds: f64) {
        self.seconds += seconds;
        self.ended_at = Some(now);
    }
}

/// What `meeting.json` holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingState {
    pub version: u32,
    /// The meeting's original timestamped folder name, `YYYY-MM-DD_HH-MM-SS`.
    ///
    /// Stable across the rename that the audio stage performs, unique because
    /// two meetings cannot start in the same second, and already meaningful to
    /// the user — it is the start time the queue card shows.
    pub id: String,
    /// The title the user gave, or empty. Kept here as well as in the folder
    /// name because the folder name is sanitised and cannot be reversed.
    pub title: String,
    pub stage: Stage,
    /// Seconds of each track already transcribed, so an aborted run resumes
    /// from there instead of starting over. See `whisper::Transcriber`.
    #[serde(default)]
    pub mic_offset_seconds: f64,
    #[serde(default)]
    pub system_offset_seconds: f64,
    /// Which summary chunk to resume from. The summary stage is already a loop
    /// over chunks, so this costs at most one chunk of repeated work.
    #[serde(default)]
    pub summary_chunk: usize,
    /// How long the meeting was, in seconds.
    ///
    /// Read from the microphone WAV's header once and kept, rather than
    /// measured whenever the UI asks: `view()` builds its snapshot under the
    /// queue lock, and file I/O does not belong there.
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    /// How long each stage took. See [`Timings`].
    #[serde(default)]
    pub timings: Timings,
    /// The failure from the last attempt, when `stage` is `Failed`.
    #[serde(default)]
    pub error: Option<String>,
    /// The configuration as it stood when the meeting was finalised, verbatim.
    ///
    /// Snapshotted at enqueue rather than read at dequeue: changing the summary
    /// type or the provider between meetings must not reach back and rewrite
    /// what a meeting already waiting in the queue will produce. Stored as
    /// opaque JSON so `Config` can gain fields without a migration here.
    pub config: serde_json::Value,
}

impl MeetingState {
    pub fn new(id: String, title: String, config: serde_json::Value) -> Self {
        Self {
            version: SCHEMA_VERSION,
            id,
            title,
            stage: Stage::Queued,
            mic_offset_seconds: 0.0,
            system_offset_seconds: 0.0,
            summary_chunk: 0,
            duration_seconds: None,
            timings: Timings::default(),
            error: None,
            config,
        }
    }
}

pub fn state_path(folder: &Path) -> PathBuf {
    folder.join(STATE_FILENAME)
}

/// Read a meeting's state, or `None` if there is none to read.
///
/// A missing file is the normal case for any meeting recorded before this
/// existed, and for any folder that is not a meeting at all. A malformed or
/// newer-versioned file is also `None`: the alternative is guessing at a shape
/// we do not understand and then writing our guess back over it.
pub fn load(folder: &Path) -> Option<MeetingState> {
    let text = std::fs::read_to_string(state_path(folder)).ok()?;
    let state: MeetingState = serde_json::from_str(&text).ok()?;
    if state.version > SCHEMA_VERSION {
        return None;
    }
    Some(state)
}

/// Write a meeting's state, atomically.
///
/// Through a `.partial` and a rename, the same shape as the model download, so
/// a crash mid-write cannot leave a truncated file that [`load`] would then
/// reject — losing the resume point of a meeting that was nearly finished.
pub fn save(folder: &Path, state: &MeetingState) -> std::io::Result<()> {
    let final_path = state_path(folder);
    let partial = final_path.with_extension("json.partial");

    let text = serde_json::to_string_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, &final_path)
}

/// Every meeting under `output_folder` that still has work outstanding.
///
/// One level deep: meetings are direct children of the output folder. Returned
/// oldest first, by id, which is chronological because the id is a timestamp —
/// so a backlog is worked through in the order it was recorded.
pub fn scan(output_folder: &Path) -> Vec<(PathBuf, MeetingState)> {
    let Ok(entries) = std::fs::read_dir(output_folder) else {
        return Vec::new();
    };

    let mut found: Vec<(PathBuf, MeetingState)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            let mut state = load(&path)?;
            if !state.stage.is_outstanding() {
                return None;
            }
            // Meetings recorded before this field existed have no length
            // stored. Reading the WAV header here costs one seek per meeting,
            // once at startup, and off the queue lock.
            if state.duration_seconds.is_none() {
                state.duration_seconds =
                    crate::pipeline::wav_duration(&path.join(crate::session::MIC_FILENAME));
            }
            Some((path, state))
        })
        .collect();

    found.sort_by(|a, b| a.1.id.cmp(&b.1.id));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resumed_stage_keeps_its_original_start_and_totals_the_work() {
        let mut clock = StageTiming::default();

        clock.begin("2026-09-09T10:00:00".into());
        clock.end("2026-09-09T10:00:30".into(), 30.0);

        // Paused, the app closed, reopened an hour later, and resumed.
        clock.begin("2026-09-09T11:00:00".into());
        clock.end("2026-09-09T11:00:20".into(), 20.0);

        assert_eq!(
            clock.started_at.as_deref(),
            Some("2026-09-09T10:00:00"),
            "the second attempt must not overwrite when the meeting began"
        );
        assert_eq!(clock.ended_at.as_deref(), Some("2026-09-09T11:00:20"));
        // The wall-clock span is over an hour; the work was 50 seconds. This is
        // the whole reason `seconds` exists rather than subtracting the two.
        assert_eq!(clock.seconds, 50.0);
    }

    #[test]
    fn a_running_stage_has_no_end() {
        let mut clock = StageTiming::default();
        clock.begin("2026-09-09T10:00:00".into());
        clock.end("2026-09-09T10:00:30".into(), 30.0);
        clock.begin("2026-09-09T11:00:00".into());

        assert!(
            clock.ended_at.is_none(),
            "re-entering a stage must clear the previous end, or a running \
             stage reads as finished"
        );
    }

    #[test]
    fn meeting_state_without_timings_still_loads() {
        // Every meeting.json written before this field existed.
        let json = r#"{
            "version": 1,
            "id": "2026-09-09_10-00-00",
            "title": "Standup",
            "stage": "queued",
            "config": {}
        }"#;
        let state: MeetingState = serde_json::from_str(json).expect("legacy state parses");
        assert_eq!(state.timings, Timings::default());
        assert_eq!(state.duration_seconds, None);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meeting-queue-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn meeting(root: &Path, id: &str, stage: Stage) -> PathBuf {
        let folder = root.join(id);
        std::fs::create_dir_all(&folder).expect("mkdir");
        let mut state = MeetingState::new(id.to_string(), String::new(), serde_json::json!({}));
        state.stage = stage;
        save(&folder, &state).expect("save");
        folder
    }

    #[test]
    fn state_survives_a_round_trip() {
        let dir = temp_dir("round-trip");
        let mut state = MeetingState::new(
            "2026-09-07_10-00-00".into(),
            "Standup".into(),
            serde_json::json!({ "whisper_model": "small" }),
        );
        state.stage = Stage::Whisper;
        state.mic_offset_seconds = 123.5;
        state.summary_chunk = 2;

        save(&dir, &state).expect("save");
        let read = load(&dir).expect("load");

        assert_eq!(read.id, state.id);
        assert_eq!(read.title, "Standup");
        assert_eq!(read.stage, Stage::Whisper);
        assert_eq!(read.mic_offset_seconds, 123.5);
        assert_eq!(read.summary_chunk, 2);
        assert_eq!(read.config["whisper_model"], "small");
    }

    #[test]
    fn saving_leaves_no_partial_behind() {
        let dir = temp_dir("no-partial");
        let state = MeetingState::new("id".into(), String::new(), serde_json::json!({}));
        save(&dir, &state).expect("save");
        assert!(!dir.join("meeting.json.partial").exists());
        assert!(dir.join(STATE_FILENAME).exists());
    }

    #[test]
    fn the_scan_returns_only_outstanding_meetings() {
        let root = temp_dir("scan");
        meeting(&root, "2026-09-07_09-00-00", Stage::Done);
        meeting(&root, "2026-09-07_10-00-00", Stage::Queued);
        meeting(&root, "2026-09-07_11-00-00", Stage::Whisper);
        meeting(&root, "2026-09-07_12-00-00", Stage::Failed);

        let ids: Vec<String> = scan(&root).into_iter().map(|(_, s)| s.id).collect();
        assert_eq!(ids, ["2026-09-07_10-00-00", "2026-09-07_11-00-00"]);
    }

    #[test]
    fn the_file_wins_over_the_folder_contents() {
        // The whole reason state is not inferred. This folder has both output
        // files AND its audio — `keep_audio` defaults to true — so every
        // file-based heuristic would call it finished. It is not.
        let root = temp_dir("files-lie");
        let folder = meeting(&root, "2026-09-07_10-00-00", Stage::Summary);
        for name in ["transcript.txt", "microphone.wav", "system_audio.wav"] {
            std::fs::write(folder.join(name), b"x").expect("write");
        }

        assert_eq!(scan(&root).len(), 1, "a half-finished meeting must be picked up");
    }

    #[test]
    fn a_finished_meeting_that_kept_its_audio_is_not_reprocessed() {
        // The mirror image, and the one that would cost the user real money in
        // time: re-transcribing everything they have ever recorded on launch.
        let root = temp_dir("done-with-audio");
        let folder = meeting(&root, "2026-09-07_10-00-00", Stage::Done);
        for name in ["transcript.txt", "summary.md", "microphone.wav"] {
            std::fs::write(folder.join(name), b"x").expect("write");
        }

        assert!(scan(&root).is_empty());
    }

    #[test]
    fn folders_without_state_are_ignored() {
        // Every meeting recorded before this feature existed. They must not be
        // swept into the queue and reprocessed.
        let root = temp_dir("legacy");
        let old = root.join("2026-01-01_09-00-00");
        std::fs::create_dir_all(&old).expect("mkdir");
        std::fs::write(old.join("microphone.wav"), b"x").expect("write");

        assert!(scan(&root).is_empty());
    }

    #[test]
    fn a_malformed_or_newer_state_file_is_skipped_not_fatal() {
        let root = temp_dir("malformed");

        let broken = root.join("2026-09-07_10-00-00");
        std::fs::create_dir_all(&broken).expect("mkdir");
        std::fs::write(state_path(&broken), b"{ not json").expect("write");

        let future = root.join("2026-09-07_11-00-00");
        std::fs::create_dir_all(&future).expect("mkdir");
        let mut ahead = MeetingState::new("2026-09-07_11-00-00".into(), String::new(), serde_json::json!({}));
        ahead.version = SCHEMA_VERSION + 1;
        save(&future, &ahead).expect("save");

        // Neither is enqueued, and neither panics.
        assert!(scan(&root).is_empty());
        assert!(load(&broken).is_none());
        assert!(load(&future).is_none());
    }

    #[test]
    fn a_missing_output_folder_is_empty_not_an_error() {
        assert!(scan(Path::new("/nonexistent/meeting-assistant/meetings")).is_empty());
    }
}

// ---------------------------------------------------------------- the queue

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

use crate::whisper::TranscriptionControl;

/// One meeting waiting for, or receiving, the worker's attention.
#[derive(Debug, Clone)]
pub struct Job {
    pub folder: PathBuf,
    pub state: MeetingState,
}

/// A job flattened for the UI.
///
/// The frontend renders the queue from a snapshot of these rather than
/// reconstructing it from a stream of per-job events. One "something changed"
/// signal plus a snapshot is far less machinery than an id on every event, and
/// it cannot drift out of sync with the truth the way an incrementally-applied
/// stream can.
#[derive(Debug, Clone, Serialize)]
pub struct JobView {
    pub id: String,
    pub title: String,
    pub stage: Stage,
    pub percent: u8,
    pub error: Option<String>,
    pub running: bool,
    pub duration_seconds: Option<f64>,
}

#[derive(Default)]
struct Inner {
    jobs: VecDeque<Job>,
    /// Id of the job the worker currently holds, if any.
    running: Option<String>,
    paused: bool,
    /// Live percentage for the running job, updated by the worker.
    percent: u8,
    /// Set when the worker should exit entirely, at shutdown.
    stopped: bool,
}

/// The processing queue: one worker, one global pause.
pub struct Queue {
    inner: Mutex<Inner>,
    /// Woken when work arrives, when the pause lifts, or at shutdown.
    wake: Condvar,
    /// The running job's handle. Held here so `pause` and `discard` can reach
    /// into a transcription that is already underway — a `full()` call blocks
    /// its thread for the whole file, so there is no other way to stop it.
    control: Mutex<TranscriptionControl>,
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            wake: Condvar::new(),
            control: Mutex::new(TranscriptionControl::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("queue poisoned")
    }

    pub fn enqueue(&self, job: Job) {
        self.lock().jobs.push_back(job);
        self.wake.notify_all();
    }

    /// Rebuild the queue from disk. Called once at startup.
    ///
    /// Existing entries are kept and duplicates skipped, so calling it twice
    /// cannot queue the same meeting for a second transcription.
    pub fn absorb(&self, found: Vec<(PathBuf, MeetingState)>) {
        let mut inner = self.lock();
        for (folder, state) in found {
            if inner.jobs.iter().any(|job| job.state.id == state.id) {
                continue;
            }
            inner.jobs.push_back(Job { folder, state });
        }
        drop(inner);
        self.wake.notify_all();
    }

    pub fn is_paused(&self) -> bool {
        self.lock().paused
    }

    /// True while a meeting is being processed or is waiting to be.
    ///
    /// Used by the guard on deleting a Whisper model: a **queued** meeting still
    /// needs its model when its turn comes, so "is anything outstanding" is the
    /// right question, not "is anything running".
    pub fn has_outstanding(&self) -> bool {
        let inner = self.lock();
        inner.running.is_some() || inner.jobs.iter().any(|j| j.state.stage.is_outstanding())
    }

    /// Stop or start processing.
    ///
    /// Pausing aborts the running job rather than letting it finish: the point
    /// of the switch is to stop competing for the machine *now*, and a
    /// large-v3 transcription can run for many minutes. Nothing is lost —
    /// the pipeline persists its progress before returning.
    pub fn set_paused(&self, paused: bool) {
        self.lock().paused = paused;
        if paused {
            self.control.lock().expect("control poisoned").abort();
        }
        self.wake.notify_all();
    }

    /// Remove a meeting from the queue, aborting it first if it is running.
    ///
    /// Returns its folder so the caller can delete it. Deleting here would be
    /// wrong: the pipeline may still be inside that directory, and removing it
    /// underneath is exactly the corruption the rename ordering avoids.
    pub fn take(&self, id: &str) -> Option<PathBuf> {
        let mut inner = self.lock();

        if inner.running.as_deref() == Some(id) {
            drop(inner);
            self.control.lock().expect("control poisoned").abort();
            inner = self.lock();

            // Actually wait. This used to abort and return immediately while
            // claiming in a comment that it waited, so a discard raced the
            // pipeline's own writes into the directory it was about to remove.
            //
            // Bounded, because a worker wedged in a C++ call must not freeze the
            // command that is trying to get rid of it. Whoever proceeds after
            // the timeout is no worse off than before this check existed.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while inner.running.as_deref() == Some(id) {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (guard, _) = self
                    .wake
                    .wait_timeout(inner, remaining.min(std::time::Duration::from_millis(200)))
                    .expect("queue poisoned");
                inner = guard;
            }
        }

        let index = inner.jobs.iter().position(|job| job.state.id == id)?;
        let job = inner.jobs.remove(index)?;
        Some(job.folder)
    }

    /// Put a failed meeting back in line.
    pub fn retry(&self, id: &str) -> bool {
        let mut inner = self.lock();
        let Some(job) = inner.jobs.iter_mut().find(|job| job.state.id == id) else {
            return false;
        };
        if job.state.stage != Stage::Failed {
            return false;
        }
        // Back to where it got to, not back to the beginning: the offsets and
        // partial files are still on disk and still valid.
        job.state.stage = if job.state.mic_offset_seconds > 0.0 {
            Stage::Whisper
        } else {
            Stage::Queued
        };
        job.state.error = None;
        let folder = job.folder.clone();
        let state = job.state.clone();
        drop(inner);
        let _ = save(&folder, &state);
        self.wake.notify_all();
        true
    }

    /// A snapshot for the UI, oldest first.
    pub fn view(&self) -> Vec<JobView> {
        let inner = self.lock();
        inner
            .jobs
            .iter()
            .map(|job| {
                let running = inner.running.as_deref() == Some(job.state.id.as_str());
                JobView {
                    id: job.state.id.clone(),
                    title: job.state.title.clone(),
                    stage: job.state.stage,
                    percent: if running { inner.percent } else { 0 },
                    error: job.state.error.clone(),
                    running,
                    duration_seconds: job.state.duration_seconds,
                }
            })
            .collect()
    }

    /// Block until there is work and processing is not paused.
    ///
    /// `None` means the app is shutting down. Each job gets a **fresh**
    /// control: reusing one would carry the previous job's abort flag into the
    /// next, so resuming after a pause would immediately stop again.
    pub fn next(&self) -> Option<Job> {
        let mut inner = self.lock();
        loop {
            if inner.stopped {
                return None;
            }

            if !inner.paused {
                if let Some(index) = inner.jobs.iter().position(|j| j.state.stage.is_outstanding())
                {
                    let job = inner.jobs[index].clone();
                    inner.running = Some(job.state.id.clone());
                    inner.percent = 0;
                    *self.control.lock().expect("control poisoned") = TranscriptionControl::new();
                    return Some(job);
                }
            }

            inner = self.wake.wait(inner).expect("queue poisoned");
        }
    }

    /// The running job's handle, for polling progress from another thread.
    pub fn control(&self) -> TranscriptionControl {
        self.control.lock().expect("control poisoned").clone()
    }

    /// Move a running job to a new stage, so its card can say what is happening.
    ///
    /// Display only: the persisted state is written when the job ends. Without
    /// this the card kept the stage it was enqueued with and read "Waiting" for
    /// the entire run, while the status line said the summary was being written.
    pub fn set_stage(&self, id: &str, stage: Stage) {
        let mut inner = self.lock();
        if let Some(job) = inner.jobs.iter_mut().find(|j| j.state.id == id) {
            job.state.stage = stage;
        }
    }

    pub fn set_percent(&self, percent: u8) {
        self.lock().percent = percent;
    }

    /// Record where a job got to and release the worker.
    ///
    /// Takes the **folder as it now stands**, because the pipeline renames it
    /// during its first stage. Keeping only the state and leaving `job.folder`
    /// at the path the job was created with meant every later attempt — a resume
    /// after a pause, a retry, a discard — used a directory that no longer
    /// existed, and reported "the system cannot find the file specified".
    ///
    /// Finished meetings leave the queue — they are done, and the main window's
    /// result buttons point at them. Failed ones stay, because they need a
    /// decision the user has not made yet.
    pub fn finish(&self, id: &str, updated: MeetingState, folder: PathBuf) {
        let mut inner = self.lock();
        inner.running = None;
        inner.percent = 0;
        if let Some(job) = inner.jobs.iter_mut().find(|j| j.state.id == id) {
            job.state = updated;
            job.folder = folder;
        }
        inner.jobs.retain(|j| j.state.stage != Stage::Done);
        drop(inner);
        self.wake.notify_all();
    }

    pub fn shutdown(&self) {
        self.lock().stopped = true;
        self.control.lock().expect("control poisoned").abort();
        self.wake.notify_all();
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    fn job(id: &str, stage: Stage) -> Job {
        let mut state = MeetingState::new(id.into(), String::new(), serde_json::json!({}));
        state.stage = stage;
        Job { folder: PathBuf::from("/tmp").join(id), state }
    }

    #[test]
    fn absorb_does_not_queue_the_same_meeting_twice() {
        // Guards the resume path: scanning at startup while something is
        // already queued must not schedule it for a second transcription.
        let queue = Queue::new();
        queue.enqueue(job("a", Stage::Queued));
        queue.absorb(vec![
            (PathBuf::from("/tmp/a"), job("a", Stage::Queued).state),
            (PathBuf::from("/tmp/b"), job("b", Stage::Queued).state),
        ]);
        let ids: Vec<String> = queue.view().into_iter().map(|v| v.id).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn a_queued_meeting_counts_as_outstanding() {
        // The Whisper-model delete guard depends on this: a meeting waiting its
        // turn still needs its model to exist when the worker reaches it.
        let queue = Queue::new();
        assert!(!queue.has_outstanding());
        queue.enqueue(job("a", Stage::Queued));
        assert!(queue.has_outstanding());
    }

    #[test]
    fn a_failed_meeting_is_not_outstanding_until_retried() {
        let queue = Queue::new();
        queue.enqueue(job("a", Stage::Failed));
        assert!(!queue.has_outstanding(), "a failure must not be retried on a loop");
    }

    #[test]
    fn retry_resumes_rather_than_restarting() {
        let queue = Queue::new();
        let mut failed = job("a", Stage::Failed);
        failed.state.mic_offset_seconds = 42.0;
        queue.enqueue(failed);

        assert!(queue.retry("a"));
        let view = queue.view();
        assert_eq!(view[0].stage, Stage::Whisper, "work already done must not be repeated");
        assert!(view[0].error.is_none());
    }

    #[test]
    fn retry_only_applies_to_failures() {
        let queue = Queue::new();
        queue.enqueue(job("a", Stage::Queued));
        assert!(!queue.retry("a"));
        assert!(!queue.retry("nonexistent"));
    }

    #[test]
    fn take_removes_the_job_and_reports_its_folder() {
        let queue = Queue::new();
        queue.enqueue(job("a", Stage::Queued));
        queue.enqueue(job("b", Stage::Queued));

        assert_eq!(queue.take("a"), Some(PathBuf::from("/tmp/a")));
        let ids: Vec<String> = queue.view().into_iter().map(|v| v.id).collect();
        assert_eq!(ids, ["b"]);
        assert_eq!(queue.take("gone"), None);
    }

    #[test]
    fn finishing_a_job_records_where_the_pipeline_moved_it() {
        // The bug that cost a real meeting. `pipeline::run` renames the folder
        // in its first stage to append the title, so the path a job was created
        // with stops existing. The queue kept the old one, and the next attempt
        // — a resume after a pause, a retry, a discard — handed it back and got
        // "the system cannot find the file specified".
        let queue = Queue::new();
        queue.enqueue(job("2026-09-09_08-16-22", Stage::Queued));

        let renamed = PathBuf::from("/tmp/2026-09-09_08-16-22_standup");
        let mut state = job("2026-09-09_08-16-22", Stage::Whisper).state;
        state.mic_offset_seconds = 42.0;
        queue.finish("2026-09-09_08-16-22", state, renamed.clone());

        // The job is still queued — paused, not done — so the worker will pick
        // it up again, and it must find it where the pipeline left it.
        let next = queue.next().expect("a paused job is still outstanding");
        assert_eq!(
            next.folder, renamed,
            "the second attempt would look in a directory that no longer exists"
        );
        assert_eq!(next.state.mic_offset_seconds, 42.0);
    }

    #[test]
    fn discarding_reports_the_current_folder_not_the_original() {
        // Same bug, different victim: `discard_job` deletes what this returns.
        // Against the stale path it removed nothing, reported an error, dropped
        // the job, and left the real folder orphaned on disk.
        let queue = Queue::new();
        queue.enqueue(job("2026-09-09_08-16-22", Stage::Queued));

        let renamed = PathBuf::from("/tmp/2026-09-09_08-16-22_standup");
        let state = job("2026-09-09_08-16-22", Stage::Whisper).state;
        queue.finish("2026-09-09_08-16-22", state, renamed.clone());

        assert_eq!(queue.take("2026-09-09_08-16-22"), Some(renamed));
    }

    #[test]
    fn a_running_job_reports_the_stage_it_reached() {
        // The card read "Waiting" for a whole meeting while the status line
        // said the summary was being generated: the worker updated the
        // percentage as it went but never the stage, so the view kept the one
        // the job was enqueued with.
        let queue = Queue::new();
        queue.enqueue(job("a", Stage::Queued));
        assert_eq!(queue.view()[0].stage, Stage::Queued);

        queue.set_stage("a", Stage::Whisper);
        assert_eq!(queue.view()[0].stage, Stage::Whisper);

        queue.set_stage("a", Stage::Summary);
        assert_eq!(queue.view()[0].stage, Stage::Summary);
    }

    #[test]
    fn pausing_aborts_the_running_transcription() {
        // Pause has to reach into a `full()` call that is already blocking its
        // thread; without this it would only take effect between meetings.
        let queue = Queue::new();
        let control = queue.control.lock().expect("control").clone();
        assert!(!control.is_aborted());

        queue.set_paused(true);
        assert!(queue.is_paused());
        assert!(control.is_aborted());
    }
}

