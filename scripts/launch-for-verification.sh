#!/bin/sh
# Launches the built .app with the verification seams on.
# Usage: scripts/launch-for-verification.sh [fixture.wav] [trail.tsv]
# The fake mic replaces the microphone; the trail records every state transition.
# History, the speed log and the ledger go to a scratch folder so a test take
# never lands in the user's own, and the speed backfill writes only there.
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
app="$root/src-tauri/target/debug/bundle/macos/see.computer.app"
wav="$(cd "$(dirname "${1:-$root/fixtures/en.wav}")" && pwd)/$(basename "${1:-$root/fixtures/en.wav}")"
trail="${2:-/tmp/see-computer-trail.tsv}"
scratch="${SEE_COMPUTER_SCRATCH:-$(mktemp -d /tmp/see-computer-verify.XXXXXX)}"
mkdir -p "$scratch/history" "$scratch/ledger"
pkill -f "see.computer.app/Contents/MacOS/see-computer" 2>/dev/null || true
sleep 0.5
: > "$trail"
open --env "SEE_COMPUTER_AUDIO_FILE=$wav" --env "SEE_COMPUTER_STATE_LOG=$trail" \
  --env "SEE_COMPUTER_HISTORY_DIR=$scratch/history" \
  --env "SEE_COMPUTER_SPEED_LOG=$scratch/speed.tsv" \
  --env "SEE_COMPUTER_LEDGER_DIR=$scratch/ledger" -a "$app"
echo "app: $app"
echo "fake mic: $wav"
echo "trail: $trail"
echo "scratch: $scratch"
