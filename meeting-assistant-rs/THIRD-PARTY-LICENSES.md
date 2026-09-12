# Third-party components redistributed in the binary

Rust dependencies are fetched at build time and their licences travel with them
in `Cargo.lock`; `cargo audit` runs in CI. This file records what is **copied
into the shipped binary**, where the obligation is ours rather than the build
system's.

## Silero VAD weights

`src-tauri/assets/ggml-silero-v5.1.2.bin` (885,098 bytes) is embedded in the
executable by `src-tauri/src/vad.rs` and written beside the Whisper weights on
first use.

Two upstreams, both MIT: the model itself, and the ggml conversion obtained via
[`ggml-org/whisper-vad`](https://huggingface.co/ggml-org/whisper-vad), which
declares MIT.

MIT requires the copyright notice **and the permission notice** to accompany
copies, so both are reproduced in full below rather than merely named.

### silero-vad — https://github.com/snakers4/silero-vad

```
MIT License

Copyright (c) 2020-present Silero Team

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### whisper.cpp — https://github.com/ggml-org/whisper.cpp

```
MIT License

Copyright (c) 2023-2026 The ggml authors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### Why bundled rather than downloaded

The alternative was worse in three specific ways: the file lives in a different
Hugging Face repository from the Whisper weights, it would have needed an entry
in `whisper::MODELS` and would then have appeared in the model picker as
something to transcribe with, and whisper.cpp's `models/README.md` lists no
silero row — so the second-publisher SHA-1 that every other model entry carries
does not exist for it. Bundling replaces that run-time check with a build-time
one: `vad.rs` pins the SHA-256 and a unit test hashes the embedded bytes.

The cost of that choice is this file. Downloading put the copy in the user's
hands directly; bundling means we distribute it, and MIT attaches the notice
requirement above to distribution.

## Whisper models

The GGML weights under `whisper::MODELS` are **not** redistributed. They are
downloaded on demand from
[`ggerganov/whisper.cpp`](https://huggingface.co/ggerganov/whisper.cpp) and
verified against two independently published digests before use. The models
originate from OpenAI's Whisper (MIT).
