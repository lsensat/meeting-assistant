"""Checks for the parts of the bake-off that need no model: run `python test_harness.py`."""

import time

import numpy as np

import bakeoff as b


def tone(seconds, amp=0.3):
    t = np.arange(int(seconds * b.SR)) / b.SR
    return (amp * np.sin(2 * np.pi * 220 * t)).astype(np.float32)


def silence(seconds):
    return np.zeros(int(seconds * b.SR), dtype=np.float32)


def test_wer():
    assert b.wer("hola mundo", "hola mundo") == 0
    assert b.wer("hola mundo", "Hola, mundo!") == 0  # case + punctuation ignored
    assert b.wer("hola mundo cruel", "hola mundo") == 1 / 3  # deletion
    assert b.wer("a b c d", "a x c d e") == 0.5  # substitution + insertion
    assert b.wer("más allá", "mas alla") == 1.0  # accents are NOT folded
    assert b.wer("", "") == 0 and b.wer("", "x") == 1


def test_endpoint_splits_on_pause():
    audio = np.concatenate([silence(1), tone(2), silence(1.5), tone(1), silence(1)])
    segs = b.endpoint(audio, pause_ms=700)
    assert len(segs) == 2, segs
    assert abs(segs[0].start - 1.0) < 0.1 and abs(segs[0].end - 3.0) < 0.1
    assert abs(segs[1].start - 4.5) < 0.1 and abs(segs[1].end - 5.5) < 0.1


def test_endpoint_keeps_short_pause_inside_segment():
    audio = np.concatenate([tone(1), silence(0.3), tone(1)])
    assert len(b.endpoint(audio, pause_ms=700)) == 1


def test_endpoint_force_cuts_long_speech():
    segs = b.endpoint(tone(40), max_s=15)
    assert len(segs) == 3 and all(s.end - s.start <= 15.1 for s in segs)


def test_endpoint_drops_blips_and_silence():
    assert b.endpoint(silence(5)) == []
    assert b.endpoint(np.concatenate([silence(1), tone(0.1), silence(1)])) == []


class Fake(b.Engine):
    """Takes `cost` seconds per segment, whatever its length."""

    name = "fake"

    def __init__(self, cost):
        self.cost = cost

    def transcribe(self, samples):
        time.sleep(self.cost)
        return "x"


def test_latency_is_compute_when_keeping_up():
    audio = np.concatenate([silence(1), tone(1), silence(2), tone(1), silence(2)])
    m = b.run_offline(Fake(0.1), audio, b.endpoint(audio))
    assert m["segments"] == 2
    assert 0.09 < m["latency_p50"] < 0.3  # no backlog: latency ~ compute


def test_backlog_shows_up_as_growing_latency():
    # phrases every 0.8s, engine needs 1.0s each: the queue must grow
    parts = []
    for _ in range(4):
        parts += [tone(0.4), silence(0.4)]
    audio = np.concatenate(parts)
    segs = b.endpoint(audio, pause_ms=300, min_ms=300)
    assert len(segs) == 4
    eng = Fake(1.0)
    m = b.run_offline(eng, audio, segs)
    assert m["latency_max"] > m["latency_p50"] > 0.9
    assert m["latency_max"] > 1.5  # 1.0 + 3 * 0.2 of accumulated backlog = 1.6


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_"):
            fn()
            print("ok", name)
