//! A per-meeting record of what the recorder actually did.
//!
//! # Why this exists
//!
//! A meeting was recorded on Windows and produced two tracks of pure silence.
//! Every track reported the device "disappearing" for the whole meeting, which
//! means no audio callback ever fired — and the app could say nothing more,
//! because everything it knew went to `Event::Log`, which the frontend forwards
//! to `console.log`. A release build has no console to open, so the evidence
//! existed for a few milliseconds and was then discarded.
//!
//! Several plausible causes were argued for and none could be told apart:
//! whether the stream failed to open, opened and delivered nothing, whether the
//! device's reported format matched what was requested, whether a callback
//! errored. Each guess was cheap; the answer was not available at any price.
//!
//! So the recorder now writes what it sees next to the audio it wrote it for.
//! The file travels with the meeting through the folder rename, survives the
//! app closing, and can be read by whoever owns the machine — which for an app
//! whose whole premise is that nothing leaves the computer is the only way a
//! failure on someone else's laptop can be diagnosed at all.
//!
//! # What it is not
//!
//! Not telemetry: nothing is sent anywhere. Not a general logger: it records
//! one recording, in the folder of the meeting it belongs to.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

/// The file, inside each meeting folder.
pub const LOG_FILENAME: &str = "recorder.log";

/// An open diagnostics file for one meeting.
pub struct MeetingLog {
    file: Mutex<Option<std::fs::File>>,
    started: Instant,
}

impl MeetingLog {
    /// Create the log in `folder`, or a silent no-op if it cannot be written.
    ///
    /// Failing to open this must never stop a recording: the log exists to
    /// explain a bad meeting, and refusing to record because the explanation
    /// cannot be written would be a poor trade.
    pub fn create(folder: &Path) -> Self {
        // The folder is created here rather than assumed. Callers open this at
        // different points relative to the recorder starting, and a log that
        // silently writes nowhere because it was opened one line too early is
        // exactly the failure this file exists to prevent.
        let _ = std::fs::create_dir_all(folder);
        let file = std::fs::File::create(folder.join(LOG_FILENAME)).ok();
        let log = Self {
            file: Mutex::new(file),
            started: Instant::now(),
        };
        log.header();
        log
    }

    /// A log that writes nowhere, for callers with no folder.
    pub fn disabled() -> Self {
        Self {
            file: Mutex::new(None),
            started: Instant::now(),
        }
    }

    fn header(&self) {
        self.line(&format!(
            "Meeting Assistant {} on {} ({})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        self.line("Times are seconds since the recording started.");
        self.line("---");
    }

    /// Append one line, stamped with the time since the recording began.
    ///
    /// Flushed immediately. A meeting that ends in a crash is exactly the one
    /// worth reading, and a buffered line is not there to read.
    pub fn line(&self, message: &str) {
        let Ok(mut guard) = self.file.lock() else {
            return;
        };
        let Some(file) = guard.as_mut() else {
            return;
        };

        let elapsed = self.started.elapsed().as_secs_f64();
        let _ = writeln!(file, "[{elapsed:8.3}] {message}");
        let _ = file.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_records_what_it_is_given() {
        let dir = std::env::temp_dir().join(format!("ma-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");

        let log = MeetingLog::create(&dir);
        log.line("microphone: opened \"Headset\" at 48000 Hz");
        drop(log);

        let text = std::fs::read_to_string(dir.join(LOG_FILENAME)).expect("read");
        assert!(text.contains("Meeting Assistant"), "no header: {text}");
        assert!(text.contains("opened \"Headset\""), "line missing: {text}");
        // The timestamp is what makes a watchdog loop legible.
        assert!(text.contains('['), "no timestamps: {text}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A folder that cannot be written must not stop a recording.
    #[test]
    fn an_unwritable_folder_is_survivable() {
        // A path under a file, which cannot be made into a directory.
        let blocker = std::env::temp_dir().join(format!("ma-blocker-{}", std::process::id()));
        std::fs::write(&blocker, b"x").expect("write");
        let log = MeetingLog::create(&blocker.join("under-a-file"));
        log.line("this goes nowhere and must not panic");
        std::fs::remove_file(&blocker).ok();
    }

    #[test]
    fn a_disabled_log_accepts_lines() {
        MeetingLog::disabled().line("nowhere");
    }
}
