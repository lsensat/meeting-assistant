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
    /// Set while the pipeline is running so a second start is refused.
    pub processing: Mutex<bool>,
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
}

impl AppState {
    pub fn new(config_file: PathBuf, config: Config) -> Self {
        Self {
            session: Mutex::new(None),
            config: Mutex::new(config),
            processing: Mutex::new(false),
            config_file,
            current_folder: Mutex::new(None),
            muted: Arc::new(AtomicBool::new(false)),
            downloading: Arc::new(AtomicBool::new(false)),
            pending: Mutex::new(None),
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

    pub fn is_processing(&self) -> bool {
        *self.processing.lock().expect("processing poisoned")
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Returns the new state.
    pub fn toggle_muted(&self) -> bool {
        let next = !self.is_muted();
        self.muted.store(next, Ordering::Relaxed);
        next
    }
}
