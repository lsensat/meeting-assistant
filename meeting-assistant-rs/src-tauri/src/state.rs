//! Shared application state.
//!
//! There is exactly one piece of shared mutable state — the current session —
//! behind one mutex, and everything a worker thread needs is snapshotted and
//! moved into it at start. Shared mutable globals reachable from the recording
//! threads, the queue worker and the UI at once are the shape this deliberately
//! does not have.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use meeting_core::config::Config;

use crate::session::RecordingSession;

/// What the microphone is doing, and who last changed it.
///
/// Two fields rather than one, because "muted" and "muted by somebody else" are
/// different facts and the second is the one worth interrupting a meeting over.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Observed {
    /// `None` until something is watching. Not the same as `Some(false)`.
    pub muted: Option<bool>,
    /// Whether the last change came from outside this app.
    pub external: bool,
}

/// What the user should be shown about the microphone.
///
/// Four states from three inputs, and none of the three is redundant:
/// intent alone cannot see a headset button, the observed value alone cannot
/// tell "we did that" from "somebody did that", and the guard alone is a claim
/// that can be lost while its value stays the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteView {
    /// The microphone is live.
    NotMuted,
    /// Muted, by this app, for this recording.
    MutedByUs,
    /// Muted by something else — a headset button, Sound settings.
    MutedElsewhere,
    /// Nothing is watching, so the device's state is genuinely not known. The
    /// app's own intent is all there is to show.
    Unknown { intent: bool },
}

impl MuteView {
    /// Whether the glyph should read as muted.
    ///
    /// `Unknown` falls back to intent: with nothing watching, what the user
    /// asked for is the best available answer and the only one the app has ever
    /// had.
    pub fn looks_muted(self) -> bool {
        match self {
            Self::NotMuted => false,
            Self::MutedByUs | Self::MutedElsewhere => true,
            Self::Unknown { intent } => intent,
        }
    }

    /// Whether to tell the user that something outside the app did this.
    pub fn is_external(self) -> bool {
        matches!(self, Self::MutedElsewhere)
    }
}

/// Decide what to show, from everything known.
///
/// A free function so it can be tested without an `AppState`, a session, or a
/// microphone — the rules are the part that is easy to get subtly wrong, and
/// they should not need hardware to check.
pub fn mute_view(intent: bool, observed: Observed, holds_guard: bool) -> MuteView {
    match observed.muted {
        None => MuteView::Unknown { intent },
        Some(false) => MuteView::NotMuted,
        // Muted, and the question is by whom. The guard is not enough on its
        // own: it can still be held after somebody else has taken the device
        // over, which is why attribution is tracked separately and wins.
        Some(true) if observed.external || !holds_guard => MuteView::MutedElsewhere,
        Some(true) => MuteView::MutedByUs,
    }
}

/// Managed by Tauri and reachable from every command.
pub struct AppState {
    /// `Some` only while recording. The mutex is held for the moment it takes
    /// to start, stop or toggle mute — never across a blocking operation.
    pub session: Mutex<Option<RecordingSession>>,
    pub config: Mutex<Config>,
    /// Meetings waiting to be processed, and the one being processed.
    ///
    /// Replaces a `processing: bool` that existed to REFUSE a second recording
    /// while the first was still being transcribed. Blocking the user at exactly
    /// the moment they are busiest was the thing worth fixing; the queue is what
    /// makes more than one meeting in flight safe instead.
    pub queue: Arc<crate::queue::Queue>,
    /// Where `config.json` lives, resolved once at startup.
    pub config_file: PathBuf,
    /// The folder the current recording is writing into.
    pub current_folder: Mutex<Option<PathBuf>>,
    /// Microphone mute.
    ///
    /// Lives here rather than inside the session so it can be set before a
    /// recording starts and stays in force for the whole of it.
    ///
    /// **Cleared when a recording stops**, which reverses an earlier decision
    /// here. Keeping it was once described as respecting the user's choice,
    /// where clearing it "silently threw the choice away" — but the choice
    /// belongs to a
    /// meeting, not to the app, and keeping it had a much worse failure: mute
    /// once, forget, and the *next* meeting records silent from its first
    /// sample, with the system mute taken automatically. A mute that has to be
    /// re-pressed costs one click. A meeting lost to a click made an hour ago
    /// cannot be recovered at all.
    ///
    /// So "still muted" now means "muted for this meeting, deliberately".
    ///
    /// **This is intent, and only intent.** What the microphone is actually
    /// doing lives in [`observed`](Self::observed) beside it. Merging the two
    /// destroys the pre-record mute: muting at rest sets this flag alone, the
    /// device is not open yet so nothing can be muted, and the first reading
    /// from the watch would then clear it — `write_unit` would stop zeroing and
    /// the app would record someone who pressed mute and was told it was muted.
    pub muted: Arc<AtomicBool>,
    /// What the microphone is doing, as last reported by the OS.
    ///
    /// `None` means nothing is watching — which is **not** the same as "not
    /// muted", and the distinction is the whole point: a window that says
    /// "unmuted" because it has not looked is exactly the failure this feature
    /// exists to remove.
    pub observed: Mutex<Observed>,
    /// Set while a Whisper model download is running. Both the setup wizard and
    /// the settings window can start one, and two downloads of the same model
    /// would race on the same temporary file.
    pub downloading: Arc<AtomicBool>,
    /// The finished recording, held between stopping capture and processing it.
    ///
    /// Capture stops the instant the user asks it to; the meeting title is
    /// collected afterwards. Without this the recording would keep running
    /// while the title dialog was open, capturing the user typing a name.
    pub pending: Mutex<Option<crate::session::SessionSummary>>,
    /// The diagnostics file for the recording in progress.
    ///
    /// Lives here rather than in the session so `stop_recording` can write the
    /// outcome — track lengths, overflow and gap counts, the damage verdict —
    /// into the same file the recorder threads were writing to, after the
    /// session itself has been consumed by `stop`.
    pub meeting_log: Mutex<Option<std::sync::Arc<crate::diagnostics::MeetingLog>>>,
    /// What the startup mute restore did, waiting for a window to tell.
    ///
    /// The restore runs in `setup`, before anything else: a microphone left
    /// muted by a crash is the most urgent thing the app can undo, and it used
    /// to queue behind a VAD model prefetch inside `startup_check` — a command
    /// the *frontend* calls, so the microphone stayed muted until the webview
    /// had booted and asked. A Windows test caught it: after relaunch the
    /// endpoint still read MUTED and `system_mic_muted` was still set.
    ///
    /// There is no UI at `setup` time, so the outcome parks here and
    /// `startup_check` emits it once somebody can read it.
    pub startup_mute_restore: Mutex<Option<String>>,
}

impl AppState {
    pub fn new(config_file: PathBuf, config: Config) -> Self {
        Self {
            session: Mutex::new(None),
            config: Mutex::new(config),
            queue: Arc::new(crate::queue::Queue::new()),
            config_file,
            current_folder: Mutex::new(None),
            muted: Arc::new(AtomicBool::new(false)),
            observed: Mutex::new(Observed::default()),
            downloading: Arc::new(AtomicBool::new(false)),
            pending: Mutex::new(None),
            meeting_log: Mutex::new(None),
            startup_mute_restore: Mutex::new(None),
        }
    }

    /// A copy of the current settings.
    ///
    /// Every worker takes one of these rather than reading the live config, so
    /// a settings save mid-meeting cannot change what the recorder is doing.
    pub fn config_snapshot(&self) -> Config {
        self.config.lock().expect("config poisoned").clone()
    }

    pub fn is_recording(&self) -> bool {
        self.session.lock().expect("session poisoned").is_some()
    }

    /// Whether any meeting is being processed or waiting to be.
    pub fn is_processing(&self) -> bool {
        self.queue.has_outstanding()
    }

    /// Persist which microphone this app has muted system-wide, or that it
    /// holds none.
    ///
    /// Written straight to disk rather than batched: the only reader is the
    /// **next launch after a crash**, so a value that never reached the file is
    /// a value that does not exist. A failed write is logged and otherwise
    /// ignored — it means the next launch will not restore the mute, which the
    /// user can do from Sound settings, and it is no reason to fail a recording.
    pub fn remember_system_mute(&self, device: Option<&str>) {
        let mut config = self.config.lock().expect("config poisoned");
        config.system_mic_muted = device.map(str::to_string);

        if let Err(e) = std::fs::write(&self.config_file, config.to_json()) {
            eprintln!("[mute] could not record the system mute: {e}");
        }
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Whether the microphone is muted for **every** application right now.
    ///
    /// Distinct from [`is_muted`](Self::is_muted), which is this app's own flag:
    /// the button can be down while the system mute was refused by the device,
    /// and the tray has to be able to tell those apart. The guard lives on the
    /// session, so this is false whenever nothing is recording.
    pub fn holds_system_mute(&self) -> bool {
        self.session
            .lock()
            .expect("session poisoned")
            .as_ref()
            .map(|s| s.holds_system_mute())
            .unwrap_or(false)
    }

    /// Returns the new state.
    pub fn toggle_muted(&self) -> bool {
        let next = !self.is_muted();
        self.muted.store(next, Ordering::Relaxed);
        next
    }
}

#[cfg(test)]
mod mute_view_tests {
    use super::*;

    /// The table, row by row.
    ///
    /// Written out rather than derived, because every row is a thing the user
    /// sees and two of them were wrong in earlier drafts of this design.
    #[test]
    fn the_view_is_a_function_of_all_three_inputs() {
        let nothing_watching = Observed { muted: None, external: false };
        let ours = Observed { muted: Some(true), external: false };
        let theirs = Observed { muted: Some(true), external: true };
        let live = Observed { muted: Some(false), external: false };

        // Nothing is watching: intent is all there is, and it is reported as
        // unknown rather than as fact. This is the Windows case today, and any
        // machine where registering the watch failed.
        assert_eq!(
            mute_view(true, nothing_watching, false),
            MuteView::Unknown { intent: true }
        );
        assert_eq!(
            mute_view(false, nothing_watching, false),
            MuteView::Unknown { intent: false }
        );
        assert!(mute_view(true, nothing_watching, false).looks_muted());

        // We muted it and still hold the claim.
        assert_eq!(mute_view(true, ours, true), MuteView::MutedByUs);

        // The headset button: muted, nobody holding a guard. This is the pair an
        // older test called unreachable, and it is the whole reason for this
        // feature.
        assert_eq!(mute_view(false, theirs, false), MuteView::MutedElsewhere);

        // Muted by somebody else *while we hold a guard*. The device value never
        // changed, only the attribution — and the claim is what was lost, so
        // the guard must not make this look like ours.
        assert_eq!(mute_view(true, theirs, true), MuteView::MutedElsewhere);

        // Somebody unmuted a mute we hold. The microphone is live, whatever the
        // guard or the intent still say; reporting anything else here would be
        // telling the user they are muted while the call hears them.
        assert_eq!(mute_view(true, live, true), MuteView::NotMuted);
        assert!(!mute_view(true, live, true).looks_muted());
    }

    /// Only a mute somebody else set is worth interrupting for.
    #[test]
    fn only_an_outside_mute_is_external() {
        let theirs = Observed { muted: Some(true), external: true };
        let ours = Observed { muted: Some(true), external: false };

        assert!(mute_view(false, theirs, false).is_external());
        assert!(!mute_view(true, ours, true).is_external());
        assert!(!mute_view(true, Observed::default(), false).is_external());
    }
}
