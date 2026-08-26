#!/bin/sh
# Measures the first recording after the model weights were evicted.
# Fills RAM with incompressible data so macOS drops the mmapped weights (the
# compressor absorbs zero-filled hogs without evicting anything), settles,
# drives one video hold through the real trigger, and prints the trail deltas.
# Focus a scratch document first; the clip link pastes at the cursor.
# Usage: scripts/bench-cold-recording.sh [trail.tsv] [hold_ms]
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
pid="$(pgrep -f 'see.computer.app/Contents/MacOS/see-computer' | head -1)"
trail="${1:-/tmp/see-computer-trail.tsv}"
hold="${2:-1500}"

residency() {
    vmmap "$pid" | grep "encoder-model.int8.onnx.data" \
        | awk '{print $4, $5}' | sed 's/\[//' | sort -rh | head -4
}

echo "== weights resident before eviction =="
residency

python3 - <<'EOF'
import os, time
gb = int(os.popen("sysctl -n hw.memsize").read()) * 2 // (3 << 30)
seed = os.urandom(1 << 30)
bufs = [seed] + [bytearray(seed) for _ in range(gb - 1)]
time.sleep(3)
EOF
sleep 30

echo "== weights resident after eviction =="
residency

lines_before="$(wc -l < "$trail")"
swift "$root/scripts/press-mod.swift" opt-shift "$hold"
sleep 8

echo "== trail deltas =="
tail -n "+$((lines_before + 1))" "$trail" \
    | awk -F'\t' 'NR==1{prev=$1} {printf "%s\t+%d ms\n", $3, $1-prev; prev=$1}'
