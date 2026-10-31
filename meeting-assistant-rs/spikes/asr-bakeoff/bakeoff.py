#!/usr/bin/env python3
"""ASR bake-off for live transcription: same audio, several engines, one table.

Throwaway probe, like the other spikes. It answers: *could this engine give a
live transcript on the user's machine, and how good is it?* The numbers that
decide that are

* **RTF** (compute seconds per audio second) -- must stay well under 1.0 while
  the machine is also capturing audio;
* **emit latency** -- how long after a phrase ended its text appears;
* **WER** against a reference transcript;
* **peak RSS**.

# How latency is measured

Engines that cannot stream (whisper.cpp, Parakeet offline) are driven the way
the app would drive them: an energy endpointer closes a segment after a pause,
and a single worker transcribes segments in order. Time is *simulated*, not
slept: a segment closing at audio-time T starts when the worker is free, takes
its measured compute time, and its latency is finish - segment_end. A backlog
that grows is therefore visible as growing latency (RTF > 1 shows up as
unbounded latency).

What this does NOT model: contention with the live audio capture threads, which
makes real numbers worse. Run the bake-off while something is recording, or
treat the RTF with a safety factor of ~1.5.

# Usage

    python bakeoff.py run --engine whispercpp --model small --audio meeting.wav \
        --reference meeting.txt --lang es
    python bakeoff.py run --engine sherpa-offline --model-dir ./sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 ...
    python bakeoff.py report results/*.json
"""

from __future__ import annotations

import argparse
import json
import re
import resource
import subprocess
import sys
import time
import unicodedata
from dataclasses import dataclass, asdict, field
from pathlib import Path

import numpy as np

SR = 16_000


# --------------------------------------------------------------------------
# audio
# --------------------------------------------------------------------------


def load_audio(path: str) -> np.ndarray:
    """Any file ffmpeg can read -> 16 kHz mono float32."""
    out = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", path, "-ac", "1", "-ar", str(SR), "-f", "f32le", "-"],
        check=True,
        capture_output=True,
    ).stdout
    return np.frombuffer(out, dtype=np.float32).copy()


# --------------------------------------------------------------------------
# endpointing (approximates what vad.rs does; deliberately simple)
# --------------------------------------------------------------------------


@dataclass
class Segment:
    start: float  # seconds
    end: float
    samples: np.ndarray = field(repr=False)


def endpoint(
    audio: np.ndarray,
    pause_ms: int = 700,
    min_ms: int = 300,
    max_s: float = 15.0,
    threshold_db: float = -45.0,
    frame_ms: int = 30,
) -> list[Segment]:
    """Cut `audio` into speech segments by frame energy.

    A segment closes after `pause_ms` of quiet, or is force-cut at `max_s`
    (Whisper degrades on very long windows, and a live UI needs text before
    someone finishes a monologue).
    """
    frame = SR * frame_ms // 1000
    n = len(audio) // frame
    if n == 0:
        return []
    frames = audio[: n * frame].reshape(n, frame)
    rms = np.sqrt((frames**2).mean(axis=1) + 1e-12)
    voiced = 20 * np.log10(rms) > threshold_db

    pause_frames = max(1, pause_ms // frame_ms)
    max_frames = int(max_s * 1000 / frame_ms)
    min_frames = max(1, min_ms // frame_ms)

    segs: list[Segment] = []
    start = None
    quiet = 0
    last_voiced = 0

    def close(end_frame: int):
        if start is not None and end_frame - start >= min_frames:
            segs.append(
                Segment(start * frame / SR, end_frame * frame / SR, audio[start * frame : end_frame * frame])
            )

    for i, v in enumerate(voiced):
        if v:
            if start is None:
                start = i
            quiet = 0
            last_voiced = i
        elif start is not None:
            quiet += 1
            if quiet >= pause_frames:
                close(last_voiced + 1)
                start, quiet = None, 0
        if start is not None and i - start >= max_frames:
            close(i + 1)
            start, quiet = None, 0
    if start is not None:
        close(last_voiced + 1)
    return segs


# --------------------------------------------------------------------------
# WER
# --------------------------------------------------------------------------


def normalize(text: str) -> list[str]:
    """Lowercase, drop punctuation, keep accents (they matter in Spanish)."""
    text = unicodedata.normalize("NFC", text.lower())
    text = re.sub(r"[^\w\s']", " ", text, flags=re.UNICODE).replace("_", " ")
    return text.split()


def wer(reference: str, hypothesis: str) -> float:
    r, h = normalize(reference), normalize(hypothesis)
    if not r:
        return 0.0 if not h else 1.0
    prev = list(range(len(h) + 1))
    for i, rw in enumerate(r, 1):
        cur = [i]
        for j, hw in enumerate(h, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (rw != hw)))
        prev = cur
    return prev[-1] / len(r)


# --------------------------------------------------------------------------
# engines
# --------------------------------------------------------------------------


class Engine:
    """Offline engine: text for one finished segment."""

    name = "base"

    def transcribe(self, samples: np.ndarray) -> str:
        raise NotImplementedError


class WhisperCpp(Engine):
    """whisper.cpp through pywhispercpp -- the engine the app uses today."""

    def __init__(self, model: str, lang: str | None, threads: int, beam: int):
        from pywhispercpp.model import Model  # pip install pywhispercpp

        self.name = f"whispercpp-{model}-beam{beam}"
        self.lang = lang
        kw = {"n_threads": threads, "print_progress": False, "print_realtime": False}
        if lang:
            kw["language"] = lang
        if beam > 1:
            kw["beam_search"] = {"beam_size": beam, "patience": -1.0}
        self.model = Model(model, **kw)

    def transcribe(self, samples):
        return " ".join(s.text.strip() for s in self.model.transcribe(samples))


class SherpaOffline(Engine):
    """Any sherpa-onnx offline transducer (Parakeet TDT v3, ...), run per segment."""

    def __init__(self, model_dir: str, threads: int):
        import sherpa_onnx  # pip install sherpa-onnx

        d = Path(model_dir)

        def pick(stem):  # prefer int8 weights, they are what a laptop would run
            hits = sorted(d.glob(f"{stem}*.int8.onnx")) or sorted(d.glob(f"{stem}*.onnx"))
            if not hits:
                sys.exit(f"{model_dir}: no {stem}*.onnx")
            return str(hits[0])

        self.name = f"sherpa-offline-{d.name}"
        self.rec = sherpa_onnx.OfflineRecognizer.from_transducer(
            encoder=pick("encoder"),
            decoder=pick("decoder"),
            joiner=pick("joiner"),
            tokens=str(d / "tokens.txt"),
            num_threads=threads,
            sample_rate=SR,
            feature_dim=80,
            decoding_method="greedy_search",
            model_type="nemo_transducer",
        )

    def transcribe(self, samples):
        s = self.rec.create_stream()
        s.accept_waveform(SR, samples)
        self.rec.decode_stream(s)
        return s.result.text.strip()


def run_offline(engine: Engine, audio: np.ndarray, segs: list[Segment]) -> dict:
    """Single-worker queue simulation. See the module docstring."""
    worker_free = 0.0
    compute_total = 0.0
    lat = []
    texts = []
    for seg in segs:
        # a segment can only be handed over once it has closed: its end plus
        # the pause the endpointer needed to be sure.
        t0 = time.perf_counter()
        text = engine.transcribe(seg.samples)
        c = time.perf_counter() - t0
        start = max(seg.end, worker_free)
        worker_free = start + c
        lat.append(worker_free - seg.end)
        compute_total += c
        texts.append(text)
    dur = len(audio) / SR
    return {
        "rtf": compute_total / dur if dur else 0.0,
        "compute_s": compute_total,
        "latency_p50": float(np.percentile(lat, 50)) if lat else 0.0,
        "latency_p95": float(np.percentile(lat, 95)) if lat else 0.0,
        "latency_max": max(lat) if lat else 0.0,
        "segments": len(segs),
        "text": " ".join(t for t in texts if t),
    }


def run_sherpa_online(model_dir: str, audio: np.ndarray, threads: int, chunk_ms: int) -> tuple[str, dict]:
    """Truly streaming transducer (Zipformer ...): feed chunks, time each step."""
    import sherpa_onnx

    d = Path(model_dir)

    def pick(stem):
        hits = sorted(d.glob(f"{stem}*.int8.onnx")) or sorted(d.glob(f"{stem}*.onnx"))
        if not hits:
            sys.exit(f"{model_dir}: no {stem}*.onnx")
        return str(hits[0])

    rec = sherpa_onnx.OnlineRecognizer.from_transducer(
        tokens=str(d / "tokens.txt"),
        encoder=pick("encoder"),
        decoder=pick("decoder"),
        joiner=pick("joiner"),
        num_threads=threads,
        sample_rate=SR,
        feature_dim=80,
        enable_endpoint_detection=True,
        decoding_method="greedy_search",
    )
    stream = rec.create_stream()
    step = SR * chunk_ms // 1000
    per_chunk = []
    finals: list[str] = []
    for i in range(0, len(audio), step):
        t0 = time.perf_counter()
        stream.accept_waveform(SR, audio[i : i + step])
        while rec.is_ready(stream):
            rec.decode_stream(stream)
        if rec.is_endpoint(stream):
            text = rec.get_result(stream)
            text = text if isinstance(text, str) else text.text
            if text.strip():
                finals.append(text.strip())
            rec.reset(stream)
        per_chunk.append(time.perf_counter() - t0)
    stream.input_finished()
    while rec.is_ready(stream):
        rec.decode_stream(stream)
    tail = rec.get_result(stream)
    tail = tail if isinstance(tail, str) else tail.text
    if tail.strip():
        finals.append(tail.strip())
    dur = len(audio) / SR
    pc = np.array(per_chunk)
    return " ".join(finals), {
        "rtf": float(pc.sum() / dur),
        "compute_s": float(pc.sum()),
        # a word is at best chunk_ms old when it is fed, plus the step's compute
        "latency_p50": chunk_ms / 1000 + float(np.percentile(pc, 50)),
        "latency_p95": chunk_ms / 1000 + float(np.percentile(pc, 95)),
        "latency_max": chunk_ms / 1000 + float(pc.max()),
        "segments": len(finals),
    }


# --------------------------------------------------------------------------
# cli
# --------------------------------------------------------------------------


def cmd_run(a):
    audio = load_audio(a.audio)
    dur = len(audio) / SR
    ref = Path(a.reference).read_text() if a.reference else None

    if a.engine == "sherpa-online":
        text, m = run_sherpa_online(a.model_dir, audio, a.threads, a.chunk_ms)
        name = f"sherpa-online-{Path(a.model_dir).name}"
    else:
        if a.engine == "whispercpp":
            eng = WhisperCpp(a.model, a.lang, a.threads, a.beam)
        elif a.engine == "sherpa-offline":
            eng = SherpaOffline(a.model_dir, a.threads)
        else:
            sys.exit(f"unknown engine {a.engine}")
        segs = endpoint(audio, pause_ms=a.pause_ms)
        m = run_offline(eng, audio, segs)
        text, name = m.pop("text"), eng.name

    # ru_maxrss is KiB on Linux, bytes on macOS.
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    rss_mb = rss / (1024 * 1024) if sys.platform == "darwin" else rss / 1024

    out = {
        "engine": name,
        "audio": Path(a.audio).name,
        "audio_s": dur,
        "threads": a.threads,
        "peak_rss_mb": rss_mb,
        "wer": wer(ref, text) if ref else None,
        "text": text,
        **m,
    }
    Path(a.out).mkdir(parents=True, exist_ok=True)
    path = Path(a.out) / f"{name}.json"
    path.write_text(json.dumps(out, ensure_ascii=False, indent=2))
    print(f"wrote {path}")
    print(format_row(out))


def format_row(r: dict) -> str:
    wer_s = "n/a" if r["wer"] is None else f"{r['wer'] * 100:.1f}%"
    return (
        f"| {r['engine']} | {r['rtf']:.2f} | {r['latency_p50']:.1f} / {r['latency_p95']:.1f} / "
        f"{r['latency_max']:.1f} | {wer_s} | {r['peak_rss_mb']:.0f} | {r['threads']} |"
    )


def cmd_report(a):
    rows = [json.loads(Path(p).read_text()) for p in a.files]
    print("| engine | RTF | latency p50 / p95 / max (s) | WER | peak RSS (MB) | threads |")
    print("|---|---|---|---|---|---|")
    for r in sorted(rows, key=lambda r: r["rtf"]):
        print(format_row(r))
    print("\nRTF < ~0.6 leaves headroom for capture; WER is only comparable on the same audio + reference.")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("run")
    r.add_argument("--engine", required=True, choices=["whispercpp", "sherpa-offline", "sherpa-online"])
    r.add_argument("--audio", required=True)
    r.add_argument("--reference", help="plain-text reference transcript (hand-corrected)")
    r.add_argument("--lang", default="es")
    r.add_argument("--model", default="small", help="whispercpp model name")
    r.add_argument("--model-dir", help="sherpa-onnx model directory")
    r.add_argument("--threads", type=int, default=4)
    r.add_argument("--beam", type=int, default=1, help="whispercpp beam (app uses 5; 1 = greedy)")
    r.add_argument("--pause-ms", type=int, default=700)
    r.add_argument("--chunk-ms", type=int, default=100)
    r.add_argument("--out", default="results")
    r.set_defaults(fn=cmd_run)

    rp = sub.add_parser("report")
    rp.add_argument("files", nargs="+")
    rp.set_defaults(fn=cmd_report)

    a = p.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
