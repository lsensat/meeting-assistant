# asr-bakeoff

Throwaway probe: does any engine beat "whisper.cpp in chunks" for a **live**
transcript, on the machines this app actually runs on, in Spanish?

It measures, per engine, on the same audio: RTF, emit latency (simulated
single-worker queue), WER against a reference, and peak RSS. See the docstring
of `bakeoff.py` for exactly what is and is not modelled -- notably, contention
with the capture threads is **not**, so leave headroom.

## Status

* The harness itself (endpointer, queue simulation, WER) is tested:
  `python test_harness.py`.
* The **engine adapters have never run against a real model.** The sandbox this
  was written in cannot reach huggingface.co. Expect to fix small API
  mismatches in `WhisperCpp` / `SherpaOffline` / `run_sherpa_online` on first
  run (pywhispercpp's parameter names and sherpa-onnx's constructor arguments
  were written from memory of their docs).
* Moonshine and Voxtral Realtime are **not wired up** -- see below.

## Run

```bash
python -m venv .venv && . .venv/bin/activate
pip install -r requirements.txt

# reference: ~5 min of a real meeting (both tracks mixed, or one track), and a
# hand-corrected transcript of the same minutes as plain text.
python bakeoff.py run --engine whispercpp --model small --beam 1 --audio m.wav --reference m.txt
python bakeoff.py run --engine whispercpp --model small --beam 5 --audio m.wav --reference m.txt
python bakeoff.py run --engine whispercpp --model base  --beam 1 --audio m.wav --reference m.txt
python bakeoff.py run --engine sherpa-offline \
    --model-dir sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 --audio m.wav --reference m.txt
python bakeoff.py report results/*.json
```

Run it on the **slowest machine you support** (the Windows laptop), with
something recording at the same time, and with `--threads` matching what live
mode would be allowed to use.

Test audio matters more than engine choice. Use (a) a mic track, (b) a
*system-audio* track from a real call (codec-compressed, with overlap and
sometimes echo), and (c) a quiet-headset mic track like the one documented in
`vad.rs`.

## What the desk research found (2026-10-31)

Secondary sources only (vendor blogs, model cards via search snippets);
Hugging Face was unreachable, so none of it was checked against the model
cards. Re-verify before relying on any of it.

### 1. Spanish support

| Model | Spanish? | Notes |
|---|---|---|
| Parakeet TDT 0.6B **v3** | Yes (25 European languages, auto language ID) | Overall FLEURS avg WER ~11.97%; I did **not** find the Spanish row. v2 is English-only. |
| Voxtral Mini 4B Realtime | Yes (13 languages) | FLEURS Spanish WER 3.31% @ 480 ms, 2.98% @ 960 ms, 2.71% @ 2400 ms delay (model card, via search). Average over languages 8.72% @ 480 ms. |
| Moonshine (non-English) | Spanish listed among 8 languages | Could not confirm a **streaming** Spanish variant. |
| Zipformer streaming (sherpa-onnx) | Per-model; mostly English/Chinese in the common releases | Check the model list for a Spanish one. |
| Whisper (today) | Yes | Baseline. |

### 3. License and size

| Model | License | Size |
|---|---|---|
| Parakeet TDT 0.6B v3 | **Sources conflict**: CC BY 4.0 (NGC) vs NVIDIA Open Model License (build.nvidia.com). Read the HF card. | 600M params; int8 ONNX is what a laptop would run. File size not found. |
| Voxtral Mini 4B Realtime | Apache 2.0 | ~3.4B LM + ~0.97B encoder. vLLM recipe says one GPU with >=16 GB VRAM (BF16). A third party quotes ~2.5 GB for a Q4 GGUF -- unconfirmed. **Not a CPU-laptop model.** |
| Moonshine English | MIT | tiny/small/medium streaming variants. |
| Moonshine non-English (incl. Spanish) | **Moonshine AI Community License**; sources disagree: non-commercial vs free under $1M annual revenue. | Read the license text itself before shipping. |
| sherpa-onnx (runtime) | Apache 2.0 | Models carry their own licenses. |

### 4. Meeting / system-audio robustness

**No independent data found** for compressed loopback audio, overlap or echo on
Parakeet, Moonshine or Voxtral. Indirect hints only: Whisper's deletions grow
with overlap (12.1% -> 28.2% in one child-adult study); Moonshine is said
(secondary review) to struggle with heavy accents and overlap; one single-user
blog test found a batch model (Parakeet v3) with live previews more accurate
than the pure-streaming models. This has to be measured -- hence the system-audio
track in the test set above.

## Not wired up, and why

* **Voxtral Realtime** -- needs a >=16 GB GPU (or an MLX/Apple-Silicon build)
  through vLLM/transformers/MLX; there is no CPU path I could confirm. Worth a
  separate run on a Mac with the MLX build, since its Spanish numbers are the
  best on paper.
* **Moonshine (v2 / streaming)** -- the sherpa-onnx constructor for v2 and the
  Spanish model packaging were not confirmed. Add an adapter once the license
  is understood; the `Engine` base class is all it needs.
