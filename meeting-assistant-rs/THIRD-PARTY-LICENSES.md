# Third-party components redistributed in the binary

Rust dependencies are fetched at build time and their licences travel with them
in `Cargo.lock`; `cargo audit` runs in CI. This file records the things that are
**copied into the shipped binary**, where the obligation is ours rather than the
build system's.

## Silero VAD weights

`src-tauri/assets/ggml-silero-v5.1.2.bin` (885,098 bytes) is embedded in the
executable by `src-tauri/src/vad.rs` and written beside the Whisper weights on
first use.

- **Model**: [silero-vad](https://github.com/snakers4/silero-vad), © Silero Team
  — **MIT**.
- **ggml conversion**: from [whisper.cpp](https://github.com/ggml-org/whisper.cpp),
  © Georgi Gerganov and contributors — **MIT**, redistributed via
  [`ggml-org/whisper-vad`](https://huggingface.co/ggml-org/whisper-vad).

Both licences require the copyright notice and permission notice to accompany
copies of the software, which is what this file is.

It is bundled rather than downloaded because the alternative was worse in three
specific ways: the file lives in a different Hugging Face repository from the
Whisper weights, it would have needed an entry in `whisper::MODELS` and would
then have appeared in the model picker as something to transcribe with, and
whisper.cpp's `models/README.md` lists no silero row — so the second-publisher
SHA-1 that every other model entry carries does not exist for it. Bundling
replaces that run-time check with a build-time one: `vad.rs` pins the SHA-256 and
a unit test hashes the embedded bytes.

## Whisper models

The GGML weights under `whisper::MODELS` are **not** redistributed. They are
downloaded on demand from
[`ggerganov/whisper.cpp`](https://huggingface.co/ggerganov/whisper.cpp) and
verified against two independently published digests before use. The models
themselves originate from OpenAI's Whisper (MIT).
