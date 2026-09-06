import numpy as np

from audio_stream_utils import (
    TARGET_SAMPLE_RATE,
    pcm16_bytes_to_mono_float,
    resample_mono,
    silent_samples,
)


def test_44100_block_resamples_to_48000_duration():
    source = np.zeros(
        441,
        dtype=np.float32,
    )

    result = resample_mono(
        source,
        44100,
        48000,
    )

    assert len(result) == 480


def test_stereo_pcm16_is_mixed_to_mono():
    stereo = np.array(
        [
            [10000, -10000],
            [20000, 10000],
        ],
        dtype=np.int16,
    )

    result = pcm16_bytes_to_mono_float(
        stereo.tobytes(),
        channels=2,
    )

    assert len(result) == 2
    assert abs(result[0]) < 1e-6
    assert result[1] > 0


def test_silent_samples_matches_gap_duration():
    result = silent_samples(
        0.7,
        TARGET_SAMPLE_RATE,
    )

    assert len(result) == 33600
    assert np.count_nonzero(result) == 0
