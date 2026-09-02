def transcription_percent(
    position_seconds,
    track_duration,
    completed_duration,
    total_duration,
):
    """
    Return global transcription progress as an integer from 0 to 100.

    `completed_duration` is the duration of tracks already completed.
    `position_seconds` is the current position in the track being transcribed.
    """
    try:
        position_seconds = max(0.0, float(position_seconds))
        track_duration = max(0.0, float(track_duration))
        completed_duration = max(0.0, float(completed_duration))
        total_duration = max(0.0, float(total_duration))
    except (TypeError, ValueError):
        return 0

    if total_duration <= 0:
        return 0

    position_seconds = min(
        position_seconds,
        track_duration,
    )

    done = min(
        total_duration,
        completed_duration + position_seconds,
    )

    return max(
        0,
        min(
            100,
            int(round(done / total_duration * 100)),
        ),
    )
