#!/bin/sh
# After swiping in the installed keyboard, print its cold loads, gesture latency
# and process-lifetime high-water footprint. Optionally pass a devicectl ID.
set -eu
log="$(mktemp)"
trap 'rm -f "$log"' EXIT
"$(dirname "$0")/pull-diagnostics.sh" "$log" "${1:-}" >/dev/null
python3 - "$log" <<'PY'
import math
import sys

times, peaks, loads = [], [], []
for line in open(sys.argv[1]):
    fields = line.rstrip().split("\t")
    if len(fields) != 4 or fields[1] != "keyboard":
        continue
    values = dict(pair.split("=", 1) for pair in fields[3].split())
    if fields[2] == "swipe-model":
        loads.append(float(values["load_ms"]))
    if fields[2] == "swipe-neural":
        times.append(float(values["ms"]))
        peaks.append(float(values["peak_mib"]))
if not times:
    sys.exit("No neural swipes in the keyboard diagnostics.")
times.sort()
print("cold_load_ms", loads)
print("gestures", len(times), "p50_ms", times[math.ceil(len(times)*.5)-1],
      "p95_ms", times[math.ceil(len(times)*.95)-1], "peak_mib", max(peaks))
PY
