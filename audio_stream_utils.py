"""
Small, hardware-independent audio conversion helpers.

Keeping these functions outside meeting.py lets pytest verify the same
conversion logic used by the real recording threads.
"""

import numpy as np


TARGET_SAMPLE_RATE = 48000


def to_mono_float(samples):
    array = np.asarray(samples, dtype=np.float32)

    if array.ndim == 0:
        return array.reshape(1)

    if array.ndim == 1:
        return array

    if array.shape[1] == 1:
        return array[:, 0]

    return array.mean(axis=1, dtype=np.float32)


def pcm16_bytes_to_mono_float(data, channels):
    channels = max(1, int(channels))

    array = np.frombuffer(
        data,
        dtype=np.int16,
    ).astype(np.float32)

    if channels > 1:
        usable = (
            len(array)
            // channels
            * channels
        )
        array = array[:usable]

        if usable:
            array = array.reshape(
                -1,
                channels,
            ).mean(
                axis=1,
                dtype=np.float32,
            )

    return array / 32768.0


def resample_mono(
    samples,
    source_rate,
    target_rate=TARGET_SAMPLE_RATE,
):
    mono = to_mono_float(samples)

    source_rate = int(source_rate)
    target_rate = int(target_rate)

    if mono.size == 0:
        return mono.astype(np.float32)

    if source_rate == target_rate:
        return mono.astype(
            np.float32,
            copy=False,
        )

    target_length = max(
        1,
        int(
            round(
                mono.size
                * target_rate
                / source_rate
            )
        ),
    )

    source_positions = np.arange(
        mono.size,
        dtype=np.float64,
    )

    target_positions = np.linspace(
        0,
        mono.size - 1,
        target_length,
        dtype=np.float64,
    )

    result = np.interp(
        target_positions,
        source_positions,
        mono,
    )

    return result.astype(np.float32)


def silent_samples(
    duration_seconds,
    sample_rate=TARGET_SAMPLE_RATE,
):
    duration_seconds = max(
        0.0,
        float(duration_seconds),
    )

    count = int(
        round(
            duration_seconds
            * int(sample_rate)
        )
    )

    return np.zeros(
        count,
        dtype=np.float32,
    )
