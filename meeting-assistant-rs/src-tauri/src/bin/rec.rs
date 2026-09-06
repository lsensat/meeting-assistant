//! Temporary CLI driver for Phase M2. No UI, no transcription — just the two
//! recorders, so the audio layer can be exercised and diffed against the Python
//! app's output before any Tauri code exists.
//!
//! ```text
//! cargo run --bin rec -- --list
//! cargo run --bin rec -- --seconds 120 --out /tmp/meeting
//! cargo run --bin rec -- --seconds 60 --mic "MacBook Air Microphone" --system "EarPods"
//! cargo run --bin rec -- --seconds 30 --mute-after 10
//! ```
//!
//! Verify the result with `afinfo` on macOS: both files must report 48000 Hz,
//! 1 channel, and durations within ~100 ms of each other.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use meeting_assistant::audio::devices::{self, SourceKind};
use meeting_assistant::audio::recorder::Event;
use meeting_assistant::session::RecordingSession;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--list") {
        list();
        return;
    }

    let seconds: u64 = flag(&args, "--seconds")
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let mute_after: Option<u64> = flag(&args, "--mute-after").and_then(|s| s.parse().ok());
    let mic = flag(&args, "--mic").unwrap_or_default();
    let system = flag(&args, "--system").unwrap_or_default();
    let out = flag(&args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(default_folder);

    println!("folder     : {}", out.display());
    println!(
        "microphone : {}",
        if mic.is_empty() { "<default>" } else { &mic }
    );
    println!(
        "system     : {}",
        if system.is_empty() {
            "<default>"
        } else {
            &system
        }
    );
    println!("duration   : {seconds}s\n");

    // The CLI never mutes; the flag exists so the app can pre-set it.
    let muted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let session = match RecordingSession::start(&out, &mic, &system, muted) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("could not start recording: {e}");
            std::process::exit(1);
        }
    };

    let started = Instant::now();
    let mut muted = false;

    while started.elapsed() < Duration::from_secs(seconds) {
        // Drain events the way the UI will, so anything the recorder reports
        // is visible during the run rather than only in the summary.
        while let Ok(event) = session.events.try_recv() {
            match event {
                Event::Device(kind, name) => println!("  [{}] {name}", kind.label()),
                Event::Fallback(kind, name) => {
                    println!("  [{}] {name} (automatic)", kind.label())
                }
                Event::Log(message) => println!("  · {message}"),
                Event::Error(message) => eprintln!("  !! {message}"),
            }
        }

        if let Some(at) = mute_after {
            if !muted && started.elapsed() >= Duration::from_secs(at) {
                muted = true;
                session.set_muted(true);
                println!("  · microphone muted at {at}s");
            }
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    println!("\nstopping...");
    let summary = session.stop();

    println!("\n----------------------------------------------------------");
    println!("elapsed : {:.2}s", summary.elapsed_seconds);
    for track in &summary.tracks {
        match track {
            Ok(t) => {
                println!(
                    "\n{:<13}: {}",
                    t.kind.label(),
                    t.path.file_name().unwrap_or_default().to_string_lossy()
                );
                println!("  device     : {}", t.final_device);
                println!("  duration   : {:.3}s ({} frames)", t.duration_seconds, t.frames);
                println!("  fallback   : {}", t.automatic_fallback);
                println!("  overflows  : {}", t.overflows);
                println!("  gap frames : {}", t.gap_frames);
            }
            Err(e) => println!("\nTRACK FAILED: {e}"),
        }
    }

    if let Some(skew) = summary.skew_seconds() {
        println!("\nskew between tracks: {:.3}s", skew);
        // The alignment gate from the plan's verification section.
        if skew > 0.1 {
            println!("  >> ABOVE the 100 ms alignment budget. Investigate before trusting these files.");
        } else {
            println!("  >> within the 100 ms alignment budget.");
        }
    }
    println!("----------------------------------------------------------");
}

fn list() {
    for kind in [SourceKind::Microphone, SourceKind::SystemAudio] {
        println!("== {} ==", kind.label());
        match devices::snapshot(kind) {
            Ok(snapshot) => {
                for device in &snapshot.devices {
                    let marker = if snapshot.default_id() == Some(device.id.as_str()) {
                        " (default)"
                    } else {
                        ""
                    };
                    println!(
                        "  {}{marker}\n      {} Hz, {} ch\n      id: {}",
                        device.name, device.sample_rate, device.channels, device.id
                    );
                }
            }
            Err(e) => println!("  <enumeration failed: {e}>"),
        }
        println!();
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let index = args.iter().position(|a| a == name)?;
    args.get(index + 1).cloned()
}

fn default_folder() -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "meeting-assistant-rec-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    ));
    path
}
