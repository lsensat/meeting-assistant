from progress_utils import transcription_percent


def test_first_track_starts_at_zero():
    assert transcription_percent(
        0,
        60,
        0,
        120,
    ) == 0


def test_first_equal_track_finishes_at_50_percent():
    assert transcription_percent(
        60,
        60,
        0,
        120,
    ) == 50


def test_second_equal_track_starts_at_50_percent():
    assert transcription_percent(
        0,
        60,
        60,
        120,
    ) == 50


def test_second_track_midpoint_is_75_percent():
    assert transcription_percent(
        30,
        60,
        60,
        120,
    ) == 75


def test_final_progress_is_100_percent():
    assert transcription_percent(
        60,
        60,
        60,
        120,
    ) == 100


def test_progress_is_weighted_when_track_lengths_differ():
    # First track is 30 s, second is 90 s.
    assert transcription_percent(
        30,
        30,
        0,
        120,
    ) == 25

    assert transcription_percent(
        45,
        90,
        30,
        120,
    ) == 62


def test_progress_is_clamped():
    assert transcription_percent(
        100,
        60,
        60,
        120,
    ) == 100

    assert transcription_percent(
        -10,
        60,
        0,
        120,
    ) == 0
