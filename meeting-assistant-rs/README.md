# Meeting Assistant — Rust + Tauri port

Records a meeting from two audio sources at once — your microphone and the
audio your computer is playing — transcribes both with whisper.cpp and
summarizes the merged transcript with a local Ollama model. Everything runs
offline.

It is a rewrite of an earlier Python/CustomTkinter app, and targets **Windows
and macOS from one codebase**.

## Layout

```
rust/
  crates/meeting-core/   portable logic: device policy, resampling, progress,
                         config, i18n, prompts, transcript text. No hardware,
                         no platform code, builds and tests anywhere.
  src-tauri/             the app: audio capture, Whisper, Ollama, pipeline,
                         Tauri shell.
  spikes/                throwaway probes kept for their findings.
  ui/                    frontend: plain ES modules, no bundler, no npm.
```

Only the audio layer is platform-split. If you find yourself writing
`#[cfg(target_os)]` outside `src-tauri/src/audio/` or `src-tauri/src/platform.rs`,
it belongs somewhere else.

## Building

```bash
cargo test                       # the regression gate; must stay green
cargo clippy --all-targets -- -D warnings
cargo run --bin meeting-assistant
```

The first build compiles whisper.cpp, which takes a few minutes. **No GPU
backend is enabled on either platform** — this used to say Metal was automatic on
Apple Silicon, which is wrong: `whisper-rs-sys` defines `GGML_METAL=OFF` unless
the `metal` feature is on. The Mac is fast because of Accelerate and NEON, on the
CPU. See "Measuring transcription speed" below.

### CLI drivers

Two binaries exercise the layers without the UI, which is how the audio and
pipeline phases were developed and how they should be debugged:

```bash
cargo run --release --bin rec -- --list
cargo run --release --bin rec -- --seconds 60 --out /tmp/meeting
cargo run --release --bin process -- --folder /tmp/meeting
```

## Installing a release build

Releases are built by `.github/workflows/release.yml` on a `v*` tag: a `.dmg`
for macOS (Apple Silicon) and `.msi` / `.exe` for Windows. They land as a
**draft** release that has to be published by hand, because the builds are
unsigned and the notes below have to go with them.

### The builds are unsigned, and both systems will object

**macOS.** Anything downloaded from a browser is quarantined, and Gatekeeper
refuses an unsigned bundle outright — usually with "Meeting Assistant is
damaged and can't be opened", which is misleading: the download is fine, it is
simply unsigned. Clear the quarantine flag:

```bash
xattr -dr com.apple.quarantine "/Applications/Meeting Assistant.app"
```

This does not apply to a locally built app — those are never quarantined,
which is why it does not show up during development.

**Windows.** SmartScreen shows "Windows protected your PC" for an unrecognised
publisher. **More info → Run anyway.**

Signing would remove both (Apple Developer ID; a Windows OV certificate), and
on macOS it would also stop the permission prompts recurring after every
update — see the note on ad-hoc signing below.

### Apple Silicon only

`macos-latest` runners are ARM, so the macOS build is `aarch64` and will not
run on an Intel Mac. The architecture is in the `.dmg` filename. A universal
build is deferred.

## macOS: permissions

The app needs **two separate permissions**, and they are different TCC
categories:

| What | Permission | Prompt shown |
|---|---|---|
| Microphone | Microphone | on first recording |
| Computer audio | Screen & System Audio Recording | on first recording |

System audio is captured with a **Core Audio process tap**, which requires
**macOS 14.4 or later**.

### The tap only hears the device it taps

A tap captures audio routed to one specific output device. If the recording
starts while your output is the built-in speakers and you then switch to
headphones, the tap keeps running and records **silence** — a full-length file
of nothing, with no error anywhere.

The recorder detects the switch and re-opens on the new default output, filling
the changeover with the exact amount of silence so the two tracks stay aligned.
**This follows the default output even when you have named a device in
Settings** — the choice there means "which output to listen to", not "record
silence if my audio goes somewhere else". `recorder.log` records every switch.

Measured, wired EarPods plugged in at 24s and pulled at 51s of an 89s meeting:
audio captured continuously at −14 to −22 dBFS throughout, 0.697s of gap across
both changeovers, 0.031s of skew between the tracks.

If a system-audio track ever comes back empty, this is still the first thing to
check — the tap is bound to one device, and only the switches the OS reports as
a default change can be followed.

### Ad-hoc signing re-prompts after every rebuild

Local builds are signed ad-hoc, and **TCC keys its grant to the signing
identity**, so each rebuild looks like a different app. After rebuilding:

1. Open **System Settings → Privacy & Security**.
2. Remove the stale `Meeting Assistant` entry under **Screen & System Audio
   Recording** and under **Microphone**.
3. Relaunch and grant again.

There is no public API to query or pre-request the audio-capture grant, so the
app cannot detect this state and warn you — it can only attempt the tap and
report the failure. Signing with a Developer ID certificate would end the
re-prompting; that is deliberately out of scope for now.

## Measuring transcription speed

Whisper runs at very different speeds on different machines: roughly 0.2x
realtime on an Apple Silicon Mac, and **3.8x slower than realtime** on a Windows
laptop before threading was fixed — a one-hour meeting taking close to four
hours. Both are CPU numbers; neither platform enables a GPU backend today.

Every meeting already measures itself. `meeting.json` records `duration_seconds`
and `timings.whisper.seconds`, so the ratio needs no instrumentation, and
`whisper_tuning` beside them says what produced it:

```json
"whisper_tuning": "threads=7 of 8, beam=5, suppress_nst=true"
```

Three knobs can be overridden from the environment, to measure without a
rebuild. They are **diagnostics, not settings** — unset, the app behaves exactly
as documented, and nothing in the UI offers them.

| Variable | Default | What it changes |
|---|---|---|
| `MEETING_ASSISTANT_WHISPER_THREADS` | cores - 1, capped at 8 | Decode threads. whisper.cpp's own default is `min(4, cores)`, which left most of the machine idle. |
| `MEETING_ASSISTANT_WHISPER_BEAM` | 5 | Beam width. Lower is faster and less accurate. |
| `MEETING_ASSISTANT_WHISPER_SUPPRESS_NST` | on | Set to `0` to stop suppressing non-speech tokens. Never benchmarked; it modifies the logits at every decode step. |

Change one at a time and compare the ratio across runs of the same audio.

On Windows the app is launched from the Start menu, so set these as **user
environment variables** (Settings -> System -> About -> Advanced system settings
-> Environment Variables) and relaunch, rather than exporting them in a shell.

## Diagnosing an empty system-audio track

`spikes/tap-probe` reports what cpal discards — the tap's `OSStatus`, its
negotiated format, and the aggregate device's real input channel count:

```bash
cd spikes/tap-probe && cargo run --release
```

`spikes/loopback-probe` answers whether capture survives a silent output
device, with the protocol automated:

```bash
cd spikes/loopback-probe
./run-protocol.sh "MacBook Air Speakers"
./run-in-bundle.sh --device "EarPods" --seconds 60   # from a signed .app
```

See `spikes/loopback-probe/RESULTS-macos.md` for what these measured, including
a post-mortem of a zero-frame result that turned out to be a testing error
rather than a bug.

## Ollama

Must be installed and running; the app starts it if it can find it. Looked up on
`PATH` first, then `/opt/homebrew/bin`, `/usr/local/bin`, and
`/Applications/Ollama.app/Contents/Resources/ollama`. It is reached over plain
HTTP on `127.0.0.1:11434` — the client has no TLS support compiled in, because
nothing here should ever leave the machine.

## The Python original

An earlier Python/CustomTkinter app was the blueprint for this one. It has been
**deleted from the working tree** — it was no longer a target, and this app has
since gained behaviour it never had: background processing with a pausable,
resumable queue, single-instance enforcement, Whisper model management.

**The `app.py:NNN` references throughout the code are deliberate and are staying.**
There are 51 of them, and each records a decision: what was ported exactly, and
what was changed on purpose. The notable divergence is that silence for a device
outage is measured from the last sample actually received to the first sample of
the new stream, rather than written at open time as `app.py:1557-1562` did — on
macOS the simpler version loses roughly 0.14 s per device transition, because
`stream.play()` returns long before a Core Audio tap starts delivering.

Those line numbers are **provenance, not links**: the file is not in this
repository, so they cannot be opened. They are kept because the reasoning they
carry is worth more than the broken reference costs.

What was given up in dropping parity is worth naming: the Python was a second,
independent implementation to check a suspicious transcript against. Nothing
replaces that, so a surprising result now has to be judged on its own.
