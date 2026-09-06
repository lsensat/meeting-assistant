#!/bin/bash
# Run the loopback probe from inside a minimal, ad-hoc signed .app bundle.
#
# WHY THIS EXISTS
#
# Running the probe as a bare CLI binary always captures zero frames from a
# Core Audio process tap, with zero callbacks and zero stream errors. That is
# not an idle-gating problem and not a cpal bug — it is TCC.
#
# System-audio capture is gated by kTCCServiceAudioCapture, a category separate
# from the microphone. Two things break for a bare binary:
#
#   1. There is no bundle, so there is no Info.plist, so there is no
#      NSAudioCaptureUsageDescription for the system to prompt with.
#   2. When launched from a terminal, TCC attributes the request to the
#      *responsible process* — the terminal app (VS Code, Terminal.app) —
#      not to our binary. The terminal has no audio-capture grant, so the tap
#      is created successfully and then delivers silence forever.
#
# Neither failure raises an error. The only symptom is a WAV full of nothing,
# which is exactly the catastrophic-and-invisible outcome the spike exists to
# catch. So the probe has to run the way the real app will: inside a bundle,
# code-signed, with the usage-description keys present.
#
# USAGE
#   ./run-in-bundle.sh --device "MacBook Air Speakers" --seconds 60
#
# Output is redirected to probe-output.txt because a bundle launched with
# `open` has no terminal attached to inherit stdout from.

set -euo pipefail

cd "$(dirname "$0")"

APP="$PWD/LoopbackProbe.app"
OUT="$PWD/probe-output.txt"
BIN="$PWD/target/release/loopback-probe"

if [ ! -x "$BIN" ]; then
    echo "building probe first..."
    cargo build --release
fi

# Rebuild the bundle from scratch every time so a stale binary can never be
# what gets measured.
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/loopback-probe"

# The two usage-description keys are the whole point of the bundle. Without
# NSAudioCaptureUsageDescription macOS has no string to show in the prompt and
# will not grant kTCCServiceAudioCapture at all.
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>loopback-probe</string>
    <key>CFBundleIdentifier</key>
    <string>com.meetingassistant.loopbackprobe</string>
    <key>CFBundleName</key>
    <string>LoopbackProbe</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>14.4</string>
    <key>NSMicrophoneUsageDescription</key>
    <string>The Meeting Assistant loopback probe records the microphone to verify audio capture.</string>
    <key>NSAudioCaptureUsageDescription</key>
    <string>The Meeting Assistant loopback probe records the audio your Mac plays to verify system-audio capture.</string>
</dict>
</plist>
PLIST

# Ad-hoc signature ("-"). TCC keys its grant to the signing identity, so every
# rebuild produces a new identity and re-prompts. That is expected here and is
# the documented consequence of the ad-hoc-signing decision.
codesign --force --sign - --timestamp=none "$APP" 2>&1 | sed 's/^/  codesign: /'

echo "bundle : $APP"
echo "output : $OUT"
echo

rm -f "$OUT"
touch "$OUT"

# `open` detaches, so redirect both streams into the file and then follow it.
open -a "$APP" --stdout "$OUT" --stderr "$OUT" --args "$@"

echo "launched; following output (ctrl-C to stop following, probe keeps running)"
echo
sleep 2
cat "$OUT"
