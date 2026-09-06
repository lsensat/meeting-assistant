//! Text helpers: filenames, timestamps, transcript assembly and chunking.
//! Ports of `sanitize_name`, `format_time`, `split_transcript` and the
//! transcript-building loop in `process_meeting` (`app.py:2333-2341`).

/// Characters Windows forbids in a filename. Ported exactly from
/// `app.py:675` — do not widen this set, it is what existing folder names
/// were sanitised against.
const ILLEGAL_FILENAME_CHARS: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Maximum length of a sanitised meeting name, before trailing trim.
const MAX_NAME_LEN: usize = 80;

/// Default chunk size for splitting a transcript before summarisation.
pub const DEFAULT_CHUNK_CHARS: usize = 10_000;

/// Make a user-supplied meeting name safe for a folder name.
///
/// Port of `sanitize_name` (`app.py:670`): trim, drop Windows-illegal
/// characters, collapse whitespace runs to `_`, truncate to 80, then strip
/// leading and trailing `.`, `_` and `-`.
///
/// The truncation counts **characters, not bytes**, so accented names are not
/// cut mid-character — Python sliced a `str`, which is also by character.
pub fn sanitize_name(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let without_illegal: String = trimmed
        .chars()
        .filter(|c| !ILLEGAL_FILENAME_CHARS.contains(c))
        .collect();

    // Collapse each run of whitespace to a single underscore.
    let mut collapsed = String::with_capacity(without_illegal.len());
    let mut in_whitespace = false;
    for c in without_illegal.chars() {
        if c.is_whitespace() {
            if !in_whitespace {
                collapsed.push('_');
                in_whitespace = true;
            }
        } else {
            collapsed.push(c);
            in_whitespace = false;
        }
    }

    let truncated: String = collapsed.chars().take(MAX_NAME_LEN).collect();
    truncated.trim_matches(['.', '_', '-']).to_string()
}

/// Format seconds as `HH:MM:SS`. Port of `format_time` (`app.py:662`).
/// Negative input clamps to zero; hours are not capped at 24.
pub fn format_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let total = seconds as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// One transcribed span of speech, already offset onto the shared timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Seconds from the start of the meeting, not from the start of the track.
    pub start: f64,
    /// Localised speaker label — `ME`/`YO` or `MEETING`/`REUNION`.
    pub speaker: String,
    pub text: String,
}

/// Merge both tracks into the final transcript.
///
/// Port of `app.py:2330-2341`. Segments are sorted by start time and rendered
/// as `[HH:MM:SS] SPEAKER: text`, blank-line separated. The sort must be
/// **stable**, so that when the two tracks produce identically-timed segments
/// the microphone's come first, matching Python's `list.sort()` on a list built
/// as `mic_segments + system_segments`.
pub fn build_transcript(segments: &mut [Segment]) -> String {
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));

    segments
        .iter()
        .map(|s| format!("[{}] {}: {}", format_time(s.start), s.speaker, s.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Split a transcript into chunks for the map step of summarisation.
///
/// Port of `split_transcript` (`app.py:1939`). Splits only on line boundaries,
/// so a single line longer than `max_chars` becomes an oversized chunk rather
/// than being cut mid-sentence — deliberate, and preserved.
pub fn split_transcript(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut current_size = 0usize;

    for line in text.lines() {
        // Python counts len(line) + 1 for the newline it will rejoin with.
        let size = line.chars().count() + 1;

        if !current.is_empty() && current_size + size > max_chars {
            chunks.push(current.join("\n"));
            current.clear();
            current_size = 0;
        }

        current.push(line);
        current_size += size;
    }

    if !current.is_empty() {
        chunks.push(current.join("\n"));
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- sanitize_name ----------------------------------------------------

    #[test]
    fn empty_and_blank_names_stay_empty() {
        assert_eq!(sanitize_name(""), "");
        assert_eq!(sanitize_name("   "), "");
    }

    #[test]
    fn spaces_become_underscores() {
        assert_eq!(sanitize_name("Weekly sync"), "Weekly_sync");
    }

    #[test]
    fn whitespace_runs_collapse_to_one_underscore() {
        assert_eq!(sanitize_name("a   b"), "a_b");
        assert_eq!(sanitize_name("a\t\nb"), "a_b");
    }

    #[test]
    fn illegal_characters_are_removed() {
        assert_eq!(sanitize_name(r#"a<>:"/\|?*b"#), "ab");
    }

    #[test]
    fn leading_and_trailing_punctuation_is_stripped() {
        assert_eq!(sanitize_name("__name--"), "name");
        assert_eq!(sanitize_name("...name..."), "name");
    }

    /// Hyphens inside a name must survive — they are ordinary in meeting
    /// titles ("daily-standup"). Only a name made entirely of the stripped
    /// characters collapses to empty, and that is `strip("._-")` doing exactly
    /// what it is meant to.
    ///
    /// Written after a report of the Python app hanging on "Saving audio..."
    /// with a hyphenated title. The hyphen turned out to be unrelated — the
    /// real cause was the unbounded blocking `stream.read(CHUNK)` at
    /// `app.py:1587` wedging a recorder thread, so `join()` never returned and
    /// the WAV was never finalized. These cases pin down the innocent half so
    /// the question does not get re-litigated.
    #[test]
    fn hyphens_inside_a_name_survive() {
        assert_eq!(sanitize_name("daily-standup"), "daily-standup");
        assert_eq!(sanitize_name("a-b"), "a-b");
        assert_eq!(sanitize_name("sprint-review-"), "sprint-review");
        assert_eq!(sanitize_name("-kickoff"), "kickoff");
    }

    #[test]
    fn a_name_of_only_stripped_characters_becomes_empty() {
        // The caller must treat "" as "no title given" and skip the rename
        // rather than producing a folder ending in a bare underscore.
        assert_eq!(sanitize_name("-"), "");
        assert_eq!(sanitize_name("--"), "");
        assert_eq!(sanitize_name(".-_"), "");
    }

    #[test]
    fn long_names_are_truncated_to_80_characters() {
        let name = sanitize_name(&"a".repeat(200));
        assert_eq!(name.chars().count(), 80);
    }

    #[test]
    fn truncation_does_not_split_a_character() {
        // 100 accented characters: cutting by bytes would corrupt one.
        let name = sanitize_name(&"á".repeat(100));
        assert_eq!(name.chars().count(), 80);
        assert!(name.chars().all(|c| c == 'á'));
    }

    #[test]
    fn accents_survive() {
        assert_eq!(sanitize_name("Reunión de equipo"), "Reunión_de_equipo");
    }

    #[test]
    fn a_name_of_only_illegal_characters_becomes_empty() {
        assert_eq!(sanitize_name("///"), "");
    }

    // --- format_time ------------------------------------------------------

    #[test]
    fn formats_as_hms() {
        assert_eq!(format_time(0.0), "00:00:00");
        assert_eq!(format_time(59.0), "00:00:59");
        assert_eq!(format_time(60.0), "00:01:00");
        assert_eq!(format_time(3661.0), "01:01:01");
    }

    #[test]
    fn truncates_rather_than_rounds() {
        // int(seconds) in Python truncates toward zero.
        assert_eq!(format_time(59.9), "00:00:59");
    }

    #[test]
    fn negative_time_clamps_to_zero() {
        assert_eq!(format_time(-5.0), "00:00:00");
    }

    #[test]
    fn hours_are_not_capped_at_a_day() {
        assert_eq!(format_time(90_000.0), "25:00:00");
    }

    // --- build_transcript -------------------------------------------------

    fn seg(start: f64, speaker: &str, text: &str) -> Segment {
        Segment {
            start,
            speaker: speaker.into(),
            text: text.into(),
        }
    }

    #[test]
    fn renders_the_expected_line_format() {
        let mut segments = vec![seg(0.0, "ME", "hello")];
        assert_eq!(build_transcript(&mut segments), "[00:00:00] ME: hello");
    }

    #[test]
    fn interleaves_both_tracks_by_time() {
        let mut segments = vec![
            seg(10.0, "ME", "second"),
            seg(0.0, "MEETING", "first"),
            seg(20.0, "ME", "third"),
        ];
        let out = build_transcript(&mut segments);
        let lines: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].ends_with("first"));
        assert!(lines[1].ends_with("second"));
        assert!(lines[2].ends_with("third"));
    }

    #[test]
    fn equal_timestamps_keep_input_order() {
        // Stable sort: the microphone track is concatenated first, so on a tie
        // it must still come first.
        let mut segments = vec![seg(5.0, "ME", "mic"), seg(5.0, "MEETING", "system")];
        let out = build_transcript(&mut segments);
        assert!(out.starts_with("[00:00:05] ME: mic"));
    }

    #[test]
    fn empty_input_gives_empty_transcript() {
        assert_eq!(build_transcript(&mut []), "");
    }

    // --- split_transcript -------------------------------------------------

    #[test]
    fn short_text_is_one_chunk() {
        let chunks = split_transcript("a\nb\nc", DEFAULT_CHUNK_CHARS);
        assert_eq!(chunks, vec!["a\nb\nc"]);
    }

    #[test]
    fn empty_text_gives_no_chunks() {
        assert!(split_transcript("", DEFAULT_CHUNK_CHARS).is_empty());
    }

    #[test]
    fn splits_on_line_boundaries() {
        // Each line costs 6 chars (5 + newline), so a limit of 12 fits two.
        let text = "aaaaa\nbbbbb\nccccc\nddddd";
        let chunks = split_transcript(text, 12);
        assert_eq!(chunks, vec!["aaaaa\nbbbbb", "ccccc\nddddd"]);
    }

    #[test]
    fn a_single_oversized_line_is_not_cut() {
        // Parity: Python only breaks between lines, so one long line overflows
        // rather than being split mid-sentence.
        let long = "x".repeat(50);
        let chunks = split_transcript(&long, 10);
        assert_eq!(chunks, vec![long]);
    }

    #[test]
    fn every_line_survives_the_split() {
        let text: String = (0..500).map(|i| format!("line {i}\n")).collect();
        let chunks = split_transcript(&text, 100);
        let rejoined = chunks.join("\n");
        for i in 0..500 {
            assert!(rejoined.contains(&format!("line {i}")), "lost line {i}");
        }
    }
}
