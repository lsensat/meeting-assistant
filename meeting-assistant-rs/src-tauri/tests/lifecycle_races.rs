//! The gate for moving the lifecycle commands off the main thread.
//!
//! `start_recording`, `stop_recording`, `cancel_recording` and
//! `finalize_meeting` are serialised today only because Tauri runs plain
//! `#[tauri::command]` functions on the main thread. Making them `(async)` —
//! which they need, since they join writer threads, open audio devices and
//! delete multi-gigabyte folders while the window is frozen — removes that, and
//! `AppState::lifecycle_guard` is what replaces it.
//!
//! The guard's own property is proven in `state.rs` by eight threads that never
//! see two of them inside at once. That runs anywhere, on every commit. What it
//! cannot prove is that a real recording still comes out whole, because the
//! commands want an `AppHandle` and real audio hardware.
//!
//! So this file is the other half, and it is split the same way the risk is:
//!
//! * **Automated, below.** A real capture through `RecordingSession`, asserting
//!   both tracks are finalised — header patched, data chunk non-empty — and that
//!   the microphone track is roughly the length it should be.
//! * **Manual, in the list further down.** The paths that need a person, a
//!   mouse and two windows. No test can click a tray menu.
//!
//! # What the automated half does not cover
//!
//! Two limits, both worth knowing before a green run is mistaken for a pass.
//!
//! It drives `RecordingSession` directly, so it exercises none of the command
//! layer — not the lifecycle guard, not `(async)`, not the tray. Those are only
//! reachable through the real UI. A green run here says the recording machinery
//! still works; the manual list is what gates a lifecycle change.
//!
//! And it deliberately asserts nothing about the system track's length or the
//! skew between tracks. Six consecutive runs on unchanged code moved the system
//! track from 577 KB to 647 KB and the skew from 2 ms to 660 ms, purely because
//! the audio engine warms up between runs — see the note at the assertions. Those
//! two numbers are printed, not checked, and are only meaningful compared against
//! another **cold** run.
//!
//! # Running the automated half
//!
//! Ignored by default: it opens the real microphone and the real output device,
//! and records for a few seconds.
//!
//! ```sh
//! cargo test --test lifecycle_races -- --ignored --nocapture
//! ```
//!
//! `MA_TEST_MIC` and `MA_TEST_SYSTEM` name the devices. Left unset, both fall
//! through to the selection policy's default, which is what the app does when a
//! configured device is missing.
//!
//! # The manual half
//!
//! Every one of these is a way to get two lifecycle commands overlapping, which
//! is the thing that could not happen before and can now. Run them **after**
//! wiring the commands to the guard, on a real machine, and treat any lost or
//! truncated recording as a blocker rather than a flake.
//!
//! 1. **Start, stop and cancel from the window.** The baseline. A meeting
//!    records, stops, and appears in the library with a summary.
//! 2. **The same three from the tray menu.** The tray calls `start_recording` as
//!    a plain Rust function rather than through the IPC wrapper, so `(async)`
//!    does not apply to it and it is a genuinely different path.
//! 3. **Start from the tray while stopping from the window**, and the reverse.
//!    The overlap the guard exists for. Expect the second to be refused, never
//!    to produce a second folder.
//! 4. **Press start twice, fast, from both surfaces.** Two folders would mean
//!    the guard is not covering the check; one truncated WAV would mean a
//!    session was overwritten rather than refused.
//! 5. **Cancel a long meeting** — twenty minutes or more, so `remove_dir_all`
//!    has real work. The window must stay responsive throughout; that
//!    responsiveness is the entire point of the change.
//! 6. **Launch a second instance while recording.** `tauri-plugin-single-instance`
//!    should reveal the running window and start nothing.
//! 7. **Quit with a recording in progress.** The session's `Drop` releases the
//!    system mute; confirm the microphone is not left muted afterwards.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use meeting_assistant::queue::Queue;
use meeting_assistant::session::{RecordingSession, MIC_FILENAME, SYSTEM_FILENAME};

/// Seconds of real audio to capture. Long enough that a device that opens
/// slowly — measured at 0.489s for the macOS process tap — is not most of it.
const CAPTURE_SECONDS: u64 = 6;

fn temp_folder() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "meeting-lifecycle-races-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ))
}

/// The data chunk's declared length, as the header states it.
///
/// Read by hand rather than through a WAV crate, because the thing being
/// checked *is* the header: `wav.rs` writes a placeholder length and patches it
/// on finalisation, so a track whose writer thread never finished — the exact
/// outcome of a session being dropped instead of stopped — leaves a file with
/// audio in it and a length field that still reads zero. A parser that trusts
/// the header reports an empty file; one that ignores it misses the bug.
///
/// The chunks are walked rather than `data` assumed at offset 36, for the same
/// reason `wav::repair` walks them: the assumption holds for what this app
/// writes today and would be wrong the day anything emits a LIST or fact chunk.
fn declared_data_len(path: &Path) -> u32 {
    let bytes = std::fs::read(path).expect("the track should be readable");
    assert!(
        bytes.len() >= 44,
        "{} is shorter than a WAV header",
        path.display()
    );
    assert_eq!(&bytes[0..4], b"RIFF", "{} is not RIFF", path.display());
    assert_eq!(&bytes[8..12], b"WAVE", "{} is not WAVE", path.display());

    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]);

        if &bytes[offset..offset + 4] == b"data" {
            return size;
        }
        // Chunks are padded to an even length.
        offset += 8 + size as usize + (size as usize % 2);
    }

    panic!("{} has no data chunk", path.display());
}

/// A recording that starts and stops leaves two finalised, aligned tracks.
///
/// The baseline the lifecycle change must not move. Everything it asserts is
/// something a dropped-instead-of-stopped session gets wrong: the header stays
/// unpatched, the data chunk reads as empty, and the track lengths diverge.
#[test]
#[ignore = "opens the real audio devices and records for several seconds"]
fn a_recording_that_starts_and_stops_leaves_two_finalised_tracks() {
    let folder = temp_folder();
    let microphone = std::env::var("MA_TEST_MIC").unwrap_or_default();
    let system = std::env::var("MA_TEST_SYSTEM").unwrap_or_default();

    println!(
        "recording {CAPTURE_SECONDS}s into {} (mic {:?}, system {:?})",
        folder.display(),
        if microphone.is_empty() { "<default>" } else { &microphone },
        if system.is_empty() { "<default>" } else { &system },
    );

    let session = RecordingSession::start(
        &folder,
        &microphone,
        &system,
        Arc::new(AtomicBool::new(false)),
        Arc::new(Queue::new()),
    )
    .expect("the session should start");

    std::thread::sleep(Duration::from_secs(CAPTURE_SECONDS));

    let summary = session.stop();

    for track in &summary.tracks {
        if let Err(e) = track {
            println!("track reported: {e}");
        }
    }

    let mic_len = declared_data_len(&folder.join(MIC_FILENAME));
    let system_len = declared_data_len(&folder.join(SYSTEM_FILENAME));

    // Asserted, for both tracks: either the writer thread finished and patched
    // the header or it did not, and nothing about the machine's state changes
    // which. This is the failure a session dropped instead of stopped produces,
    // and it is the one this file exists to catch.
    assert!(
        mic_len > 0,
        "the microphone header was never patched — the writer thread did not finish"
    );
    assert!(
        system_len > 0,
        "the system header was never patched — the writer thread did not finish"
    );

    // 48 kHz mono PCM16 is 96,000 bytes a second.
    let expected = CAPTURE_SECONDS as f64 * 96_000.0;

    // Asserted for the microphone only.
    //
    // A microphone is an ordinary capture endpoint: it is opened and it
    // delivers, so its length is a property of the code. Measured over six runs
    // it stayed inside 578-584 KB, and a short one means capture stopped early
    // or never began.
    assert!(
        mic_len as f64 / expected > 0.5,
        "the microphone captured {mic_len} bytes, less than half the {expected:.0} expected — \
         capture stopped early or never began"
    );

    // NOT asserted: the system track's length, and the skew between the tracks.
    //
    // The system track is a tap on what the machine is playing, not a device
    // that is simply open, and macOS runs that engine only while it has
    // something to run it for. Its start time is therefore a property of how
    // recently sound last played — `audio/devices.rs` says so directly, and
    // warns that a run started shortly after any audio "will capture fine and
    // look like a pass".
    //
    // Measured here across six consecutive runs: the system track climbed from
    // 577 KB to 647 KB and the skew from 2 ms to 660 ms, monotonically, then sat
    // flat — with no code change between them. Asserting on either would fail on
    // a cold machine, pass on a warm one, and tell you nothing about the code
    // in both cases. A 1-second skew limit was also six times looser than the
    // ~100 ms alignment budget it was supposed to be protecting, so it would
    // have slept through a real regression while crying wolf over this one.
    //
    // They are printed because they are worth *looking* at — just only ever
    // against another cold run.
    let skew = summary
        .skew_seconds()
        .map(|s| format!("{s:.3}s"))
        .unwrap_or_else(|| "n/a".to_string());
    println!(
        "microphone {mic_len} bytes — asserted\n\
         system {system_len} bytes, skew {skew} — informational only: both depend on \
         how recently audio last played on this machine. Compare them only between \
         cold runs, never between back-to-back ones."
    );

    std::fs::remove_dir_all(&folder).ok();
}
