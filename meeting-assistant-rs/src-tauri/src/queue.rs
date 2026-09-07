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
            let state = load(&path)?;
            state.stage.is_outstanding().then_some((path, state))
        })
        .collect();

    found.sort_by(|a, b| a.1.id.cmp(&b.1.id));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

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
