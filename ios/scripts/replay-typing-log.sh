#!/bin/sh
# SIMULATOR_UDID can point to an already booted simulator.
set -eu
log=${1:?Usage: replay-typing-log.sh <log.jsonl>}
test -r "$log"
device=${SIMULATOR_UDID:-}
if [ -z "$device" ]; then
    device_type=$(xcrun simctl list -j runtimes | python3 -c '
import json, sys
for runtime in reversed(json.load(sys.stdin)["runtimes"]):
    phones = [d for d in runtime["supportedDeviceTypes"] if d["productFamily"] == "iPhone"]
    if runtime["isAvailable"] and phones:
        print(phones[0]["identifier"])
        break
else:
    sys.exit("Install an iPhone simulator runtime or set SIMULATOR_UDID")
')
    device=$(xcrun simctl create "typing-log-$$" "$device_type")
    trap 'xcrun simctl delete "$device"' EXIT
    xcrun simctl boot "$device"
    xcrun simctl bootstatus "$device" -b >&2
fi
"$(dirname "$0")/typing-sim.sh" "$device" --typing-log "$(cd "$(dirname "$log")" && pwd)/$(basename "$log")"
