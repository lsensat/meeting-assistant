#!/bin/bash
# Drive the full R-M1 idle-gating protocol without a human at the keyboard.
#
# The question this answers: does a Core Audio process tap keep delivering
# frames while the tapped output device is genuinely idle? If it does not, the
# system-audio track comes out shorter than the meeting and the two tracks lose
# alignment — a failure that is invisible in any short test where audio plays
# throughout, which is exactly why the protocol has a silence window in the
# middle.
#
#   0-15s   audio playing
#   15-45s  TRUE SILENCE (nothing plays)
#   45-60s  audio playing again
#
# PASS is the silence-window ratio staying at ~100%: frames keep arriving at
# the full rate even though the samples are all zeroes.
#
# Usage: ./run-protocol.sh "EarPods"

set -euo pipefail
cd "$(dirname "$0")"

DEVICE="${1:-EarPods}"
SOUND=/System/Library/Sounds/Submarine.aiff
OUT="$PWD/protocol-output.txt"

echo "device: $DEVICE"
echo "output: $OUT"
echo

# Tap must be running before the audio starts.
./target/release/loopback-probe --device "$DEVICE" --seconds 60 > "$OUT" 2>&1 &
PROBE=$!

# 0-15s: play. afplay blocks for the length of the clip, so loop it.
END=$((SECONDS + 15))
while [ $SECONDS -lt $END ]; do afplay "$SOUND"; done

# 15-45s: true silence. Nothing plays, nothing is even open.
sleep 30

# 45-60s: play again.
END=$((SECONDS + 15))
while [ $SECONDS -lt $END ]; do afplay "$SOUND"; done

wait $PROBE
cat "$OUT"
