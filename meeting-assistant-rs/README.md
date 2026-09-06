# Meeting Assistant — Rust + Tauri port

Records a meeting from two audio sources at once — your microphone and the
audio your computer is playing — transcribes both with whisper.cpp and
summarizes the merged transcript with a local Ollama model. Everything runs
offline.

This is the Rust rewrite of the Python/CustomTkinter app in the repository
root. It targets **Windows and macOS from one codebase**.

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

The first build compiles whisper.cpp, which takes a few minutes. Metal is
enabled automatically on Apple Silicon — do **not** pass `--features metal`, and
note that `--no-default-features` does not turn it off.

### CLI drivers

Two binaries exercise the layers without the UI, which is how the audio and
pipeline phases were developed and how they should be debugged:

```bash
cargo run --release --bin rec -- --list
cargo run --release --bin rec -- --seconds 60 --out /tmp/meeting
cargo run --release --bin process -- --folder /tmp/meeting
```

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
If a system-audio track ever comes back empty, this is the first thing to check.

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

## Parity with the Python app

The port holds strict feature parity. Where behaviour deliberately differs, the
reason is at the call site — the notable one is that silence for a device outage
is measured from the last sample actually received to the first sample of the
new stream, rather than written at open time as `app.py:1557-1562` does. On
macOS the Python's simpler version loses roughly 0.14 s per device transition,
because `stream.play()` returns long before a Core Audio tap starts delivering.

Known issues carried over deliberately, rather than fixed during the port, are
in the deferred-fixes register in the migration plan.
