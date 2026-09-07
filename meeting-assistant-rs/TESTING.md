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
