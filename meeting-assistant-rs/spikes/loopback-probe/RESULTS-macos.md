# Phase M0 results — macOS loopback spike

Machine: MacBook Air, Apple Silicon (arm64), macOS 26.6.2 (build 25G83).
Toolchain: cargo/rustc 1.98.0. cpal pinned `=0.18.2`.
Date: 2026-09-02.

## Verdict

**PASS. cpal 0.18.2 carries the macOS system-audio path. Proceed with the plan
as written.** No `screencapturekit`, no BlackHole, no second capture model.

## R-M1 — is the tap idle-gated? **NO.**

This was the single highest-risk item. Run via `./run-protocol.sh "EarPods"`,
which drives the protocol without a human: 15 s audio → **30 s true silence** →
15 s audio.

```
t= 14s  frames/s=  44544  (101.0% of realtime)  total=615424
t= 15s  frames/s=  44032  ( 99.8% of realtime)  total=659456 <- SILENCE WINDOW
...  every second in between at 99.8–101.0%  ...
t= 44s  frames/s=  44544  (101.0% of realtime)  total=1944064 <- SILENCE WINDOW
t= 45s  frames/s=  44032  ( 99.8% of realtime)  total=1988096
```

Frames arrive at full rate throughout the silent window: the tap delivers
buffers of zeroes rather than stalling. This is the **opposite** of the WASAPI
event-driven loopback behaviour that motivated Windows risk R1, and it means
the system track cannot silently come out short.

Final tally for the full 60 s protocol run:

```
elapsed          : 60.26s
frames captured  : 2651648
frames expected  : 2657517
overall ratio    : 99.8%
callbacks        : 5179
stream errors    : 0

silence window (t=15..45s):
  frames         : 1328640
  expected       : 1323000
  ratio          : 100.4%

VERDICT: PASS — capture continued through silence.
```

## The false alarm — read this before debugging a zero-frame result

The first several runs captured **0 frames, 0 callbacks and 0 stream errors**,
which looks exactly like a catastrophic failure and burned most of the spike's
time. Two separate causes, neither of them a cpal or Core Audio bug:

1. **Testing error.** Plugging in the EarPods silently made them the default
   output device. `afplay` then played into the EarPods while the probe was
   tapping `MacBook Air Speakers` — a device with no audio flowing to it. Zero
   frames was the *correct* answer. **A tap only sees audio routed to the
   device it taps.** Always confirm which device is the current default output
   before interpreting a zero.
2. **A ~2.5-minute block inside `build_input_stream`** on the very first run
   only. Stack showed `audio_unit_from_device_id_uninitialized` →
   `AudioUnitSetProperty` → `mach_msg` waiting on `coreaudiod`. This was the
   one-time TCC permission round-trip. It has not recurred.

Consequence for the port: a zero-frame system track must be diagnosed with
`spikes/tap-probe` (below), not by staring at cpal.

## Permissions — TCC

A permission dialog appeared on first capture and was granted. Running the
probe from inside an ad-hoc signed `.app` bundle carrying
`NSAudioCaptureUsageDescription` (`./run-in-bundle.sh`) also works; that harness
is kept because the shipped app needs exactly that bundle shape, and because a
bare CLI binary has no Info.plist for the system to prompt with.

Note: nothing about the tap ever appeared in the unified log
(`log show --predicate 'process == "tccd"'` returned nothing), so **the absence
of log entries proves nothing** — do not use it as evidence either way.

## Device model — three corrections to the plan's assumptions

**1. R-M2 (combined-I/O device silently records the mic) did not reproduce, and
is unlikely on macOS.** Core Audio splits a USB headset into two *separate*
devices:

```
== loopback candidates (render endpoints, opened as input) ==
  [0] EarPods                    44100 Hz, 2 ch, F32
  [1] MacBook Air Speakers       48000 Hz, 2 ch, F32
== capture devices (microphones) ==
  [0] Luis's iPhone Microphone   48000 Hz, 1 ch, F32
  [1] EarPods Microphone         44100 Hz, 1 ch, F32
  [2] MacBook Air Microphone     48000 Hz, 1 ch, F32
```

`EarPods` (output-only) and `EarPods Microphone` (input-only) are distinct
endpoints, so `supports_input()` is false on the render endpoint and cpal takes
the loopback branch correctly. Windows' `pyaudiowpatch` behaviour of one device
with both directions does not carry over. **F1 is therefore demoted from
"required" to "contingency"** — still worth keeping for aggregate devices and
unusual interfaces, but it is not blocking Phase M2.

**2. Sample rates are per-device and are NOT all 48 kHz.** EarPods run at
**44100 Hz**, built-in speakers and mic at 48000 Hz. The two tracks will
routinely have *different* native rates on the same machine — more so than on
Windows. The `resample_mono` path is therefore load-bearing on macOS from day
one, not an edge case.

**3. An iPhone shows up as an input device** ("Luis's iPhone Microphone",
Continuity). It is a real, selectable capture device that can vanish at any
moment. Good hot-unplug test subject.

## Buffer size

Callback cadence measured at ~1023 callbacks / 12 s ≈ 512 frames per callback,
confirming the plan's note that macOS hands you 512-frame buffers rather than
1024. The re-packetiser in the writer thread is required, as specified.

## Still open

- **Idle gating on the built-in speakers specifically** was never cleanly
  measured (every silence-window run used the EarPods). Re-run
  `./run-protocol.sh "MacBook Air Speakers"` with the EarPods unplugged.
- **Hot-unplug** (yank the EarPods mid-capture): which detector fires first,
  and does ~10 s of silence land in the file.
- **R-M6** (`set_physical_format` changing the output device's rate): not yet
  checked against Audio MIDI Setup before/after a run.
- **Leaked aggregate after `kill -9`** (R-M5).

## Reproducing

```bash
source ~/.cargo/env
cd rust/spikes/loopback-probe
cargo run --release -- --list                  # what can be tapped
./run-protocol.sh "EarPods"                    # the R-M1 idle test, automated
./run-in-bundle.sh --device "EarPods" --seconds 60   # same, from a signed .app

cd ../tap-probe && cargo run --release         # raw Core Audio tap diagnostic
```

`tap-probe` is the tool to reach for whenever the system track is empty. It
reports what cpal throws away — the tap's `OSStatus`, its negotiated format,
and the aggregate device's actual input channel count:

```
default output device UID: AppleUSBAudioEngine:Apple, Inc.:EarPods:KXCYTQX0XP:1
AudioHardwareCreateProcessTap -> OSStatus 0 (ok)
tap format               : 44100 Hz, 2 ch, 32 bits, format id 0x6c70636d, flags 0x9
AudioHardwareCreateAggregateDevice -> OSStatus 0 (ok)
aggregate input streams  : 1 buffer(s), 2 channel(s)
```
