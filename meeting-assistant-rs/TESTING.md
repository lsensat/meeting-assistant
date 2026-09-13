# Testing

Most of this project is covered by `cargo test`. This file is about the part
that is not, and why.

```bash
cargo test                                    # 186 tests, always green
cargo clippy --all-targets -- -D warnings
```

> Read clippy's **output**, not its exit code. A piped `grep` returns the status
> of the last command in the pipeline, which has hidden a failing run here once.

## Integration tests (ignored by default)

Two tests need a Whisper model, real speech and minutes, so they do not run in
CI. They cover the pause-and-resume path, which is where the interesting bugs
have actually been.

Generate the fixture once — 40 numbered sentences, about three minutes. The
numbering is what makes the assertions mechanical rather than a judgement about
whether two transcripts look similar:

```bash
python3 -c 'print(" ".join(f"Sentence number {i}. The deployment finished without errors." for i in range(1,41)))' > /tmp/script.txt
say -v Samantha -r 180 -f /tmp/script.txt -o /tmp/speech.aiff     # macOS
afconvert -f WAVE -d LEI16@48000 -c 1 /tmp/speech.aiff /tmp/speech.wav
```

```bash
# The seam inside one transcription: no speech lost or repeated across a pause.
MA_TEST_AUDIO=/tmp/speech.wav MA_TEST_MODEL=base \
  cargo test --release --test resume -- --ignored --nocapture

# The layer above: a paused meeting survives the app closing and finishes later.
# Needs Ollama running with the named model.
MA_TEST_AUDIO=/tmp/speech.wav MA_TEST_MODEL=base MA_TEST_OLLAMA=gemma4:e4b \
  cargo test --release --test queue_lifecycle -- --ignored --nocapture
```

**These two have already earned their place.** The first found a soundness bug
in whisper-rs that made transcription fail nondeterministically with error -6,
and a seek offset applied twice that put a resumed segment past the end of its
own recording. Both were invisible to `cargo test` and to clippy.

---

## The system mute, and the Windows half of it

The mute is the one thing this app changes **outside itself**, so its tests run
against real hardware rather than a fake. They set the device, read the state
back from the OS, and restore whatever they found:

```bash
# macOS: use the exact name from System Settings → Sound → Input.
MUTE_TEST_DEVICE="MacBook Air Microphone" \
  cargo test -p meeting-assistant --lib audio::system_mute -- --ignored --nocapture
```

Three of them, and the third is the point: it spawns a child process, has it
take the mute, and kills it with `abort` so no destructor runs — the crash the
`Drop` guard cannot cover. It then asserts the device is still muted, that the
flag on disk names it, and that `restore_after_crash` puts it back. The expected
output ends:

```
crashed  child died with signal: 6 (SIGABRT)
stranded the OS agrees, and the flag names the device
restored the OS agrees
```

### Does the mute actually silence anything? (measured, macOS)

`set` returning `Ok` and `is_muted` agreeing prove only that Core Audio recorded
the request. They do **not** prove a single sample went quiet — a real call
appeared unaffected while the property read back as muted, and that gap is what
this measurement closes.

The listener is a separate process using plain HAL capture (cpal), with a tone
playing through the speakers so the microphone always has signal. Measured on a
MacBook Air's built-in microphone:

| lever | peak heard by another process |
|---|---|
| nothing (baseline) | `-30.5 dBFS` |
| `kAudioDevicePropertyMute` = 1 | **`-120 dBFS` — digital silence** |
| `kAudioDevicePropertyVolumeScalar` = 0 | `-43.4 dBFS` — ~13 dB down, still audible |

Two things follow. **Volume-0 is not a mute** — it attenuates, so it is not a
usable fallback, which settles a question raised early in the design. And the
mute works **mid-stream**: flipped while the listener was already capturing, the
samples went to zero within ~300 ms and came back immediately on release, which
is the real scenario (a call app already holds the microphone when the user
mutes).

What this does not cover: conferencing apps capture through Voice-Processing IO,
which wraps the device in a private aggregate. Whether device mute reaches that
path is untested, and it is the open question behind any report that a call was
unaffected. The Mac's own input meter and the orange dot are **not** evidence
either way — the app keeps the device open while receiving zeros.

The device's own properties are worth dumping before drawing conclusions: on
this machine `mute` and `volume` are settable on element 0 only, and channels
1-3 refuse both, so element 0 is the only target that exists.

### Type-checking the Windows path from a Mac

`cargo check --target x86_64-pc-windows-msvc` on the whole app fails, because
whisper.cpp needs a C++ cross-compiler. But `audio::system_mute`'s Windows module
is pure Rust over the `windows` crate, and `cargo check` links nothing, so it can
be compiled in an isolated crate with the same dependency:

```bash
rustup target add x86_64-pc-windows-msvc
# A scratch crate: the module's source, `MuteError`, and the `windows`
# dependency copied verbatim from src-tauri/Cargo.toml.
cargo check --target x86_64-pc-windows-msvc
```

**This found two errors that would each have failed the first Windows build**: a
missing set of `windows` features (`IPropertyStore::GetValue` is gated behind
`Win32_System_Com_StructuredStorage` and `Win32_System_Variant`, which is not
guessable from the call), and `PROPERTYKEY` imported from
`UI::Shell::PropertiesSystem` where the rest of that API lives, rather than from
`Win32::Foundation` where it actually is. Worth doing before any release that
carries new Windows code, because the alternative is finding out a build later.

What it does **not** prove is that the calls work — only that they exist and
type-check. The device mute still needs a Windows machine.

---

## What still needs a person, and why

Everything below is either driven by a GUI or needs hardware this machine does
not have. Where a check can be made mechanical it has been; what is left is
genuinely manual.

### 1. A meeting through the app, end to end

The queue's logic is covered by the tests above. What is not covered is the
Tauri command layer and the rendering — nothing has recorded a real meeting
through the UI.

1. Launch, start a meeting, talk for a minute, stop, give it a title.
2. **Start a second meeting within a few seconds.** It must start. Before the
   queue this was refused outright, and it is the reason the feature exists.
3. While the second records, the first should appear as a card: start time,
   title, `Transcribing 2/3`, a bar that moves.
4. Stop the second. Both cards should be listed, one working and one waiting.
5. Let them finish. Cards disappear as each completes; Transcript and Summary
   become clickable and open the **most recent** meeting.

**Watch for:** the toolbar being pushed off the bottom as cards appear. The
window grows to fit and the list scrolls past three, but that path has broken
twice before.

### 2. Pause and resume, through the app

1. With a meeting transcribing, press pause in the queue header. The bar should
   stop and the card should read `Paused`.
2. **Quit the app entirely** — not just close the window, which only hides it.
   Use the tray's Quit.
3. Relaunch. The card must come back, still paused, at roughly the same
   percentage.
4. Press play. It must finish, and the transcript must contain the whole
   meeting rather than only the part after the pause.

Step 4 is the one that matters. `queue_lifecycle` proves the mechanism; this
proves it is wired to the button.

### 3. Discard

1. Discard a **queued** meeting: confirm, then check its folder is gone from the
   output directory.
2. Discard the **running** one. It must stop, its folder must go, and the next
   meeting must carry on. Nothing should be left half-deleted.

### 4. Single instance

The routes that matter are the ones the OS does not cover:

- Close the window so the app is tray-only, then launch again from the Dock or
  Start menu. The existing window must come back — no second process.
- On macOS also `open -n /Applications/Meeting\ Assistant.app` and run the
  binary inside the bundle directly. LaunchServices does not stop either.

### 5. Windows: processing while recording

**This is the honest test of whether background processing is safe**, and it can
only be done on the Windows machine, because that is where Whisper *and* Ollama
both run on the CPU (`library=cpu` in Ollama's log). On Apple Silicon, Metal
does the transcription and the contention barely exists.

1. Queue a large-v3 transcription.
2. Start recording while it runs, for several minutes.
3. Afterwards, check the recorder's own numbers: `TrackSummary.gap_frames` is
   how much silence it had to pad because samples did not arrive, and the skew
   between the two tracks should stay under 100 ms.

If those are clean, nothing more is needed. If they are not, capping Whisper's
thread count while recording is the first lever — `whisper.rs` does not set
`n_threads` at all today — and deferring the queue during a recording is the
last resort, not the first.

**The asymmetry that decides it:** audio is the only artifact that cannot be
recreated. A transcript can always be run again.

### 6. Windows: the loopback protocol

Unchanged and still outstanding — see the migration plan. 15 s of audio, then
**30 s of true silence**, then 15 s. If cpal's event-driven WASAPI loopback does
not fire while the render endpoint is idle, `system_audio.wav` comes out far
shorter than the meeting **and looks perfectly fine in any short test where
audio plays throughout**.
