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

/// `lock()` that recovers a poisoned mutex instead of propagating the panic.
///
/// # Why poisoning is the wrong default here
///
/// Poisoning exists to stop a thread inheriting state that another thread left
/// half-updated. None of the mutexes below guard anything of that shape: each
/// holds either an `Option<T>` that is taken wholesale or a handle, so there is
/// no torn invariant for the next caller to trip over.
///
/// What propagating the panic buys instead is an app that can never stop the
/// meeting it is currently recording. One panic anywhere inside any critical
/// section poisons that mutex permanently, and because `session` is the mutex
/// `stop_recording` needs, every later attempt to stop panics too — for the life
/// of the process, with the audio still on disk and its WAV header never fixed
/// up. Recovering the value is the lesser harm by a wide margin.
///
/// This is deliberately not a blanket policy: a mutex whose critical section can
/// leave a genuinely inconsistent value should keep `expect`. See
/// [`AppState::remember_system_mute`], whose section was reduced to a single
/// field assignment precisely so this trait could be used on it.
pub trait LockRecover<T> {
    fn lock_recover(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockRecover<T> for Mutex<T> {
    fn lock_recover(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
    /// Serialises `start_recording`, `stop_recording`, `cancel_recording` and
    /// `finalize_meeting` against one another. Guards nothing; the lock itself
    /// is the point. See [`AppState::lifecycle_guard`].
    lifecycle: Mutex<()>,
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
            startup_mute_restore: Mutex::new(None),
            lifecycle: Mutex::new(()),
        }
    }

    /// Hold this for the whole body of a lifecycle command.
    ///
    /// # What it is replacing
    ///
    /// The four lifecycle commands are serialised today by an accident of
    /// dispatch: they are plain `#[tauri::command]`, so Tauri runs them on the
    /// main thread, one at a time. Nothing in the code says so, and moving any
    /// of them to `(async)` — which they need, because they join writer threads,
    /// open audio devices and delete multi-gigabyte folders — silently removes
    /// it.
    ///
    /// What that would expose is a real gap rather than a theoretical one.
    /// `start_recording` asks `is_recording()`, then does around ninety lines of
    /// fallible work, and only then installs the session. Two starts that both
    /// pass the check both reach the assignment, and the second overwrites the
    /// first. `RecordingSession` has no `Drop` and `stop()` consumes `self`, so
    /// the overwritten one is simply dropped: its writer threads detach and keep
    /// running against a WAV whose header is never fixed up, holding the audio
    /// device. That is a lost meeting, and a meeting cannot be recorded twice.
    ///
    /// So the serialisation becomes explicit and keeps exactly the shape it has
    /// now — strictly no weaker than the status quo, which is the property worth
    /// having when the thing being protected is unrepeatable.
    ///
    /// Deliberately a plain `Mutex<()>` rather than a try-lock: a second caller
    /// should wait and then be told "already recording" by the check inside,
    /// which is what happens today. It is also not a state machine; that is the
    /// better long-term shape and a much larger change than this earns.
    pub fn lifecycle_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lifecycle.lock_recover()
    }

    /// A copy of the current settings.
    ///
    /// Every worker takes one of these rather than reading the live config, so
    /// a settings save mid-meeting cannot change what the recorder is doing.
    pub fn config_snapshot(&self) -> Config {
        self.config.lock_recover().clone()
    }

    pub fn is_recording(&self) -> bool {
        self.session.lock_recover().is_some()
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
        // Serialise under the lock, write outside it.
        //
        // This used to hold the `config` guard across `fs::write`, and the
        // caller — `apply_system_mute` — holds the `session` guard for the whole
        // of its body. A mute toggle therefore blocked every other command that
        // touches the session, `stop_recording` included, for as long as the
        // disk took. On a network volume that is not a rounding error.
        //
        // It also leaves the critical section a single field assignment, which
        // cannot panic, which is what makes `lock_recover` safe to use here.
        let json = {
            let mut config = self.config.lock_recover();
            config.system_mic_muted = device.map(str::to_string);
            config.to_json()
        };

        if let Err(e) = std::fs::write(&self.config_file, json) {
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
            .lock_recover()
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
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn state() -> AppState {
        let dir = std::env::temp_dir();
        AppState::new(dir.join("config.json"), Config::defaults(&dir))
    }

    /// The property the lifecycle commands will depend on once they are `async`:
    /// however many threads ask, only one is ever inside at a time.
    ///
    /// Written before the commands use it, so the mechanism is proven on its own
    /// rather than argued about through four call sites that need real audio
    /// hardware to exercise.
    #[test]
    fn only_one_lifecycle_command_runs_at_a_time() {
        let state = Arc::new(state());
        let inside = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let state = Arc::clone(&state);
                let inside = Arc::clone(&inside);
                let peak = Arc::clone(&peak);
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        let _guard = state.lifecycle_guard();

                        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        // Widen the window a real command would occupy, so an
                        // overlap has somewhere to happen.
                        std::thread::yield_now();
                        inside.fetch_sub(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();

        for thread in threads {
            thread.join().expect("thread panicked");
        }

        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "two lifecycle commands were inside the guard at once"
        );
    }

    /// `start_recording` is a chain of `?`s. If an early return could strand the
    /// guard, the first failed start would wedge every later one — including the
    /// stop for a recording already in progress.
    #[test]
    fn an_early_return_releases_the_guard() {
        let state = state();

        fn fails_early(state: &AppState) -> Result<(), &'static str> {
            let _guard = state.lifecycle_guard();
            Err("as `?` would")
        }

        assert!(fails_early(&state).is_err());

        // Would block forever if the guard had leaked, so the test is the
        // timeout rather than the assertion.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn({
            let state = Arc::new(state);
            move || {
                let _guard = state.lifecycle_guard();
                let _ = tx.send(());
            }
        });

        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the guard was not released by the early return"
        );
    }
}
