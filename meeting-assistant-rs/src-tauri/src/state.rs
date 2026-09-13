//! Shared application state.
//!
//! The Python kept ~20 module-level globals mutated from four threads with no
//! synchronization (deferred fixes #1 and #2). Here there is exactly one piece
//! of shared mutable state — the current session — behind one mutex, and
//! everything a worker thread needs is snapshotted and moved into it at start.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use meeting_core::config::Config;

use crate::session::RecordingSession;

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
    /// recording starts and stays set across one. The Python reset
    /// `mic_muted = False` on every stop (`app.py:3962`), which silently threw
    /// the user's choice away.
    pub muted: Arc<AtomicBool>,
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
            downloading: Arc::new(AtomicBool::new(false)),
            pending: Mutex::new(None),
            meeting_log: Mutex::new(None),
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
