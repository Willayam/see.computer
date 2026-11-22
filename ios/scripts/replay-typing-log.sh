#!/bin/sh
# SIMULATOR_UDID can point to an already booted simulator.
set -eu
log=${1:?Usage: replay-typing-log.sh <log.jsonl>}
test -r "$log"
device=${SIMULATOR_UDID:-}
if [ -z "$device" ]; then
    pairs=$(xcrun simctl list -j runtimes | python3 -c '
import json, os, sys

requested = os.environ.get("SIMULATOR_RUNTIME")
runtimes = json.load(sys.stdin)["runtimes"]
for runtime in reversed(runtimes):
    if not runtime["isAvailable"] or not runtime["identifier"].startswith("com.apple.CoreSimulator.SimRuntime.iOS-"):
        continue
    if requested and requested != runtime["identifier"]:
        continue
    for phone in runtime["supportedDeviceTypes"]:
        if phone["productFamily"] == "iPhone":
            print(phone["identifier"], runtime["identifier"])
')
    # Try explicit runtime and device pairs. A listed pairing can still fail to create.
    while read -r device_type runtime; do
        [ -n "$device_type" ] || continue
        if device=$(xcrun simctl create "typing-log-$$" "$device_type" "$runtime"); then
            break
        fi
    done <<EOF
$pairs
EOF
    if [ -z "$device" ]; then
        echo "Could not create an iPhone simulator. Set SIMULATOR_UDID to a booted device or SIMULATOR_RUNTIME to an installed iOS runtime ID." >&2
        exit 1
    fi
    trap 'xcrun simctl delete "$device"' EXIT
    if ! xcrun simctl boot "$device" || ! xcrun simctl bootstatus "$device" -b >&2; then
        echo "Could not boot $device_type on $runtime. Set SIMULATOR_UDID to a booted device or try another SIMULATOR_RUNTIME." >&2
        exit 1
    fi
fi
"$(dirname "$0")/typing-sim.sh" "$device" --typing-log "$(cd "$(dirname "$log")" && pwd)/$(basename "$log")"
