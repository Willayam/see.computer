#!/bin/sh
# Launches the built .app with the verification seams on.
# Usage: scripts/launch-for-verification.sh [fixture.wav] [trail.tsv]
# The fake mic replaces the microphone; the trail records every state transition.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
app="$root/src-tauri/target/debug/bundle/macos/see.computer.app"
wav="${1:-$root/fixtures/en.wav}"
trail="${2:-/tmp/see-computer-trail.tsv}"
pkill -f "see.computer.app/Contents/MacOS/see-computer" 2>/dev/null || true
sleep 0.5
: > "$trail"
open --env "SEE_COMPUTER_AUDIO_FILE=$wav" --env "SEE_COMPUTER_STATE_LOG=$trail" -a "$app"
echo "app: $app"
echo "fake mic: $wav"
echo "trail: $trail"
