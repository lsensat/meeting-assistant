# Meeting Assistant

Record a meeting, get a transcript and a summary. Nothing leaves your computer.

It captures **your microphone and what your computer is playing at the same
time**, so you get both halves of a call — not just your own side. The audio is
transcribed locally with [whisper.cpp](https://github.com/ggerganov/whisper.cpp)
and summarised by a local [Ollama](https://ollama.com) model.

Windows and macOS, from one codebase. Free and open source.

---

## Why it exists

Most meeting tools upload your audio to somebody else's servers. That is a
problem when the meeting is about a client, a salary, a patient, or anything
under an NDA — and it is a decision you make once, silently, when you install
the thing.

This runs the whole pipeline on your own machine. There is no account, no API
key required, and no network call in the default configuration. You can
disconnect from the internet and it still works.

## What it does

- **Records two sources at once** — microphone and system audio — so remote
  participants are captured as well as you.
- **Transcribes offline** with Whisper, in English or Spanish, or detects the
  language automatically.
- **Summarises offline** with Ollama. Four styles: meeting minutes, executive
  summary, action items, or a short brief. You can also write your own prompt.
- **Keeps working while you start the next meeting.** Processing runs in a
  queue in the background, and you can pause the whole queue when you need your
  CPU back.
- **Survives a restart.** Close the app mid-transcription and it resumes where
  it stopped rather than starting over.
- **Interface in English and Spanish.**

Optionally, it can summarise through an OpenAI-compatible API instead of Ollama
— useful on a machine that cannot run a model locally. That one *does* send your
transcript to whoever you point it at, so it is off by default.

### What you get

Each meeting becomes one folder, named with the time and the title you give it:

```
13:15 Weekly sync/
  microphone.wav      your side
  system_audio.wav    everyone else
  transcript.txt      both, merged and in order
  summary.md          the summary
```

Plain files in a folder you choose. Nothing is locked in a database, and you can
delete a meeting by deleting its folder.

## What it is not

- **Not a speaker identifier.** It separates *your microphone* from *the
  computer's output*, which usually means you against everyone else. It cannot
  tell two remote speakers apart.
- **Not a bot that joins your calls.** It records what the machine plays, so it
  works with any application — Teams, Meet, Zoom, a phone on speaker — without
  integrating with any of them.
- **Not a real-time transcriber.** Transcription runs after the meeting ends.

## Installing

Download the latest build from
[Releases](https://github.com/lsensat/meeting-assistant/releases).

- **Windows** — `.msi` (or the `.exe` installer)
- **macOS** — `.dmg`, Apple Silicon only

### The builds are unsigned

There is no paid signing certificate on this project, so both systems will warn
you. This is the expected behaviour for an unsigned open-source app, not a sign
that something is wrong — but you should only click through it because you chose
to trust this repository.

- **Windows** — SmartScreen shows *"Windows protected your PC"*. Click **More
  info → Run anyway**.
- **macOS** — Gatekeeper says the app *"is damaged"* or cannot be opened. Open
  **System Settings → Privacy & Security**, find the message about Meeting
  Assistant, and choose **Open Anyway**.

### First run

The app walks you through it:

1. **Choose a Whisper model.** `base` or `small` is the sensible starting point;
   `tiny` is fast and rough, `large-v3` is slow and accurate. It downloads once.
2. **Install Ollama** if you want summaries, and pull a model.
3. **Grant permissions** — macOS asks for microphone access, and for screen
   recording, which is how the system asks about capturing audio output.

For summaries, use a normal **instruct** model, such as
`qwen2.5:7b-instruct-q4_K_M` or `llama3.1:8b`. Reasoning models spend their
output on hidden thinking and can return an empty summary.

## Requirements

- **Windows 10/11**, or **macOS on Apple Silicon**
- **~1 GB disk** for a Whisper model, more for larger ones
- **Ollama** for summaries — optional, and only for that step
- Transcription is CPU-heavy. Expect a meeting to take a few minutes to process.

## Privacy

Recording other people has rules that vary by country and by employer, and in
many places every participant has to be told. This app does not ask on your
behalf. **Tell people they are being recorded.**

## Building from source

See [meeting-assistant-rs/README.md](meeting-assistant-rs/README.md) for the
development setup, project layout, and platform notes.

## Licence

MIT, as declared in `Cargo.toml`. A `LICENSE` file still needs adding at the
repository root — without one, GitHub shows no licence and the terms are not
actually granted to anyone reading this.
