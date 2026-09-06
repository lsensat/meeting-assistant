//! Transcription progress math. Port of `progress_utils.py`.

/// Global transcription progress from 0 to 100.
///
/// Progress is weighted by the real duration of each track, so a short
/// microphone track and a long system-audio track do not each count for half.
///
/// * `position_seconds` — position within the track currently being transcribed
/// * `track_duration` — duration of that track
/// * `completed_duration` — total duration of tracks already finished
/// * `total_duration` — duration of all tracks combined
///
/// The final `round` is round-half-to-even to match Python's `round()`. With
/// `f64::round` instead, the 62.5% case in the ported tests would give 63.
pub fn transcription_percent(
    position_seconds: f64,
    track_duration: f64,
    completed_duration: f64,
    total_duration: f64,
) -> u8 {
    // Python coerces via float() inside a try/except and returns 0 on garbage.
    // Rust's type system rules out most of that; NaN is the remaining case.
    if [
        position_seconds,
        track_duration,
        completed_duration,
        total_duration,
    ]
    .iter()
    .any(|v| v.is_nan())
    {
        return 0;
    }

    let position_seconds = position_seconds.max(0.0);
    let track_duration = track_duration.max(0.0);
    let completed_duration = completed_duration.max(0.0);
    let total_duration = total_duration.max(0.0);

    if total_duration <= 0.0 {
        return 0;
    }

    let position_seconds = position_seconds.min(track_duration);
    let done = (completed_duration + position_seconds).min(total_duration);

    ((done / total_duration * 100.0).round_ties_even() as i64).clamp(0, 100) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_track_starts_at_zero() {
        assert_eq!(transcription_percent(0.0, 60.0, 0.0, 120.0), 0);
    }

    #[test]
    fn first_equal_track_finishes_at_50_percent() {
        assert_eq!(transcription_percent(60.0, 60.0, 0.0, 120.0), 50);
    }

    #[test]
    fn second_equal_track_starts_at_50_percent() {
        assert_eq!(transcription_percent(0.0, 60.0, 60.0, 120.0), 50);
    }

    #[test]
    fn second_track_midpoint_is_75_percent() {
        assert_eq!(transcription_percent(30.0, 60.0, 60.0, 120.0), 75);
    }

    #[test]
    fn final_progress_is_100_percent() {
        assert_eq!(transcription_percent(60.0, 60.0, 60.0, 120.0), 100);
    }

    #[test]
    fn progress_is_weighted_when_track_lengths_differ() {
        // First track 30 s, second 90 s.
        assert_eq!(transcription_percent(30.0, 30.0, 0.0, 120.0), 25);

        // 75/120 = 62.5%. Python's banker's rounding gives 62, not 63.
        // This case is why round_ties_even is used throughout.
        assert_eq!(transcription_percent(45.0, 90.0, 30.0, 120.0), 62);
    }

    #[test]
    fn progress_is_clamped() {
        assert_eq!(transcription_percent(100.0, 60.0, 60.0, 120.0), 100);
        assert_eq!(transcription_percent(-10.0, 60.0, 0.0, 120.0), 0);
    }

    #[test]
    fn zero_total_duration_is_zero() {
        assert_eq!(transcription_percent(10.0, 10.0, 0.0, 0.0), 0);
    }

    #[test]
    fn nan_is_zero() {
        assert_eq!(transcription_percent(f64::NAN, 60.0, 0.0, 120.0), 0);
    }
}
