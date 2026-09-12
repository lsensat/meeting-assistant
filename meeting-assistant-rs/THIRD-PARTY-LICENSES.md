# Third-party components redistributed in the binary

This file exists because of what is **compiled into the shipped executable**,
where the obligation is ours rather than the user's package manager's.

It is easy to assume nothing is redistributed, since the Whisper and VAD models
are both downloaded at run time and never travel with the app. But
`whisper-rs-sys` builds whisper.cpp from source and links it **statically**:

```
cargo:rustc-link-lib=static=whisper
cargo:rustc-link-lib=static=ggml
cargo:rustc-link-lib=static=ggml-base
cargo:rustc-link-lib=static=ggml-cpu
cargo:rustc-link-lib=static=ggml-blas
```

So every `.dmg` and `.exe` contains that MIT-licensed C++, and so does every
Rust crate in `Cargo.lock` — Tauri, cpal, serde, reqwest and the rest, nearly
all MIT or Apache-2.0. This predates any single feature; it has been true since
the first build.

MIT requires the copyright notice **and the permission notice** to accompany
copies, so the text is reproduced rather than named.

## whisper.cpp and ggml — https://github.com/ggml-org/whisper.cpp

Compiled from source by `whisper-rs-sys` and linked statically into the binary.

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

## Rust dependencies

Every crate in `Cargo.lock` is statically linked too. Their licences are not
transcribed here by hand — that list changes with every dependency update and a
stale copy is worse than none. Generate it:

```bash
cargo install cargo-about   # once
cargo about generate about.hbs > THIRD-PARTY-RUST.html
```

**Not yet wired into the release**, and it should be before the app is handed to
anyone outside this repository.

## What is *not* redistributed

- **Whisper weights** — downloaded on demand from
  [`ggerganov/whisper.cpp`](https://huggingface.co/ggerganov/whisper.cpp) and
  verified against two independently published digests.
- **Silero VAD weights** — downloaded once at first launch from
  [`ggml-org/whisper-vad`](https://huggingface.co/ggml-org/whisper-vad) and
  verified against a pinned SHA-256.

Both were briefly considered for bundling. Downloading puts the copy in the
user's hands directly, so no notice obligation attaches to them — which is the
whole reason the VAD model is fetched rather than embedded, at 885 KB where the
size argument alone would not have decided it.

## Reaching the user

A notice in the repository is not a notice delivered with the binary. This file
still needs to travel with the app — as a bundled resource, or an
"Acknowledgements" section in Settings — before the installers are distributed
outside this repository.
