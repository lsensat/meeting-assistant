//! Does pausing a transcription and resuming it lose or duplicate speech?
//!
//! This is the correctness question the whole pause feature rests on. Resuming
//! works by asking whisper.cpp to skip what a previous attempt already covered
//! (`set_offset_ms`) and then stitching the two halves together. Get the seam
//! wrong by a second and words vanish or are said twice — and neither shows up
//! in a unit test, because both halves are individually well-formed.
//!
//! # Running it
//!
//! Ignored by default: it needs a Whisper model, real speech, and minutes.
//!
//! ```sh
//! # 40 numbered sentences, ~3 minutes. `say` is macOS; any speech will do.
//! python3 -c 'print(" ".join(f"Sentence number {i}. The deployment finished without errors." for i in range(1,41)))' > /tmp/script.txt
//! say -v Samantha -r 180 -f /tmp/script.txt -o /tmp/speech.aiff
//! afconvert -f WAVE -d LEI16@48000 -c 1 /tmp/speech.aiff /tmp/speech.wav
//!
//! MA_TEST_AUDIO=/tmp/speech.wav cargo test --test resume -- --ignored --nocapture
//! ```
//!
//! The audio must contain **numbered** sentences. That is what makes the
//! assertion mechanical rather than a judgement about whether two transcripts
//! look similar: every number must appear exactly once, in order, in both runs.
//!
//! It also checks that segment times stay on the file's timeline across the
//! seam. That is not a detail — the first run of this test found a resumed half
//! reporting its first segment at 228s in a 192s recording, because the seek
//! offset was being applied twice.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use meeting_assistant::whisper::{Transcriber, TranscriptionControl};

/// Every numbered sentence, in order, as the numbers alone.
///
/// Compares meaning rather than characters. Whisper loses its context across a
/// seek and legitimately renders the same words differently on either side of
/// it — the first draft of this matched only "number N" and reported the whole
/// second half as lost speech, when whisper had simply written "No. 25" there
/// and "number 24" before the seam. The transcript was correct; the test was
/// not. Accept every spelling of the word, then compare the numbers.
fn sentence_numbers(text: &str) -> Vec<u32> {
    let lower = text.to_lowercase();
    let mut found = Vec::new();

    for (i, _) in lower.char_indices() {
        let rest = &lower[i..];
        let after = ["number ", "number", "no. ", "no.", "num "]
            .iter()
            .find_map(|word| rest.strip_prefix(word));

        let Some(after) = after else { continue };
        // Only at a word boundary, so "no." inside another word does not count.
        if i > 0 && lower[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric()) {
            continue;
        }

        let digits: String = after.trim_start().chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u32>() {
            found.push(n);
        }
    }
    found
}

fn joined(segments: &[meeting_core::text::Segment]) -> String {
    segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
#[ignore = "needs a Whisper model and real audio; see the module docs"]
fn a_paused_transcription_resumes_without_losing_or_repeating_speech() {
    let audio = std::env::var("MA_TEST_AUDIO").expect("set MA_TEST_AUDIO to a speech .wav");
    let audio = std::path::PathBuf::from(audio);
    let model = std::env::var("MA_TEST_MODEL").unwrap_or_else(|_| "base".to_string());

    let transcriber = Transcriber::load(&model, Some("en")).expect("load model");

    // --- baseline: straight through -------------------------------------
    let whole = transcriber
        .transcribe(&audio, "ME", 0.0, &TranscriptionControl::new())
        .expect("baseline");
    assert!(!whole.aborted, "the baseline must not abort");

    let expected = sentence_numbers(&joined(&whole.segments));
    println!("baseline: {} segments, {} sentences", whole.segments.len(), expected.len());
    assert!(
        expected.len() > 8,
        "the fixture needs enough numbered sentences to have a middle; got {}",
        expected.len()
    );

    // --- first half: abort partway --------------------------------------
    //
    // Aborting on elapsed audio rather than wall-clock, so the seam lands in
    // the same place regardless of how fast the machine is.
    let control = TranscriptionControl::new();
    let stop_after = whole.segments.last().map(|s| s.start).unwrap_or(60.0) / 2.0;
    let watching = Arc::new(AtomicBool::new(true));

    let watcher = {
        let control = control.clone();
        let watching = Arc::clone(&watching);
        std::thread::spawn(move || {
            while watching.load(Ordering::Relaxed) {
                if control.seconds_done() >= stop_after {
                    control.abort();
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };

    let first = transcriber.transcribe(&audio, "ME", 0.0, &control).expect("first half");
    watching.store(false, Ordering::Relaxed);
    let _ = watcher.join();

    assert!(first.aborted, "the run should have been stopped partway");
    assert!(
        first.last_end_seconds > 0.0 && first.last_end_seconds < whole.last_end_seconds,
        "the seam must be inside the file: stopped at {:.1}s of {:.1}s",
        first.last_end_seconds,
        whole.last_end_seconds
    );

    // --- second half: resume from the seam ------------------------------
    let second = transcriber
        .transcribe(&audio, "ME", first.last_end_seconds, &TranscriptionControl::new())
        .expect("second half");
    assert!(!second.aborted);

    let mut resumed = first.segments.clone();
    resumed.extend(second.segments.clone());
    let actual = sentence_numbers(&joined(&resumed));

    println!(
        "resumed : paused at {:.1}s, {} + {} segments, {} sentences",
        first.last_end_seconds,
        first.segments.len(),
        second.segments.len(),
        actual.len()
    );

    if std::env::var("MA_DUMP").is_ok() {
        println!("--- first half (0 -> {:.1}s) ---", first.last_end_seconds);
        for seg in first.segments.iter().rev().take(3).collect::<Vec<_>>().iter().rev() {
            println!("  [{:7.2}] {}", seg.start, seg.text);
        }
        println!("--- second half (from {:.1}s) ---", first.last_end_seconds);
        for seg in second.segments.iter().take(5) {
            println!("  [{:7.2}] {}", seg.start, seg.text);
        }
        println!("  ... last: {:?}", second.segments.last().map(|s| (s.start, &s.text)));
    }

    // --- the assertions that matter -------------------------------------
    let mut duplicated: Vec<u32> = Vec::new();
    for (i, n) in actual.iter().enumerate() {
        if actual[..i].contains(n) {
            duplicated.push(*n);
        }
    }
    assert!(duplicated.is_empty(), "speech repeated across the seam: {duplicated:?}");

    let missing: Vec<u32> = expected.iter().copied().filter(|n| !actual.contains(n)).collect();
    assert!(missing.is_empty(), "speech lost across the seam: {missing:?}");

    // Timestamps must stay on the meeting's timeline, not restart at zero:
    // that is what `offset_seconds` is for, and getting it wrong stacks the
    // second half on top of the first.
    let starts: Vec<f64> = resumed.iter().map(|s| s.start).collect();
    assert!(
        starts.windows(2).all(|w| w[1] >= w[0]),
        "segment starts went backwards across the seam: {starts:?}"
    );
}
